//! Browser-only UI commands: JSON in hidden `__rpc__` plus hole fill ([`crate::form_template`]).

use crate::form_template::fill_template_from_form;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

pub const UI_RPC_FIELD: &str = "__rpc__";

/// HTML form / fetch `POST /ui` payload after template fill and deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HtmlUiAction {
    /// Record a pairwise vote within `scope` and morph `#ranking-panel`.
    RecordVote {
        a: String,
        b: String,
        ratio_left: i32,
        ratio_right: i32,
        /// Ranking subject (e.g. a subreddit). Empty string = default/global scope.
        #[serde(default)]
        scope: String,
    },
    /// Parse pasted Reddit URL/path; redirect to subreddit ranking on success.
    ParseQuery {
        query: String,
    },
}

#[derive(Debug, Error)]
pub enum HtmlUiParseError {
    #[error("missing __rpc__ field")]
    MissingRpc,
    #[error("invalid template json: {0}")]
    Template(serde_json::Error),
    #[error("invalid ui action: {0}")]
    Action(serde_json::Error),
}

pub fn parse_html_ui_from_form(
    form: &HashMap<String, String>,
) -> Result<HtmlUiAction, HtmlUiParseError> {
    let template = form.get(UI_RPC_FIELD).ok_or(HtmlUiParseError::MissingRpc)?;
    let mut hole_map = form.clone();
    hole_map.remove(UI_RPC_FIELD);
    let v: Value =
        fill_template_from_form(template, &hole_map).map_err(HtmlUiParseError::Template)?;
    serde_json::from_value(v).map_err(HtmlUiParseError::Action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_vote_round_trip_with_form_holes() {
        let template = serde_json::json!({
            "action": "record_vote",
            "a": {"$form": "item_a"},
            "b": {"$form": "item_b"},
            "ratio_left": 2,
            "ratio_right": 1,
            "scope": {"$form": "scope"}
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        form.insert("item_a".into(), "alpha".into());
        form.insert("item_b".into(), "beta".into());
        form.insert("scope".into(), "amitheasshole".into());
        assert_eq!(
            parse_html_ui_from_form(&form).unwrap(),
            HtmlUiAction::RecordVote {
                a: "alpha".into(),
                b: "beta".into(),
                ratio_left: 2,
                ratio_right: 1,
                scope: "amitheasshole".into(),
            }
        );
    }

    #[test]
    fn record_vote_scope_defaults_when_absent() {
        let template = serde_json::json!({
            "action": "record_vote",
            "a": "x",
            "b": "y",
            "ratio_left": 2,
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
                ratio_left: 2,
                ratio_right: 1,
                scope: String::new(),
            }
        );
    }

    #[test]
    fn parse_query_round_trip_with_form_hole() {
        let template = serde_json::json!({
            "action": "parse_query",
            "query": {"$form": "query"},
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        form.insert("query".into(), "r/rust".into());
        assert_eq!(
            parse_html_ui_from_form(&form).unwrap(),
            HtmlUiAction::ParseQuery {
                query: "r/rust".into(),
            }
        );
    }
}
