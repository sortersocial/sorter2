use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use std::collections::HashMap;

use crate::{
    auth::{
        alias_redirect_js, alias_status_js, config, login_redirect_js, oauth, redirect_js,
        resolve_vote_actor,
        session::{load_valid_session, session_has_pseudonym, session_id_from_jar},
    },
    fetch,
    html::{input_panel, js_string_literal, ranking_panel, JsBuilder},
    nsfw::{item_is_nsfw_in_store, nsfw_allowed},
    parser::parse_reddit_url,
    path_types::ItemId,
    state::{parse_item_param, AppState},
    storage_schema::{load_user_skips, pseudonym_owner},
    ui_action::{parse_html_ui_from_form, HtmlUiAction},
};

fn ui_js_warn(msg: &str) -> Response {
    let js = format!("console.warn({});", crate::html::js_string_literal(msg));
    axum::response::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )
        .body(axum::body::Body::from(js))
        .unwrap()
}

fn parent_from_scope(scope: &str) -> ItemId {
    parse_item_param(scope)
}

fn vote_auth_redirect(state: &AppState, jar: &CookieJar) -> Option<Response> {
    let db = state.projection_store.db();
    let session = session_id_from_jar(jar)
        .as_deref()
        .and_then(|id| load_valid_session(db, id));
    match session {
        None => Some(login_redirect_js().into_response()),
        Some(s) if !session_has_pseudonym(&s) => Some(alias_redirect_js().into_response()),
        Some(_) => None,
    }
}

