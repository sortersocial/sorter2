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
    /// Demo: morph `#demo-counter-panel` after bumping the persisted counter.
    BumpDemoCounter,
    /// Record a pairwise vote and morph `#ranking-panel`.
    RecordVote {
        a: String,
        b: String,
        ratio_left: i32,
        ratio_right: i32,
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
    fn bump_demo_counter_round_trip() {
        let template = serde_json::json!({ "action": "bump_demo_counter" });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        let a = parse_html_ui_from_form(&form).unwrap();
        assert_eq!(a, HtmlUiAction::BumpDemoCounter);
    }

    #[test]
    fn record_vote_round_trip_with_form_holes() {
        let template = serde_json::json!({
            "action": "record_vote",
            "a": {"$form": "item_a"},
            "b": {"$form": "item_b"},
            "ratio_left": 2,
            "ratio_right": 1
        });
        let mut form = HashMap::new();
        form.insert(
            UI_RPC_FIELD.to_string(),
            serde_json::to_string(&template).unwrap(),
        );
        form.insert("item_a".into(), "alpha".into());
        form.insert("item_b".into(), "beta".into());
        assert_eq!(
            parse_html_ui_from_form(&form).unwrap(),
            HtmlUiAction::RecordVote {
                a: "alpha".into(),
                b: "beta".into(),
                ratio_left: 2,
                ratio_right: 1,
            }
        );
    }
}
