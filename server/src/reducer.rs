use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Structured data imported from Reddit or elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityData {
    pub title: String,
    pub author: Option<String>,
    pub body_html: Option<String>,
    pub thumb_url: Option<String>,
}

/// One node in the fractal tree: entity + ranked children.
#[derive(Debug, Clone, Default)]
pub struct NodeState {
    pub id: ItemId,
    /// Full imported API JSON (persisted in the event log).
    pub entity_raw: Option<Value>,
    /// Domain-specific view derived from `entity_raw` (e.g. Reddit title/author).
    pub data: Option<EntityData>,
    pub children: HashSet<ItemId>,
    pub local_ranking: GroupState,
}

impl NodeState {
    fn new(id: ItemId) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }
}

/// Global fractal graph: every URL is both an item and a ranking scope for its children.
#[derive(Default)]
pub struct GlobalTree {
    pub nodes: HashMap<ItemId, NodeState>,
}

impl GlobalTree {
    pub fn new() -> Self {
        let mut tree = Self::default();
        tree.ensure_node(&ItemId::root());
        tree
    }

    pub fn ensure_node(&mut self, id: &ItemId) -> &mut NodeState {
        if !self.nodes.contains_key(id) {
            self.nodes.insert(id.clone(), NodeState::new(id.clone()));
        }
        self.nodes.get_mut(id).expect("node just inserted")
    }

    /// Register a node and wire parent→child links along the canonical path.
    pub fn ensure_path(&mut self, id: &ItemId) {
        if id.is_root() {
            self.ensure_node(id);
            return;
        }
        self.ensure_node(&ItemId::root());
        for path in id.breadcrumb_paths() {
            self.ensure_node(&path);
            if let Some(parent) = path.parent() {
                self.ensure_node(&parent);
                if let Some(p) = self.nodes.get_mut(&parent) {
                    p.children.insert(path.clone());
                }
            } else if let Some(r) = self.nodes.get_mut(&ItemId::root()) {
                r.children.insert(path.clone());
            }
        }
    }

    pub fn get(&self, id: &ItemId) -> Option<&NodeState> {
        self.nodes.get(id)
    }

    pub fn apply_vote(&mut self, parent: &ItemId, vote: VoteData) {
        self.ensure_path(parent);
        self.ensure_path(&vote.a);
        self.ensure_path(&vote.b);
        if let Some(node) = self.nodes.get_mut(parent) {
            node.children.insert(vote.a.clone());
            node.children.insert(vote.b.clone());
            node.local_ranking.apply_vote(vote);
        }
    }

    pub fn apply_entity_raw(&mut self, id: &ItemId, payload: Value, view: Option<EntityData>) {
        self.ensure_path(id);
        if let Some(node) = self.nodes.get_mut(id) {
            node.entity_raw = Some(payload);
            node.data = view;
        }
    }

    /// Directly attach `child` under `parent`, bypassing path-based nesting.
    /// Used for imported listings (e.g. a subreddit's posts) so they show up
    /// as children of the subreddit rather than a deep `…/comments/<id>` path.
    pub fn link_child(&mut self, parent: &ItemId, child: &ItemId) {
        self.ensure_path(parent);
        self.ensure_path(child);
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.insert(child.clone());
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
    fn rejects_empty_pair() {
        assert!(VoteData::from_recorded(1, "", "b", 2, 1).is_none());
    }

    #[test]
    fn ensure_path_wires_children() {
        let mut tree = GlobalTree::new();
        let id = ItemId::parse("reddit.com/r/rust").unwrap();
        tree.ensure_path(&id);
        let root = tree.get(&ItemId::root()).unwrap();
        assert!(root.children.contains(&ItemId::parse("reddit.com").unwrap()));
        let reddit = tree.get(&ItemId::parse("reddit.com").unwrap()).unwrap();
        assert!(reddit.children.contains(&ItemId::parse("reddit.com/r").unwrap()));
        let sub = tree.get(&id).unwrap();
        assert_eq!(sub.id, id);
    }
}
