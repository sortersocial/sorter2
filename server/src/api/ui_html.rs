use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Form,
};
use std::collections::HashMap;

use crate::{
    html::{js_string_literal, ranking_panel, JsBuilder},
    parser::parse_reddit_url,
    parser_render::navigate_panel,
    state::AppState,
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
            if let Err(e) = state
                .record_vote(&scope, &a, &b, ratio_left, ratio_right)
                .await
            {
                return ui_js_warn(&e).into_response();
            }
            let scope = crate::state::normalize_scope(&scope);
            let groups = state.groups.read().await;
            let empty = crate::reducer::GroupState::new();
            let group = groups.get(&scope).unwrap_or(&empty);
            let panel = ranking_panel(&scope, group);
            JsBuilder::new()
                .morph_selector("#ranking-panel", panel)
                .into_response()
        }
        HtmlUiAction::ParseQuery { query } => match parse_reddit_url(&query) {
            Ok(subreddit) => {
                let dest = format!("/?sub={subreddit}");
                JsBuilder::new()
                    .raw(&format!(
                        "window.location.href={};",
                        js_string_literal(&dest)
                    ))
                    .into_response()
            }
            Err(message) => {
                let panel = navigate_panel(&query, Some(&message));
                JsBuilder::new()
                    .morph_selector("#parser-panel", panel)
                    .into_response()
            }
        },
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
