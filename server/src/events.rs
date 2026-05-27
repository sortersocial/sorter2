use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Page view recorded (path → counter in views.json).
    ViewRecorded { path: String, ts: i64 },
    /// Demo counter bump from `POST /ui` (persisted in the single JSONL log).
    DemoCounterBumped { ts: i64, value: u64 },
}
