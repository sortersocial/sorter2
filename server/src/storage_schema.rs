//! Durable schema for sorter2 — the projection laid out as point-addressable
//! durable collections instead of one blob per node.
//!
//! A vote updates a handful of keys: a few edge-weight merges, a voted-pair flag,
//! a recent-vote deque push, and child-link set entries. The in-memory
//! [`crate::reducer::GroupState`] is reconstructed from these keys on read for
//! rank-centrality.

use std::collections::{BTreeSet, HashMap, HashSet};

use durable::{Batch, Db, Deque, Durable, Leaf, Map, Sum};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GroupState, NodeState, VoteData},
    storage_dto::{
        decode_entity_data, decode_vote, encode_entity_data, encode_vote, parse_stored_id,
        StoredEntityDataV1, StoredEntityRecord, StoredVoteV1,
    },
};

/// Directed edge key `(from_id, to_id)`.
pub type EdgeKey = (String, String);
/// Unordered voted-pair key, stored canonically as `(min, max)` by string.
pub type PairKey = (String, String);

/// One node in the fractal tree, exploded into precisely-updatable collections.
#[derive(Durable)]
#[allow(dead_code)]
pub struct NodeSchema {
    /// Presence marker (a node "exists" once ensured/voted/imported).
    pub present: Leaf<bool>,
    /// Domain-specific derived view (Reddit title/author/…); absent => None.
    pub data: Leaf<StoredEntityDataV1>,
    /// Child ids (a set; value is always `true`).
    pub children: Map<String, Leaf<bool>>,
    /// Directed edge weights `(from, to) -> weight`, updated by blind merges.
    pub edges: Map<EdgeKey, Sum<f64>>,
    /// Voted pairs `(min, max) -> true`.
    pub voted_pairs: Map<PairKey, Leaf<bool>>,
    /// Recent votes, newest at the front (capped on write).
    pub recent_votes: Deque<Leaf<StoredVoteV1>>,
}

/// The single database root: nodes, raw payloads, view counts, and per-concern
/// metadata maps (cursors and schema versions).
#[derive(Durable)]
#[allow(dead_code)]
pub struct Store {
    pub nodes: Map<String, NodeSchema>,
    pub proj_meta: Map<String, Leaf<u64>>,
    pub entities: Map<String, Leaf<StoredEntityRecord>>,
    pub entity_meta: Map<String, Leaf<u64>>,
    pub view_counts: Map<String, Leaf<u64>>,
    pub view_meta: Map<String, Leaf<u64>>,
}

/// Cap on the per-node recent-vote window (matches the in-memory reducer).
pub const RECENT_VOTES_CAP: u64 = 200;

fn id_key(id: &ItemId) -> String {
    id.as_str().to_string()
}

/// Path to a node by id.
pub fn node(id: &ItemId) -> durable::Path<NodeSchema> {
    Store::root().nodes().key(&id_key(id))
}

// ---------------------------------------------------------------------------
// Reconstruction (durable -> in-memory)
// ---------------------------------------------------------------------------

/// Reconstruct a node's in-memory state, or `None` if the node does not exist.
pub fn load_node_state(db: &Db, id: &ItemId) -> durable::Result<Option<NodeState>> {
    let np = node(id);
    let present = np.present().get(db)?.unwrap_or(false);

    let children_keys = np.children().keys(db)?;
    let voted = np.voted_pairs().keys(db)?;
    let edges_raw = np.edges().iter(db)?;
    let data = np.data().get(db)?;

    if !present
        && children_keys.is_empty()
        && voted.is_empty()
        && edges_raw.is_empty()
        && data.is_none()
    {
        return Ok(None);
    }

    let mut children = HashSet::new();
    for child in children_keys {
        children.insert(parse_storage_id(&child)?);
    }

    let local_ranking = build_group_state(db, &np, voted, edges_raw)?;

    Ok(Some(NodeState {
        id: id.clone(),
        data: data.map(decode_entity_data),
        children,
        local_ranking,
    }))
}

fn build_group_state(
    db: &Db,
    np: &durable::Path<NodeSchema>,
    voted: Vec<PairKey>,
    edges_raw: Vec<(EdgeKey, f64)>,
) -> durable::Result<GroupState> {
    // Item universe = every endpoint that appears in a voted pair or an edge.
    let mut item_strs: BTreeSet<String> = BTreeSet::new();
    for (a, b) in &voted {
        item_strs.insert(a.clone());
        item_strs.insert(b.clone());
    }
    for ((a, b), _) in &edges_raw {
        item_strs.insert(a.clone());
        item_strs.insert(b.clone());
    }

    let mut idx_to_item: Vec<ItemId> = Vec::with_capacity(item_strs.len());
    let mut item_to_idx: HashMap<ItemId, usize> = HashMap::with_capacity(item_strs.len());
    let mut str_to_idx: HashMap<String, usize> = HashMap::with_capacity(item_strs.len());
    for s in item_strs {
        let id = parse_storage_id(&s)?;
        let idx = idx_to_item.len();
        str_to_idx.insert(s, idx);
        item_to_idx.insert(id.clone(), idx);
        idx_to_item.push(id);
    }

    let mut edges: HashMap<(usize, usize), f64> = HashMap::with_capacity(edges_raw.len());
    for ((a, b), w) in edges_raw {
        if let (Some(&ai), Some(&bi)) = (str_to_idx.get(&a), str_to_idx.get(&b)) {
            edges.insert((ai, bi), w);
        }
    }

    let mut voted_pairs: HashSet<(usize, usize)> = HashSet::with_capacity(voted.len());
    for (a, b) in voted {
        if let (Some(&ai), Some(&bi)) = (str_to_idx.get(&a), str_to_idx.get(&b)) {
            let (i, j) = if ai < bi { (ai, bi) } else { (bi, ai) };
            voted_pairs.insert((i, j));
        }
    }

    // Deque is front=newest; in-memory VecDeque is also front=newest.
    let mut recent_votes = std::collections::VecDeque::new();
    for stored in np.recent_votes().iter(db)? {
        recent_votes.push_back(decode_vote(stored).map_err(durable::Error::Deserialize)?);
    }

    Ok(GroupState {
        item_to_idx,
        idx_to_item,
        edges,
        voted_pairs,
        recent_votes,
    })
}

