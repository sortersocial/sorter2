//! Browser-only UI commands: JSON in hidden `__rpc__` plus hole fill ([`crate::form_template`]).

use crate::form_template::fill_template_from_form;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

pub const UI_RPC_FIELD: &str = "__rpc__";

/// What a `fetch_entity` action targets: the node itself, or its children.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FetchTarget {
    #[default]
    #[serde(rename = "self")]
    SelfEntity,
    #[serde(rename = "children")]
    Children,
}

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
        /// Parent node [`ItemId`] string; empty = tree root.
        #[serde(default)]
        scope: String,
        /// Posted from `/vote` compare UI — morph edge history in place.
        #[serde(default)]
        vote_compare: bool,
    },
    /// Parse pasted Reddit URL/path; redirect to subreddit ranking on success.
    ParseQuery { query: String },
    /// Import entity data; `POST /ui` responds with `text/event-stream` whose
    /// events carry JS snippets to `eval` (Idiomorph morphs), not JSON.
    FetchEntity {
        item: String,
        #[serde(default)]
        kind: FetchTarget,
    },
    /// Refresh Reddit display content for ranked items in a scope's ranking panel.
    FetchEntitiesBatch {
        /// Parent scope [`ItemId`] string (for re-rendering `#ranking-panel`).
        scope: String,
        items: Vec<String>,
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
    fn record_vote_round_trip_with_typed_ratio_holes() {
        let template = serde_json::json!({
            "action": "record_vote",
            "a": "x",
            "b": "y",
            "ratio_left": {"$form:i32": "ratio_left"},
            "ratio_right": {"$form:i32": "ratio_right"},
            "scope": "parent",
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        form.insert("ratio_left".into(), "60".into());
        form.insert("ratio_right".into(), "40".into());
        assert_eq!(
            parse_html_ui_from_form(&form).unwrap(),
            HtmlUiAction::RecordVote {
                a: "x".into(),
                b: "y".into(),
                ratio_left: 60,
                ratio_right: 40,
                scope: "parent".into(),
                vote_compare: false,
            }
        );
    }

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
                vote_compare: false,
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
                vote_compare: false,
            }
        );
    }

    #[test]
    fn fetch_entities_batch_deserializes() {
        let v = serde_json::json!({
            "action": "fetch_entities_batch",
            "scope": "https://reddit.com/r/rust",
            "items": [
                "https://reddit.com/r/rust/comments/abc/t",
                "https://reddit.com/r/rust/comments/def/u"
            ],
        });
        assert_eq!(
            serde_json::from_value::<HtmlUiAction>(v).unwrap(),
            HtmlUiAction::FetchEntitiesBatch {
                scope: "https://reddit.com/r/rust".into(),
                items: vec![
                    "https://reddit.com/r/rust/comments/abc/t".into(),
                    "https://reddit.com/r/rust/comments/def/u".into(),
                ],
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
