use serde::{Deserialize, Serialize};

/// Schema version for JSONL log records. Bump when event semantics change.
pub const CURRENT_LOG_SCHEMA: u32 = 1;

/// One JSONL line: schema envelope around a payload event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord<E> {
    pub schema: u32,
    pub seq: u64,
    pub ts: i64,
    pub event: E,
}

impl<E> LogRecord<E> {
    pub fn new(seq: u64, ts: i64, event: E) -> Self {
        Self {
            schema: CURRENT_LOG_SCHEMA,
            seq,
            ts,
            event,
        }
    }
}

pub type EventRecord = LogRecord<Event>;
pub type ViewRecord = LogRecord<ViewEvent>;

/// Wall-clock timestamp carried on the log envelope for domain events.
pub fn event_timestamp(event: &Event) -> i64 {
    match event {
        Event::VoteRecorded { ts, .. } => *ts,
        Event::NodeEnsured { .. } => crate::fetch::now_ms(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewEvent {
    PageView { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
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
}
