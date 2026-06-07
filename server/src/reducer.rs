use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::path_types::ItemId;

/// Parsed pairwise vote (internal representation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoteData {
    pub ts: i64,
    pub a: ItemId,
    pub b: ItemId,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub pseudonym: String,
    pub trust_weight: f64,
}

impl VoteData {
    /// Build a vote from validated event fields (replay / tests).
    pub fn from_event(
        ts: i64,
        a: &str,
        b: &str,
        ratio_left: i32,
        ratio_right: i32,
        pseudonym: String,
        trust_weight: f64,
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
            pseudonym,
            trust_weight,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupState {
    pub item_to_idx: HashMap<ItemId, usize>,
    pub idx_to_item: Vec<ItemId>,
    pub edges: HashMap<(usize, usize), f64>,
    pub voted_pairs: HashSet<(usize, usize)>,
    /// Latest vote per `(actor_uuid, min_idx, max_idx)` — Sybil dedup anchor.
    pub uuid_votes: HashMap<(String, usize, usize), VoteData>,
    pub recent_votes: Vec<VoteData>,
}

impl GroupState {
    pub fn new() -> Self {
        Self {
            item_to_idx: HashMap::new(),
            idx_to_item: Vec::new(),
            edges: HashMap::new(),
            voted_pairs: HashSet::new(),
            uuid_votes: HashMap::new(),
            recent_votes: Vec::new(),
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

    fn subtract_edge_weight(&mut self, src: usize, dst: usize, w: f64) {
        if w <= 0.0 {
            return;
        }
        if let Some(entry) = self.edges.get_mut(&(src, dst)) {
            *entry -= w;
            if *entry <= 0.0 {
                self.edges.remove(&(src, dst));
            }
        }
    }

    fn apply_weights(&mut self, vote: &VoteData, a_idx: usize, b_idx: usize) {
        let w_a = vote.ratio_left as f64 * vote.trust_weight;
        let w_b = vote.ratio_right as f64 * vote.trust_weight;
        let (i, j) = if a_idx < b_idx {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };
        self.voted_pairs.insert((i, j));
        self.add_edge_weight(b_idx, a_idx, w_a);
        self.add_edge_weight(a_idx, b_idx, w_b);
    }

    fn rollback_weights(&mut self, vote: &VoteData) {
        let a_idx = match self.item_to_idx.get(&vote.a) {
            Some(&i) => i,
            None => return,
        };
        let b_idx = match self.item_to_idx.get(&vote.b) {
            Some(&i) => i,
            None => return,
        };
        let w_a = vote.ratio_left as f64 * vote.trust_weight;
        let w_b = vote.ratio_right as f64 * vote.trust_weight;
        self.subtract_edge_weight(b_idx, a_idx, w_a);
        self.subtract_edge_weight(a_idx, b_idx, w_b);
    }

    /// Apply a validated vote, deduplicating by `actor_uuid` per unordered pair.
    pub fn apply_vote(&mut self, vote: VoteData, actor_uuid: &str) {
        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);
        let (i, j) = if a_idx < b_idx {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };

        let dedupe_key = (actor_uuid.to_string(), i, j);
        if let Some(old) = self.uuid_votes.get(&dedupe_key).cloned() {
            self.rollback_weights(&old);
        }

        self.apply_weights(&vote, a_idx, b_idx);
        self.uuid_votes.insert(dedupe_key, vote.clone());
        self.recent_votes.push(vote);
    }

    /// Rebuild edge weights from deduped uuid votes (load path — no rollback).
    pub fn ingest_uuid_vote(&mut self, vote: VoteData, actor_uuid: &str) {
        let a_idx = self.ensure_item(&vote.a);
        let b_idx = self.ensure_item(&vote.b);
        let (i, j) = if a_idx < b_idx {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };
        self.apply_weights(&vote, a_idx, b_idx);
        self.uuid_votes.insert((actor_uuid.to_string(), i, j), vote);
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
    /// Ephemeral display view (Reddit title/author/etc.; not event-logged).
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

    pub fn apply_vote(&mut self, parent: &ItemId, vote: VoteData, actor_uuid: &str) {
        self.ensure_path(parent);
        self.ensure_path(&vote.a);
        self.ensure_path(&vote.b);
        if let Some(node) = self.nodes.get_mut(parent) {
            node.children.insert(vote.a.clone());
            node.children.insert(vote.b.clone());
            node.local_ranking.apply_vote(vote, actor_uuid);
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
mod tests {
    use super::*;

    fn vote(ts: i64, a: &str, b: &str, l: i32, r: i32, pseudonym: &str) -> VoteData {
        VoteData {
            ts,
            a: ItemId::opaque(a),
            b: ItemId::opaque(b),
            ratio_left: l,
            ratio_right: r,
            pseudonym: pseudonym.to_string(),
            trust_weight: 1.0,
        }
    }

    #[test]
    fn from_event_rejects_same_item() {
        assert!(VoteData::from_event(
            1,
            "a",
            "a",
            2,
            1,
            "anon".into(),
            1.0
        )
        .is_none());
    }

    #[test]
    fn from_event_rejects_empty_pair() {
        assert!(VoteData::from_event(1, "", "b", 2, 1, "anon".into(), 1.0).is_none());
    }

    #[test]
    fn same_uuid_replaces_prior_vote_on_pair() {
        let mut g = GroupState::new();
        let uuid = "u1";
        g.apply_vote(vote(1, "a", "b", 2, 1, "alice"), uuid);
        let first_total: f64 = g.edges.values().sum();
        assert_eq!(first_total, 3.0);

        g.apply_vote(vote(2, "a", "b", 0, 1, "bob"), uuid);
        let second_total: f64 = g.edges.values().sum();
        assert_eq!(second_total, 1.0);
        assert_eq!(g.uuid_votes.len(), 1);
    }

    #[test]
    fn different_uuids_both_count() {
        let mut g = GroupState::new();
        g.apply_vote(vote(1, "a", "b", 2, 1, "alice"), "u1");
        g.apply_vote(vote(2, "a", "b", 0, 1, "bob"), "u2");
        let total: f64 = g.edges.values().sum();
        assert_eq!(total, 4.0);
        assert_eq!(g.uuid_votes.len(), 2);
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