pub async fn post_ui_html(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let action = match parse_html_ui_from_form(&form) {
        Ok(a) => a,
        Err(e) => return ui_js_warn(&e.to_string()).into_response(),
    };

    let nsfw_ok = nsfw_allowed(&jar);

    match action {
        HtmlUiAction::RecordVote {
            a,
            b,
            ratio_left,
            ratio_right,
            scope,
            vote_compare,
        } => {
            if let Some(resp) = vote_auth_redirect(&state, &jar) {
                return resp;
            }
            let parent = parent_from_scope(&scope);
            let actor = match resolve_vote_actor(
                state.projection_store.db(),
                session_id_from_jar(&jar).as_deref(),
            ) {
                Ok(actor) => actor,
                Err(_) => {
                    return vote_auth_redirect(&state, &jar)
                        .unwrap_or_else(|| login_redirect_js().into_response());
                }
            };
            let left = parse_item_param(&a);
            let right = parse_item_param(&b);
            let skipped =
                load_user_skips(state.projection_store.db(), &actor.uuid).unwrap_or_default();
            if skipped.contains(&left) || skipped.contains(&right) {
                return ui_js_warn("restore skipped items from your account before voting")
                    .into_response();
            }
            // Strict boundary: refuse votes that would surface NSFW without opt-in.
            if !nsfw_ok {
                let store = &state.projection_store;
                if item_is_nsfw_in_store(store, &parent)
                    || item_is_nsfw_in_store(store, &left)
                    || item_is_nsfw_in_store(store, &right)
                {
                    return ui_js_warn("NSFW opt-in required").into_response();
                }
            }
            if let Err(e) = state
                .record_vote(&parent, &a, &b, ratio_left, ratio_right, &actor)
                .await
            {
                return ui_js_warn(&e).into_response();
            }
            let tree = match state.scope_tree(&parent) {
                Ok(tree) => tree,
                Err(e) => return ui_js_warn(&e).into_response(),
            };
            if vote_compare {
                let morph = crate::html::vote::vote_recorded_morph(
                    &tree, &parent, &left, &right, nsfw_ok, &skipped,
                );
                return morph.into_response();
            }
            let empty = crate::reducer::NodeState::default();
            let node = tree.get(&parent).unwrap_or(&empty);
            let panel = ranking_panel(&parent, node, &tree, nsfw_ok, &skipped);
            JsBuilder::new()
                .morph_selector("#ranking-panel", panel)
                .into_response()
        }
        HtmlUiAction::CheckPseudonym { pseudonym } => {
            let db = state.projection_store.db();
            let session_id = match session_id_from_jar(&jar) {
                Some(id) => id,
                None => return alias_status_js("sign in first", false).into_response(),
            };
            let session = match load_valid_session(db, &session_id) {
                Some(s) => s,
                None => return alias_status_js("session expired", false).into_response(),
            };
            match oauth::validate_pseudonym(&pseudonym) {
                Err(msg) => alias_status_js(msg, false).into_response(),
                Ok(name) => match pseudonym_owner(db, &name) {
                    Ok(None) => alias_status_js("available", true).into_response(),
                    Ok(Some(owner)) if owner == session.uuid => {
                        alias_status_js("already yours", true).into_response()
                    }
                    Ok(Some(_)) => alias_status_js("taken", false).into_response(),
                    Err(e) => ui_js_warn(&e.to_string()).into_response(),
                },
            }
        }
        HtmlUiAction::ClaimPseudonym {
            pseudonym,
            return_to,
        } => {
            let db = state.projection_store.db();
            let session_id = match session_id_from_jar(&jar) {
                Some(id) => id,
                None => return login_redirect_js().into_response(),
            };
            let session = match load_valid_session(db, &session_id) {
                Some(s) => s,
                None => return login_redirect_js().into_response(),
            };
            let name = match oauth::validate_pseudonym(&pseudonym) {
                Ok(n) => n,
                Err(msg) => return alias_status_js(msg, false).into_response(),
            };
            if let Ok(Some(owner)) = pseudonym_owner(db, &name) {
                if owner != session.uuid {
                    return alias_status_js("taken", false).into_response();
                }
            } else if let Err(e) = state.claim_pseudonym(&session.uuid, &name).await {
                return ui_js_warn(&e).into_response();
            }
            if let Err(e) = crate::auth::session::update_session_pseudonym(&db, &session_id, &name)
            {
                return ui_js_warn(&e).into_response();
            }
            redirect_js(&config::sanitize_return_to(&return_to)).into_response()
        }
        HtmlUiAction::SkipItem { item, parent } => {
            if let Some(resp) = vote_auth_redirect(&state, &jar) {
                return resp;
            }
            let session_id = match session_id_from_jar(&jar) {
                Some(id) => id,
                None => return login_redirect_js().into_response(),
            };
            let session = match load_valid_session(state.projection_store.db(), &session_id) {
                Some(session) => session,
                None => return login_redirect_js().into_response(),
            };
            let item = parse_item_param(&item);
            if item.is_root() {
                return ui_js_warn("cannot skip the root item").into_response();
            }
            if let Err(e) = state.set_item_skipped(&session.uuid, &item, true).await {
                return ui_js_warn(&e).into_response();
            }
            redirect_js(&crate::html::vote::vote_href(&parent_from_scope(&parent))).into_response()
        }
        HtmlUiAction::UnskipItem { item } => {
            let session_id = match session_id_from_jar(&jar) {
                Some(id) => id,
                None => return login_redirect_js().into_response(),
            };
            let session = match load_valid_session(state.projection_store.db(), &session_id) {
                Some(session) => session,
                None => return login_redirect_js().into_response(),
            };
            let item = parse_item_param(&item);
            if let Err(e) = state.set_item_skipped(&session.uuid, &item, false).await {
                return ui_js_warn(&e).into_response();
            }
            redirect_js("/login").into_response()
        }
        HtmlUiAction::ParseQuery { query } => match parse_reddit_url(&query) {
            Ok(item) => {
                let _ = state.ensure_node(&item).await;
                let dest = item.browse_href();
                JsBuilder::new()
                    .raw(&format!(
                        "window.location.href={};",
                        js_string_literal(&dest)
                    ))
                    .into_response()
            }
            Err(message) => {
                let panel = input_panel(&query, Some(&message));
                JsBuilder::new()
                    .morph_selector("#parser-panel", panel)
                    .into_response()
            }
        },
        HtmlUiAction::FetchEntity { item, kind } => {
            let id = parse_item_param(&item);
            let skipped = crate::skip::for_jar(&state.projection_store, &jar);
            fetch::fetch_entity_stream(state, id, kind, nsfw_ok, skipped).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_action::{HtmlUiAction, HtmlUiParseError, UI_RPC_FIELD};

    #[test]
    fn parse_error_is_warn_not_panic() {
        let form = HashMap::new();
        let err = parse_html_ui_from_form(&form).unwrap_err();
        assert!(matches!(err, HtmlUiParseError::MissingRpc));
    }

    #[test]
    fn record_vote_action_deserializes() {
        let template = serde_json::json!({
            "action": "record_vote",
            "a": "x",
            "b": "y",
            "ratio_left": 3,
            "ratio_right": 1
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        assert_eq!(
            parse_html_ui_from_form(&form).unwrap(),
            HtmlUiAction::RecordVote {
                a: "x".into(),
                b: "y".into(),
                ratio_left: 3,
                ratio_right: 1,
                scope: String::new(),
                vote_compare: false,
            }
        );
    }
}
