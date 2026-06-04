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
        let a = ItemId::from_storage(a)?;
        let b = ItemId::from_storage(b)?;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        vote.a = ItemId::from_storage(vote.a.as_str()).unwrap_or(vote.a.clone());
        vote.b = ItemId::from_storage(vote.b.as_str()).unwrap_or(vote.b.clone());
        if vote.ratio_left < 0 {
            vote.ratio_left = 0;
        }
        if vote.ratio_right < 0 {
            vote.ratio_right = 0;
        }
        if vote.ratio_left == 0 && vote.ratio_right == 0 {
            return;
        }
        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);

        let (i, j) = if a_idx < b_idx {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };

        let w_a = vote.ratio_left as f64;
        let w_b = vote.ratio_right as f64;

        self.voted_pairs.insert((i, j));
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
    /// Small preview (subreddit listing / child rows).
    pub thumb_url: Option<String>,
    /// Full-size still image for the post detail view.
    pub image_url: Option<String>,
    /// Outbound link for link/video posts (`url` / `url_overridden_by_dest`).
    pub link_url: Option<String>,
}

/// One node in the fractal tree: entity + ranked children.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeState {
    pub id: ItemId,
    /// Domain-specific view derived from imported payload (e.g. Reddit title/author).
    /// Raw JSON lives in [`crate::entity_store::EntityStore`].
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

    pub fn apply_entity(&mut self, id: &ItemId, view: Option<EntityData>) {
        self.ensure_path(id);
        if let Some(node) = self.nodes.get_mut(id) {
            node.data = view;
        }
    }

    /// Import entity view for `id` and attach it as a direct child of `parent`
    /// without running [`Self::ensure_path`] on `id` (avoids Reddit `/comments/`
    /// parent rules pulling intermediate path segments into the subreddit).
    pub fn apply_entity_under_parent(
        &mut self,
        parent: &ItemId,
        id: &ItemId,
        view: Option<EntityData>,
    ) {
        self.ensure_path(parent);
        self.ensure_node(id);
        if let Some(node) = self.nodes.get_mut(id) {
            node.data = view;
        }
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.insert(id.clone());
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
    fn zero_weight_vote_does_not_mark_pair_or_edges() {
        let mut g = GroupState::new();
        g.apply_vote(VoteData {
            ts: 1,
            a: ItemId::opaque("a"),
            b: ItemId::opaque("b"),
            ratio_left: 0,
            ratio_right: 0,
            body: String::new(),
            principal: "test".to_string(),
            delegate: None,
            thread_tag: "untagged".to_string(),
        });
        assert!(g.voted_pairs.is_empty());
        assert!(g.edges.is_empty());
        assert!(g.recent_votes.is_empty());
    }

    #[test]
    fn ensure_path_wires_children() {
        let mut tree = GlobalTree::new();
        let id = ItemId::from_url("https://reddit.com/r/rust").unwrap();
        tree.ensure_path(&id);
        let root = tree.get(&ItemId::root()).unwrap();
        assert!(root
            .children
            .contains(&ItemId::from_url("https://reddit.com").unwrap()));
        let reddit = tree
            .get(&ItemId::from_url("https://reddit.com").unwrap())
            .unwrap();
        assert!(reddit
            .children
            .contains(&ItemId::from_url("https://reddit.com/r").unwrap()));
        let sub = tree.get(&id).unwrap();
        assert_eq!(sub.id, id);
    }
}
