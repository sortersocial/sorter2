use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Form,
};
use std::collections::HashMap;

use crate::{
    html::{js_string_literal, ranking_panel, JsBuilder},
    parser::parse_reddit_url,
    parser_render::parser_panel_morph,
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
        } => {
            if let Err(e) = state
                .record_vote(&a, &b, ratio_left, ratio_right)
                .await
            {
                return ui_js_warn(&e).into_response();
            }
            let group = state.group.read().await;
            let panel = ranking_panel(&group);
            JsBuilder::new()
                .morph_selector("#ranking-panel", panel)
                .into_response()
        }
        HtmlUiAction::ParseQuery { query } => {
            let action = parse_reddit_url(&query);
            let panel = parser_panel_morph(&query, &action);
            let mut js = JsBuilder::new().morph_selector("#parser-panel", panel);
            if let Some(comp) = action.primary_completion() {
                js = js.raw(&format!(
                    "var __pi=document.getElementById('parser-input'); if(__pi){{__pi.dataset.completion={};}}",
                    js_string_literal(comp)
                ));
            }
            js.into_response()
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
            }
        );
    }
}
