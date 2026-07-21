use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::path_types::ItemId;

/// `(actor_uuid, min_item_id, max_item_id)` — one vote slot per human per pair.
pub type UuidVoteKey = (String, String, String);

pub fn canonical_pair_ids(a: &ItemId, b: &ItemId) -> (String, String) {
    let ak = a.as_str().to_string();
    let bk = b.as_str().to_string();
    if ak <= bk {
        (ak, bk)
    } else {
        (bk, ak)
    }
}

pub fn uuid_vote_key(actor_uuid: &str, a: &ItemId, b: &ItemId) -> UuidVoteKey {
    let (lo, hi) = canonical_pair_ids(a, b);
    (actor_uuid.to_string(), lo, hi)
}

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

/// Votes cast within one ranking scope (parent node). Edges and rankings are
/// derived on demand from [`Self::uuid_votes`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeVotes {
    pub uuid_votes: HashMap<UuidVoteKey, VoteData>,
    pub recent_votes: Vec<VoteData>,
}

impl ScopeVotes {
    pub fn apply_vote(&mut self, vote: VoteData, actor_uuid: &str) {
        let key = uuid_vote_key(actor_uuid, &vote.a, &vote.b);
        self.uuid_votes.insert(key, vote.clone());
        self.recent_votes.push(vote);
    }
}

/// Structured data imported from Reddit or elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityData {
    pub title: String,
    pub author: Option<String>,
    pub body_html: Option<String>,
    /// Reddit `over_18` / `over18`. Used for the NSFW content dimension.
    #[serde(default)]
    pub over_18: bool,
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
    /// Durable Reddit safety classification. `None` means the node has not yet
    /// been classified and must not appear in listings.
    #[serde(default)]
    pub nsfw_classification: Option<bool>,
    pub children: HashSet<ItemId>,
    pub votes: ScopeVotes,
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
            node.votes.apply_vote(vote, actor_uuid);
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
    use crate::ranking::edge_weight_sum;

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
        assert!(VoteData::from_event(1, "a", "a", 2, 1, "anon".into(), 1.0).is_none());
    }

    #[test]
    fn from_event_rejects_empty_pair() {
        assert!(VoteData::from_event(1, "", "b", 2, 1, "anon".into(), 1.0).is_none());
    }

    #[test]
    fn same_uuid_replaces_prior_vote_on_pair() {
        let mut scope = ScopeVotes::default();
        let uuid = "u1";
        scope.apply_vote(vote(1, "a", "b", 2, 1, "alice"), uuid);
        assert_eq!(edge_weight_sum(&scope), 3.0);

        scope.apply_vote(vote(2, "a", "b", 0, 1, "bob"), uuid);
        assert_eq!(edge_weight_sum(&scope), 1.0);
        assert_eq!(scope.uuid_votes.len(), 1);
    }

    #[test]
    fn different_uuids_both_count() {
        let mut scope = ScopeVotes::default();
        scope.apply_vote(vote(1, "a", "b", 2, 1, "alice"), "u1");
        scope.apply_vote(vote(2, "a", "b", 0, 1, "bob"), "u2");
        assert_eq!(edge_weight_sum(&scope), 4.0);
        assert_eq!(scope.uuid_votes.len(), 2);
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
