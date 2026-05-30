use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Page view recorded (path → counter in views.json).
    ViewRecorded { path: String, ts: i64 },
    /// Pairwise comparison vote (replayed into the scope's [`crate::reducer::GroupState`] on boot).
    /// `scope` is the ranking subject (e.g. a subreddit); empty string is the default/global scope.
    VoteRecorded {
        ts: i64,
        a: String,
        b: String,
        ratio_left: i32,
        ratio_right: i32,
        #[serde(default)]
        scope: String,
    },
}
