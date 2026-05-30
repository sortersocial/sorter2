use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Page view recorded (path → counter in views.json).
    ViewRecorded { path: String, ts: i64 },
    /// Pairwise comparison vote (replayed into the parent node's [`crate::reducer::GroupState`] on boot).
    /// `scope` is the parent [`crate::path_types::ItemId`] string; empty string is the tree root.
    VoteRecorded {
        ts: i64,
        a: String,
        b: String,
        ratio_left: i32,
        ratio_right: i32,
        #[serde(default)]
        scope: String,
    },
    /// Register a node path in the fractal tree (no external fetch).
    NodeEnsured { id: String },
    /// Full upstream API payload for a node (domain-specific view derived at replay/render time).
    EntityImported {
        id: String,
        ts: i64,
        payload: Value,
    },
}
