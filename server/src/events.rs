use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Page view recorded (path → counter in views.json).
    ViewRecorded { path: String, ts: i64 },
    /// Demo counter bump from `POST /ui` (persisted in the single JSONL log).
    DemoCounterBumped { ts: i64, value: u64 },
    /// Pairwise comparison vote (replayed into [`crate::reducer::GroupState`] on boot).
    VoteRecorded {
        ts: i64,
        a: String,
        b: String,
        ratio_left: i32,
        ratio_right: i32,
    },
}
