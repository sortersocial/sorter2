use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Form,
};
use std::collections::HashMap;

use crate::{
    fetch,
    html::{input_panel, js_string_literal, ranking_panel, JsBuilder},
    parser::parse_reddit_url,
    path_types::ItemId,
    reddit::ensure_partial_tree,
    state::{parse_item_param, AppState},
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

pub async fn post_ui_html(
    State(state): State<AppState>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let action = match parse_html_ui_from_form(&form) {
        Ok(a) => a,
        Err(e) => return ui_js_warn(&e.to_string()).into_response(),
    };

    match action {
        HtmlUiAction::RecordVote {
            a,
            b,
            ratio_left,
            ratio_right,
            scope,
        } => {
            let parent = parent_from_scope(&scope);
            if let Err(e) = state
                .record_vote(&parent, &a, &b, ratio_left, ratio_right)
                .await
            {
                return ui_js_warn(&e).into_response();
            }
            let tree = state.tree.read().await;
            let empty = crate::reducer::NodeState::default();
            let node = tree.get(&parent).unwrap_or(&empty);
            let panel = ranking_panel(&parent, node, &tree);
            JsBuilder::new()
                .morph_selector("#ranking-panel", panel)
                .into_response()
        }
        HtmlUiAction::ParseQuery { query } => match parse_reddit_url(&query) {
            Ok(item) => {
                {
                    let mut tree = state.tree.write().await;
                    ensure_partial_tree(&mut tree, &item);
                }
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
            fetch::fetch_entity_stream(state, id, kind).into_response()
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
            }
        );
    }
}