fn parse_storage_id(s: &str) -> durable::Result<ItemId> {
    parse_stored_id(s).map_err(durable::Error::Deserialize)
}

// ---------------------------------------------------------------------------
// Write helpers (event -> reified point updates on a batch)
// ---------------------------------------------------------------------------

/// Wire a node and its ancestors into the tree exactly like
/// [`crate::reducer::GlobalTree::ensure_path`]: set presence and parent→child
/// links along the canonical breadcrumb path.
pub fn ensure_path_writes(batch: &mut Batch, id: &ItemId) {
    let root = ItemId::root();
    batch.write(node(&root).present().set(&true));
    if id.is_root() {
        return;
    }
    for path in id.breadcrumb_paths() {
        batch.write(node(&path).present().set(&true));
        match path.parent() {
            Some(parent) => {
                batch.write(node(&parent).present().set(&true));
                batch.write(node(&parent).children().key(&id_key(&path)).set(&true));
            }
            None => {
                batch.write(node(&root).children().key(&id_key(&path)).set(&true));
            }
        }
    }
}

/// Reified writes for a recorded vote under `parent`. Mirrors
/// [`crate::reducer::GroupState::apply_vote`] as point updates.
pub fn vote_writes(
    batch: &mut Batch,
    parent: &ItemId,
    a: &str,
    b: &str,
    ratio_left: i32,
    ratio_right: i32,
    ts: i64,
) -> durable::Result<()> {
    let left = ratio_left.max(0);
    let right = ratio_right.max(0);
    if left == 0 && right == 0 {
        return Ok(());
    }

    let a_id = ItemId::from_storage(a).unwrap_or_else(|| ItemId::opaque(a));
    let b_id = ItemId::from_storage(b).unwrap_or_else(|| ItemId::opaque(b));

    ensure_path_writes(batch, parent);
    ensure_path_writes(batch, &a_id);
    ensure_path_writes(batch, &b_id);

    let pnode = node(parent);
    batch.write(pnode.children().key(&id_key(&a_id)).set(&true));
    batch.write(pnode.children().key(&id_key(&b_id)).set(&true));

    // Edge weights: edge (b,a) += left, edge (a,b) += right (positive only).
    if left > 0 {
        batch.write(
            pnode
                .edges()
                .key(&(id_key(&b_id), id_key(&a_id)))
                .add(left as f64),
        );
    }
    if right > 0 {
        batch.write(
            pnode
                .edges()
                .key(&(id_key(&a_id), id_key(&b_id)))
                .add(right as f64),
        );
    }

    // Voted pair, canonicalized.
    let (lo, hi) = if id_key(&a_id) <= id_key(&b_id) {
        (id_key(&a_id), id_key(&b_id))
    } else {
        (id_key(&b_id), id_key(&a_id))
    };
    batch.write(pnode.voted_pairs().key(&(lo, hi)).set(&true));

    // Recent votes (newest at front).
    let stored = encode_vote(&VoteData {
        ts,
        a: a_id,
        b: b_id,
        ratio_left: left,
        ratio_right: right,
        body: String::new(),
        principal: "web".to_string(),
        delegate: None,
        thread_tag: "default".to_string(),
    });
    batch.push_front(&pnode.recent_votes(), &stored)?;
    Ok(())
}

/// Reified writes for an imported entity view (node data + path wiring).
pub fn entity_view_writes(batch: &mut Batch, id: &ItemId, view: Option<&EntityData>) {
    ensure_path_writes(batch, id);
    if let Some(view) = view {
        batch.write(node(id).data().set(&encode_entity_data(view)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vote_roundtrip_reconstructs_group_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let parent = ItemId::root();

        let mut batch = db.batch();
        vote_writes(&mut batch, &parent, "alpha", "beta", 2, 1, 1).unwrap();
        batch.commit().unwrap();

        let node_state = load_node_state(&db, &parent).unwrap().unwrap();
        let g = &node_state.local_ranking;
        assert_eq!(g.idx_to_item.len(), 2);
        let edge_total: f64 = g.edges.values().sum();
        assert_eq!(edge_total, 3.0);
        assert_eq!(g.recent_votes.len(), 1);
        assert!(node_state.children.contains(&ItemId::opaque("alpha")));
        assert!(node_state.children.contains(&ItemId::opaque("beta")));
    }

    #[test]
    fn zero_weight_vote_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let parent = ItemId::root();

        let mut batch = db.batch();
        vote_writes(&mut batch, &parent, "alpha", "beta", 0, 0, 1).unwrap();
        batch.commit().unwrap();

        assert!(load_node_state(&db, &parent).unwrap().is_none());
    }

    #[test]
    fn missing_node_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        assert!(load_node_state(&db, &ItemId::parse("nope").unwrap())
            .unwrap()
            .is_none());
    }
}
