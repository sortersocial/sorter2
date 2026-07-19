//! OAuth linking, session cookies, and vote actor resolution.
//!
//! Canonical identity is a UUID. OAuth providers only *link* to that UUID
//! (first link creates the principal; later links attach while logged in).
//! Which providers are linked is private to the account owner.

pub mod config;
pub mod identity;
pub mod oauth;
pub mod session;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};
use reqwest::Client;
use serde::Deserialize;

use crate::{
    events::Event,
    fetch::now_ms,
    form_template::template_json_compact,
    html::layout,
    state::AppState,
    storage_schema::{
        linked_providers_for_uuid, oauth_link_owner, pseudonym_owner, Store, StoreFields,
    },
    ui_action::UI_RPC_FIELD,
};

pub use session::{nav_pseudonym, resolve_vote_actor, session_id_from_jar, VoteActor};

pub fn base_url_from_env(port: u16) -> String {
    std::env::var("SORTER2_BASE_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{port}"))
}

fn new_actor_uuid() -> String {
    let mut bytes = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut bytes);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]) | 0x4000,
        u16::from_be_bytes([bytes[8], bytes[9]]) | 0x8000,
        u128::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15], 0, 0, 0, 0,
            0, 0, 0, 0,
        ]) & 0x0000_FFFF_FFFF_FFFF
    )
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub return_to: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OAuthStartQuery {
    #[serde(default)]
    pub return_to: Option<String>,
    #[serde(default)]
    pub mock_user: Option<String>,
}

fn return_from_query_or_jar(jar: &CookieJar, query: Option<&str>) -> String {
    if let Some(raw) = query {
        return config::sanitize_return_to(raw);
    }
    jar.get(config::AUTH_RETURN_COOKIE)
        .map(|c| config::sanitize_return_to(c.value()))
        .unwrap_or_else(|| "/".to_string())
}

/// Available OAuth link targets: `(provider_key, label, start_href)`.
fn oauth_providers(base_url: &str, return_to: &str) -> Vec<(&'static str, &'static str, String)> {
    let mut out = Vec::new();
    let enc = urlencoding::encode(return_to);
    if oauth::GitHubConfig::from_env(base_url).is_some() {
        out.push((
            "github",
            oauth::provider_label("github"),
            format!("/auth/github?return_to={enc}"),
        ));
    }
    if oauth::RedditConfig::from_env(base_url).is_some() {
        out.push((
            "reddit",
            oauth::provider_label("reddit"),
            format!("/auth/reddit?return_to={enc}"),
        ));
    }
    out
}

fn alias_list(db: &durable::Db, uuid: &str) -> Vec<String> {
    Store::root()
        .user_pseudonyms()
        .key(&uuid.to_string())
        .iter(db)
        .unwrap_or_default()
}

fn alias_claim_forms(return_to: &str, submit_label: &str) -> Result<Markup, StatusCode> {
    let check_rpc = template_json_compact(&serde_json::json!({
        "action": "check_pseudonym",
        "pseudonym": {"$form": "pseudonym"},
    }))
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let claim_rpc = template_json_compact(&serde_json::json!({
        "action": "claim_pseudonym",
        "pseudonym": {"$form": "pseudonym"},
        "return_to": return_to,
    }))
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(html! {
        div class="alias-claim" {
            form id="alias-check-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(check_rpc);
                label for="alias-input" { "alias" }
                input type="text" id="alias-input" name="pseudonym" autocomplete="off"
                    data-testid="alias-input" maxlength="64" placeholder="letters, numbers, _ -";
                p id="alias-status" class="muted" data-testid="alias-status" { "type to check availability" }
            }
            form id="alias-claim-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(claim_rpc);
                input type="hidden" name="pseudonym" id="alias-claim-field" value="";
                button type="submit" class="btn-primary" data-testid="alias-claim" { (submit_label) }
            }
        }
    })
}

fn login_error_message(code: Option<&str>) -> Option<&'static str> {
    match code {
        Some("oauth_taken") => {
            Some("that OAuth account is already linked to a different sorter2 account")
        }
        Some("oauth_failed") => Some("OAuth failed — try again"),
        _ => None,
    }
}

