use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable item key for votes and rankings (opaque string for now).
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ItemId(String);

impl ItemId {
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        Some(Self(t.to_string()))
    }

    pub fn opaque(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
