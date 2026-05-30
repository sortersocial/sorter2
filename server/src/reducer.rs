use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::path_types::ItemId;

/// Parsed pairwise vote (internal representation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteData {
    pub ts: i64,
    pub a: ItemId,
    pub b: ItemId,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub body: String,
    pub principal: String,
    pub delegate: Option<String>,
    pub thread_tag: String,
}

impl VoteData {
    /// Build a vote from persisted event fields (web UI / replay).
    pub fn from_recorded(
        ts: i64,
        a: &str,
        b: &str,
        ratio_left: i32,
        ratio_right: i32,
    ) -> Option<Self> {
        let a = ItemId::parse(a)?;
        let b = ItemId::parse(b)?;
        if a == b {
            return None;
        }
        Some(Self {
            ts,
            a,
            b,
            ratio_left,
            ratio_right,
            body: String::new(),
            principal: "web".to_string(),
            delegate: None,
            thread_tag: "default".to_string(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupState {
    pub item_to_idx: HashMap<ItemId, usize>,
    pub idx_to_item: Vec<ItemId>,
    pub edges: HashMap<(usize, usize), f64>,
    pub voted_pairs: HashSet<(usize, usize)>,
    pub recent_votes: VecDeque<VoteData>,
}

impl GroupState {
    pub fn new() -> Self {
        Self {
            item_to_idx: HashMap::new(),
            idx_to_item: Vec::new(),
            edges: HashMap::new(),
            voted_pairs: HashSet::new(),
            recent_votes: VecDeque::with_capacity(200),
        }
    }

    fn ensure_item(&mut self, item: &ItemId) -> usize {
        if let Some(&idx) = self.item_to_idx.get(item) {
            return idx;
        }
        let idx = self.idx_to_item.len();
        self.idx_to_item.push(item.clone());
        self.item_to_idx.insert(item.clone(), idx);
        idx
    }

    fn add_edge_weight(&mut self, src: usize, dst: usize, w: f64) {
        if w <= 0.0 {
            return;
        }
        *self.edges.entry((src, dst)).or_insert(0.0) += w;
    }

    pub fn apply_vote(&mut self, mut vote: VoteData) {
        vote.a = ItemId::parse(vote.a.as_str()).unwrap_or_else(|| vote.a.clone());
        vote.b = ItemId::parse(vote.b.as_str()).unwrap_or_else(|| vote.b.clone());
        if vote.ratio_left < 0 {
            vote.ratio_left = 0;
        }
        if vote.ratio_right < 0 {
            vote.ratio_right = 0;
        }
        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        let (i, j) = if a_idx < b_idx {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };
        self.voted_pairs.insert((i, j));

        let w_a = vote.ratio_left as f64;
        let w_b = vote.ratio_right as f64;

        self.add_edge_weight(b_idx, a_idx, w_a);
        self.add_edge_weight(a_idx, b_idx, w_b);

        self.recent_votes.push_front(vote);
        while self.recent_votes.len() > 200 {
            self.recent_votes.pop_back();
        }
    }
}

#[cfg(test)]
mod from_recorded_tests {
    use super::*;

    #[test]
    fn rejects_same_item() {
        assert!(VoteData::from_recorded(1, "a", "a", 2, 1).is_none());
    }

    #[test]
    fn rejects_empty() {
        assert!(VoteData::from_recorded(1, "", "b", 2, 1).is_none());
    }
}