fn signed_out_body(
    providers: &[(&str, &str, String)],
    error: Option<&str>,
) -> Markup {
    html! {
        main class="panel login-page" {
            section class="login-section" {
                h1 { "sign in" }
                p class="muted" {
                    "link an OAuth account to create your identity, then claim an alias to vote"
                }
                @if let Some(msg) = login_error_message(error) {
                    p class="alias-bad" data-testid="login-error" { (msg) }
                }
                @if providers.is_empty() {
                    p class="muted" {
                        "OAuth is not configured. Set GitHub and/or Reddit client credentials."
                    }
                } @else {
                    ul class="oauth-provider-list" {
                        @for (key, label, href) in providers {
                            li {
                                a href=(href) class="btn-primary oauth-provider"
                                    data-testid=(format!("oauth-{key}")) {
                                    (format!("Link {label}"))
                                }
                            }
                        }
                    }
                }
            }
            p class="login-back" { a href="/" { "← back" } }
        }
    }
}

fn account_body(
    actor: &session::SessionActor,
    aliases: &[String],
    // Provider keys already linked to this UUID (private).
    linked: &[String],
    // Providers available to link: not yet attached.
    unlinkable: &[(&str, &str, String)],
    claim_forms: Markup,
) -> Markup {
    let current = actor.pseudonym.trim();
    html! {
        main class="panel login-page account-page" {
            section class="login-section" {
                h1 { "account" }
                @if current.is_empty() {
                    p class="muted" { "finish setup by choosing an alias below" }
                } @else {
                    p class="account-current" {
                        "voting as "
                        strong data-testid="account-current" { (current) }
                    }
                }
                p class="muted small" data-testid="account-weight" {
                    "trust weight " (format!("{:.1}", actor.trust_weight))
                    " · rises when you link more OAuth providers"
                }
            }

            section class="login-section" {
                h2 { "aliases" }
                @if aliases.is_empty() {
                    p class="muted" data-testid="alias-list-empty" { "none yet — claim one below" }
                } @else {
                    ul id="alias-list" class="alias-list" data-testid="alias-list" {
                        @for alias in aliases {
                            @let is_current = alias == current;
                            li class=(if is_current { "alias-item alias-current" } else { "alias-item" }) {
                                span class="alias-name" { (alias) }
                                @if is_current {
                                    span class="alias-badge" data-testid="alias-current-badge" { "current" }
                                } @else {
                                    form class="alias-switch" method="post" action="/auth/switch"
                                        data-navigate="full" {
                                        input type="hidden" name="pseudonym" value=(alias);
                                        button type="submit" class="btn-secondary"
                                            data-testid=(format!("alias-switch-{alias}")) {
                                            "use"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            section class="login-section" {
                h2 { "add alias" }
                p class="muted small" { "each alias is unique across sorter2" }
                (claim_forms)
            }

            section class="login-section" {
                h2 { "linked sign-in" }
                p class="muted small" {
                    "private to you — linking more providers raises trust weight without publishing which accounts you use"
                }
                @if linked.is_empty() {
                    p class="muted" data-testid="linked-providers-empty" { "none yet" }
                } @else {
                    ul class="linked-provider-list" data-testid="linked-providers" {
                        @for key in linked {
                            li data-testid=(format!("linked-{key}")) {
                                (oauth::provider_label(key))
                            }
                        }
                    }
                }
                @if !unlinkable.is_empty() {
                    ul class="oauth-provider-list" {
                        @for (key, label, href) in unlinkable {
                            li {
                                a href=(href) class="btn-secondary oauth-provider"
                                    data-testid=(format!("oauth-link-{key}")) {
                                    (format!("Link {label}"))
                                }
                            }
                        }
                    }
                }
            }

            section class="login-section login-actions" {
                form method="post" action="/auth/logout" data-navigate="full" {
                    button type="submit" class="btn-secondary" data-testid="account-logout" { "log out" }
                }
            }

            p class="login-back" { a href="/" { "← back" } }
        }
    }
}

fn login_body(
    session: Option<&session::SessionActor>,
    aliases: &[String],
    linked: &[String],
    providers: &[(&str, &str, String)],
    claim_forms: Option<Markup>,
    error: Option<&str>,
) -> Markup {
    match (session, claim_forms) {
        (Some(actor), Some(forms)) => {
            let unlinkable: Vec<_> = providers
                .iter()
                .filter(|(key, _, _)| !linked.iter().any(|p| p == key))
                .cloned()
                .collect();
            account_body(actor, aliases, linked, &unlinkable, forms)
        }
        _ => signed_out_body(providers, error),
    }
}

pub async fn login_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<LoginQuery>,
) -> Result<Response, StatusCode> {
    let return_to = return_from_query_or_jar(&jar, query.return_to.as_deref());
    let jar = jar.add(session::auth_return_cookie_value(&return_to));

    let db = state.projection_store.db();
    let session = session::session_id_from_jar(&jar)
        .as_deref()
        .and_then(|id| session::load_session_actor(db, id));
    let aliases = session
        .as_ref()
        .map(|s| alias_list(db, &s.uuid))
        .unwrap_or_default();
    let linked = session
        .as_ref()
        .map(|s| linked_providers_for_uuid(db, &s.uuid).unwrap_or_default())
        .unwrap_or_default();
    let providers = oauth_providers(&base_url_from_env(state.cfg.port), &return_to);

    let claim_forms = if session.is_some() {
        Some(alias_claim_forms("/login", "claim alias")?)
    } else {
        None
    };

    let nsfw_ok = crate::nsfw::nsfw_allowed(&jar);
    let markup = layout(
        if session.is_some() {
            "account · sorter2"
        } else {
            "login · sorter2"
        },
        login_body(
            session.as_ref(),
            &aliases,
            &linked,
            &providers,
            claim_forms,
            query.error.as_deref(),
        ),
        state.views.get_views("/login"),
        session
            .as_ref()
            .filter(|s| !s.pseudonym.trim().is_empty())
            .map(|s| s.pseudonym.as_str()),
        nsfw_ok,
        "/login",
    );
    Ok((jar, Html(markup.into_string())).into_response())
}

pub async fn alias_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<LoginQuery>,
) -> Result<Response, StatusCode> {
    let return_to = return_from_query_or_jar(&jar, query.return_to.as_deref());
    let session_id = session::session_id_from_jar(&jar).ok_or(StatusCode::UNAUTHORIZED)?;
    let db = state.projection_store.db();
    let session = session::load_valid_session(db, &session_id).ok_or(StatusCode::UNAUTHORIZED)?;
    if session::session_has_pseudonym(&session) {
        return Ok(Redirect::to("/login").into_response());
    }

    let claim_forms = alias_claim_forms(&return_to, "continue")?;
    let body = html! {
        main class="panel alias-page" {
            h1 { "choose alias" }
            p class="muted" { "pick a unique display name for your votes" }
            (claim_forms)
            p class="login-back" { a href="/login" { "← back to login" } }
        }
    };

    let nsfw_ok = crate::nsfw::nsfw_allowed(&jar);
    Ok(Html(
        layout(
            "choose alias · sorter2",
            body,
            state.views.get_views("/login/alias"),
            None,
            nsfw_ok,
            "/login/alias",
        )
        .into_string(),
    )
    .into_response())
}

pub async fn github_start(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<OAuthStartQuery>,
) -> Result<Response, StatusCode> {
    let cfg = oauth::GitHubConfig::from_env(&base_url_from_env(state.cfg.port))
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let return_to = return_from_query_or_jar(&jar, query.return_to.as_deref());
    let state_token = session::new_oauth_state();
    let mock_user = if config::mock_oauth_allowed() {
        query.mock_user.as_deref()
    } else {
        None
    };
    let url = oauth::github_authorize_url(&cfg, &state_token, mock_user);
    let jar = jar
        .add(session::oauth_state_cookie_value(&state_token))
        .add(session::auth_return_cookie_value(&return_to));
    Ok((jar, Redirect::temporary(&url)).into_response())
}

pub async fn reddit_start(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<OAuthStartQuery>,
) -> Result<Response, StatusCode> {
    let cfg = oauth::RedditConfig::from_env(&base_url_from_env(state.cfg.port))
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let return_to = return_from_query_or_jar(&jar, query.return_to.as_deref());
    let state_token = session::new_oauth_state();
    let mock_user = if config::mock_oauth_allowed() {
        query.mock_user.as_deref()
    } else {
        None
    };
    let url = oauth::reddit_authorize_url(&cfg, &state_token, mock_user);
    let jar = jar
        .add(session::oauth_state_cookie_value(&state_token))
        .add(session::auth_return_cookie_value(&return_to));
    Ok((jar, Redirect::temporary(&url)).into_response())
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: String,
    pub state: String,
}

/// Link `provider:provider_id` to a UUID.
///
/// - Logged in + new provider → attach to session UUID
/// - Logged in + already ours → no-op
/// - Logged in + owned by someone else → conflict
/// - Logged out + known link → resume that UUID
/// - Logged out + unknown → create principal + first link
async fn finish_oauth_login(
    state: &AppState,
    jar: CookieJar,
    provider: &str,
    provider_id: String,
) -> Result<(CookieJar, String), StatusCode> {
    let db = state.projection_store.db();
    let return_to = return_from_query_or_jar(&jar, None);

    let existing_owner = oauth_link_owner(db, provider, &provider_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let session_uuid = session::session_id_from_jar(&jar)
        .as_deref()
        .and_then(|id| session::load_valid_session(db, id))
        .map(|s| s.uuid);
    let linking_while_logged_in = session_uuid.is_some();

    let uuid = match (session_uuid, existing_owner) {
        (Some(session_uuid), Some(owner)) if owner == session_uuid => session_uuid,
        (Some(_), Some(_)) => {
            return Ok((
                jar.add(session::clear_oauth_state_cookie()),
                "/login?error=oauth_taken".into(),
            ));
        }
        (Some(session_uuid), None) => {
            let ts = now_ms();
            state
                .append_identity_events(vec![Event::OauthLinked {
                    uuid: session_uuid.clone(),
                    provider: provider.to_string(),
                    provider_id,
                    ts,
                }])
                .await
                .map_err(|e| {
                    tracing::warn!(err = %e, "oauth link append failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            session_uuid
        }
        (None, Some(owner)) => owner,
        (None, None) => {
            let uuid = new_actor_uuid();
            let ts = now_ms();
            state
                .append_identity_events(vec![
                    Event::PrincipalCreated {
                        uuid: uuid.clone(),
                        ts,
                    },
                    Event::OauthLinked {
                        uuid: uuid.clone(),
                        provider: provider.to_string(),
                        provider_id,
                        ts,
                    },
                ])
                .await
                .map_err(|e| {
                    tracing::warn!(err = %e, "identity event append failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            uuid
        }
    };

    let aliases = alias_list(db, &uuid);
    let pseudonym = aliases.last().cloned().unwrap_or_default();
    let (session_id, _) = session::create_session(db, &uuid, &pseudonym).map_err(|e| {
        tracing::warn!(err = %e, "session create failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let jar = jar
        .add(session::session_cookie_value(&session_id))
        .add(session::clear_oauth_state_cookie());

    let dest = if pseudonym.is_empty() {
        format!(
            "/login/alias?return_to={}",
            urlencoding::encode(&return_to)
        )
    } else if linking_while_logged_in {
        // Additional link while already in an account → stay on account page.
        "/login".to_string()
    } else {
        return_to
    };

    Ok((jar, dest))
}

pub async fn github_callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<OAuthCallbackQuery>,
) -> Result<Response, StatusCode> {
    let cfg = oauth::GitHubConfig::from_env(&base_url_from_env(state.cfg.port))
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let expected_state = session::oauth_state_from_jar(&jar).ok_or(StatusCode::BAD_REQUEST)?;
    if expected_state != query.state {
        return Err(StatusCode::BAD_REQUEST);
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let token = oauth::github_exchange_code(&client, &cfg, &query.code)
        .await
        .map_err(|e| {
            tracing::warn!(err = %e, "github oauth token exchange failed");
            StatusCode::BAD_GATEWAY
        })?;
    let user = oauth::github_fetch_user(&client, &cfg.api_base, &token)
        .await
        .map_err(|e| {
            tracing::warn!(err = %e, "github user fetch failed");
            StatusCode::BAD_GATEWAY
        })?;

    let (jar, dest) =
        finish_oauth_login(&state, jar, "github", oauth::github_provider_id(&user)).await?;
    Ok((jar, Redirect::to(&dest)).into_response())
}

pub async fn reddit_callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<OAuthCallbackQuery>,
) -> Result<Response, StatusCode> {
    let cfg = oauth::RedditConfig::from_env(&base_url_from_env(state.cfg.port))
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let expected_state = session::oauth_state_from_jar(&jar).ok_or(StatusCode::BAD_REQUEST)?;
    if expected_state != query.state {
        return Err(StatusCode::BAD_REQUEST);
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let token = oauth::reddit_exchange_code(&client, &cfg, &query.code)
        .await
        .map_err(|e| {
            tracing::warn!(err = %e, "reddit oauth token exchange failed");
            StatusCode::BAD_GATEWAY
        })?;
    let user = oauth::reddit_fetch_user(&client, &cfg, &token)
        .await
        .map_err(|e| {
            tracing::warn!(err = %e, "reddit user fetch failed");
            StatusCode::BAD_GATEWAY
        })?;

    let (jar, dest) =
        finish_oauth_login(&state, jar, "reddit", oauth::reddit_provider_id(&user)).await?;
    Ok((jar, Redirect::to(&dest)).into_response())
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(session_id) = session::session_id_from_jar(&jar) {
        let _ = session::destroy_session(state.projection_store.db(), &session_id);
    }
    let jar = jar
        .add(session::clear_session_cookie())
        .add(session::clear_auth_return_cookie());
    (jar, Redirect::to("/login"))
}

#[derive(Deserialize)]
pub struct SwitchPseudonymForm {
    pseudonym: String,
}

pub async fn switch_pseudonym(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<SwitchPseudonymForm>,
) -> Result<Response, StatusCode> {
    let session_id = session::session_id_from_jar(&jar).ok_or(StatusCode::UNAUTHORIZED)?;
    let db = state.projection_store.db();
    let actor = session::load_session_actor(db, &session_id).ok_or(StatusCode::UNAUTHORIZED)?;
    let owner = pseudonym_owner(db, &form.pseudonym)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if owner != actor.uuid {
        return Err(StatusCode::FORBIDDEN);
    }
    session::update_session_pseudonym(db, &session_id, &form.pseudonym).map_err(|_| {
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Redirect::to("/login").into_response())
}

pub fn redirect_js(path: &str) -> crate::html::JsBuilder {
    crate::html::JsBuilder::new().raw(&format!(
        "window.location.href={};",
        crate::html::js_string_literal(path)
    ))
}

pub fn login_redirect_js() -> crate::html::JsBuilder {
    crate::html::JsBuilder::new().raw(
        "window.location.href='/login?return_to='+encodeURIComponent(window.location.pathname+window.location.search);",
    )
}

pub fn alias_redirect_js() -> crate::html::JsBuilder {
    crate::html::JsBuilder::new().raw(
        "window.location.href='/login/alias?return_to='+encodeURIComponent(window.location.pathname+window.location.search);",
    )
}

pub fn alias_status_js(message: &str, ok: bool) -> crate::html::JsBuilder {
    let class = if ok { "alias-ok" } else { "alias-bad" };
    crate::html::JsBuilder::new().raw(&format!(
        "var el=document.getElementById('alias-status'); if(el){{ el.textContent={}; el.className={}; }} var cf=document.getElementById('alias-claim-field'); if(cf) cf.value=document.getElementById('alias-input')?.value||'';",
        crate::html::js_string_literal(message),
        crate::html::js_string_literal(class),
    ))
}
