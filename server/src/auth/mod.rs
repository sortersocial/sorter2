//! GitHub OAuth login, session cookies, and vote actor resolution.

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
    storage_schema::{oauth_link_owner, pseudonym_owner, Store, StoreFields},
    ui_action::UI_RPC_FIELD,
};

pub use session::{resolve_vote_actor, session_id_from_jar, VoteActor};

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
}

#[derive(Debug, Deserialize)]
pub struct GitHubStartQuery {
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

fn oauth_providers(base_url: &str, return_to: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if oauth::GitHubConfig::from_env(base_url).is_some() {
        out.push((
            "GitHub",
            format!(
                "/auth/github?return_to={}",
                urlencoding::encode(return_to)
            ),
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

fn login_body(
    session: Option<&session::SessionActor>,
    aliases: &[String],
    providers: &[(&str, String)],
) -> Markup {
    html! {
        main class="panel login-page" {
            div class="login-grid" {
                section class="login-oauth" {
                    h1 { "sign in" }
                    @if providers.is_empty() {
                        p class="muted" {
                            "OAuth is not configured. Set GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET."
                        }
                    } @else {
                        ul class="oauth-provider-list" {
                            @for (name, href) in providers {
                                li {
                                    a href=(href) class="button oauth-provider" data-testid=(format!("oauth-{}", name.to_lowercase())) {
                                        (format!("Continue with {name}"))
                                    }
                                }
                            }
                        }
                    }
                    @if let Some(actor) = session {
                        p class="muted small" {
                            "session active · weight " (format!("{:.1}", actor.trust_weight))
                        }
                        form method="post" action="/auth/logout" data-navigate="full" {
                            button type="submit" { "log out" }
                        }
                    }
                }
                section class="login-aliases" {
                    h2 { "your aliases" }
                    ul id="alias-list" class="alias-list" {
                        @if aliases.is_empty() {
                            li class="muted" data-testid="alias-list-empty" { "none yet" }
                        } @else {
                            @for alias in aliases {
                                li { (alias) }
                            }
                        }
                    }
                }
            }
            p { a href="/" { "← back" } }
        }
    }
}

pub async fn login_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<LoginQuery>,
) -> Response {
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
    let providers = oauth_providers(&base_url_from_env(state.cfg.port), &return_to);

    let markup = layout(
        "login · sorter2",
        login_body(session.as_ref(), &aliases, &providers),
        state.views.get_views("/login"),
    );
    (jar, Html(markup.into_string())).into_response()
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
        return Ok(Redirect::to(&return_to).into_response());
    }

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

    let body = html! {
        main class="panel alias-page" {
            h1 { "choose alias" }
            p class="muted" { "pick a unique display name for your votes" }
            form id="alias-check-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(check_rpc);
                label { "alias" }
                input type="text" id="alias-input" name="pseudonym" autocomplete="off"
                    data-testid="alias-input" maxlength="64";
                p id="alias-status" class="muted" data-testid="alias-status" { "type to check availability" }
            }
            form id="alias-claim-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(claim_rpc);
                input type="hidden" name="pseudonym" id="alias-claim-field" value="";
                button type="submit" class="btn-primary" data-testid="alias-claim" { "continue" }
            }
            p { a href="/login" { "← back to login" } }
        }
    };

    Ok(Html(
        layout(
            "choose alias · sorter2",
            body,
            state.views.get_views("/login/alias"),
        )
        .into_string(),
    )
    .into_response())
}

pub async fn github_start(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<GitHubStartQuery>,
) -> Result<Response, StatusCode> {
    let cfg = oauth::GitHubConfig::from_env(&base_url_from_env(state.cfg.port))
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let return_to = return_from_query_or_jar(&jar, query.return_to.as_deref());
    let state_token = session::new_oauth_state();
    let url = oauth::authorize_url(&cfg, &state_token, query.mock_user.as_deref());
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

async fn finish_oauth_login(
    state: &AppState,
    jar: CookieJar,
    provider: &str,
    provider_id: String,
) -> Result<(CookieJar, String), StatusCode> {
    let db = state.projection_store.db();
    let return_to = return_from_query_or_jar(&jar, None);

    let uuid = match oauth_link_owner(db, provider, &provider_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        Some(existing) => existing,
        None => {
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

    let token = oauth::exchange_code(&client, &cfg, &query.code)
        .await
        .map_err(|e| {
            tracing::warn!(err = %e, "github oauth token exchange failed");
            StatusCode::BAD_GATEWAY
        })?;
    let user = oauth::fetch_user(&client, &cfg.api_base, &token)
        .await
        .map_err(|e| {
            tracing::warn!(err = %e, "github user fetch failed");
            StatusCode::BAD_GATEWAY
        })?;

    let provider = "github";
    let provider_id = oauth::provider_id(&user);
    let (jar, dest) = finish_oauth_login(&state, jar, provider, provider_id).await?;
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
