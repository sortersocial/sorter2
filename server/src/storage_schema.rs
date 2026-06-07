//! Durable schema for sorter2 — the projection laid out as point-addressable
//! durable collections instead of one blob per node.
//!
//! Votes are stored as deduped `uuid_votes` entries plus an append-only
//! `recent_votes` audit list. Edge weights for rank centrality are derived
//! from `uuid_votes` on read, not incrementally merged in RocksDB.

use std::collections::{HashSet};

use durable::{Batch, Db, Durable, Leaf, List, Map};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GroupState, NodeState, VoteData},
    storage_dto::{
        decode_entity_data, decode_vote, encode_entity_data, encode_vote, parse_stored_id,
        StoredEntityDataV1, StoredVoteV1,
    },
};

/// `(actor_uuid, min_item_id, max_item_id)` — one vote slot per human per pair.
pub type UuidVoteKey = (String, String, String);

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
    /// Latest vote per actor per unordered pair; edges are derived from this on read.
    pub uuid_votes: Map<UuidVoteKey, Leaf<StoredVoteV1>>,
    /// Recent votes, append-only oldest-first (cap applied on read).
    pub recent_votes: List<Leaf<StoredVoteV1>>,
    /// When ephemeral Reddit display content was last fetched (ms); absent after eviction.
    pub fetched_at: Leaf<i64>,
}

/// The single database root: nodes, identity maps, view counts, and metadata.
#[derive(Durable)]
#[allow(dead_code)]
pub struct Store {
    pub nodes: Map<String, NodeSchema>,
    /// Global pseudonym → actor UUID (Sybil dedup anchor).
    pub pseudonyms: Map<String, Leaf<String>>,
    pub proj_meta: Map<String, Leaf<u64>>,
    pub view_counts: Map<String, Leaf<u64>>,
    pub view_meta: Map<String, Leaf<u64>>,
}

/// Max recent votes returned when loading a node (query-time cap only).
pub const RECENT_VOTES_CAP: u64 = 200;

fn id_key(id: &ItemId) -> String {
    id.as_str().to_string()
}

fn pair_keys(a: &ItemId, b: &ItemId) -> (String, String) {
    let ak = id_key(a);
    let bk = id_key(b);
    if ak <= bk {
        (ak, bk)
    } else {
        (bk, ak)
    }
}

pub fn uuid_vote_key(actor_uuid: &str, a: &ItemId, b: &ItemId) -> UuidVoteKey {
    let (lo, hi) = pair_keys(a, b);
    (actor_uuid.to_string(), lo, hi)
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
    let uuid_vote_entries = np.uuid_votes().iter(db)?;
    let data = np.data().get(db)?;

    if !present && children_keys.is_empty() && uuid_vote_entries.is_empty() && data.is_none() {
        return Ok(None);
    }

    let mut children = HashSet::new();
    for child in children_keys {
        children.insert(parse_storage_id(&child)?);
    }

    let local_ranking = build_group_state(db, &np)?;

    Ok(Some(NodeState {
        id: id.clone(),
        data: data.map(decode_entity_data),
        children,
        local_ranking,
    }))
}

fn build_group_state(db: &Db, np: &durable::Path<NodeSchema>) -> durable::Result<GroupState> {
    let mut group = GroupState::new();

    for (key, stored) in np.uuid_votes().iter(db)? {
        let (actor_uuid, _lo, _hi) = key;
        let vote = decode_vote(stored).map_err(durable::Error::Deserialize)?;
        group.ingest_uuid_vote(vote, &actor_uuid);
    }

    let stored = np.recent_votes().iter(db)?;
    let cap = RECENT_VOTES_CAP as usize;
    let start = stored.len().saturating_sub(cap);
    group.recent_votes = stored[start..]
        .iter()
        .map(|s| decode_vote(s.clone()).map_err(durable::Error::Deserialize))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(group)
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

/// Reified writes for a validated vote under `parent`.
pub fn vote_writes(
    batch: &mut Batch,
    parent: &ItemId,
    vote: &VoteData,
    actor_uuid: &str,
) -> durable::Result<()> {
    ensure_path_writes(batch, parent);
    ensure_path_writes(batch, &vote.a);
    ensure_path_writes(batch, &vote.b);

    let pnode = node(parent);
    batch.write(pnode.children().key(&id_key(&vote.a)).set(&true));
    batch.write(pnode.children().key(&id_key(&vote.b)).set(&true));

    let key = uuid_vote_key(actor_uuid, &vote.a, &vote.b);
    batch.write(pnode.uuid_votes().key(&key).set(&encode_vote(vote)));
    batch.push(&pnode.recent_votes(), &encode_vote(vote))?;
    Ok(())
}

/// Reified writes for ephemeral Reddit display content (not event-logged).
pub fn entity_content_writes(batch: &mut Batch, id: &ItemId, view: &EntityData, fetched_at: i64) {
    ensure_path_writes(batch, id);
    batch.write(node(id).data().set(&encode_entity_data(view)));
    batch.write(node(id).fetched_at().set(&fetched_at));
}

/// Clear cached display content for one node (structure/votes are untouched).
pub fn entity_content_clear_writes(batch: &mut Batch, id: &ItemId) {
    batch.write(node(id).data().delete());
    batch.write(node(id).fetched_at().delete());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{seed_default_pseudonym, DEFAULT_ACTOR_UUID, DEFAULT_PSEUDONYM};

    fn sample_vote(ts: i64, a: &str, b: &str, l: i32, r: i32) -> VoteData {
        VoteData {
            ts,
            a: ItemId::opaque(a),
            b: ItemId::opaque(b),
            ratio_left: l,
            ratio_right: r,
            pseudonym: DEFAULT_PSEUDONYM.to_string(),
            trust_weight: 1.0,
        }
    }

    #[test]
    fn vote_roundtrip_reconstructs_group_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        seed_default_pseudonym(&db).unwrap();
        let parent = ItemId::root();

        let vote = sample_vote(1, "alpha", "beta", 2, 1);
        let mut batch = db.batch();
        vote_writes(&mut batch, &parent, &vote, DEFAULT_ACTOR_UUID).unwrap();
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
    fn uuid_vote_replace_updates_edges() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let parent = ItemId::root();
        let uuid = "u1";

        let mut batch = db.batch();
        vote_writes(
            &mut batch,
            &parent,
            &sample_vote(1, "alpha", "beta", 2, 1),
            uuid,
        )
        .unwrap();
        vote_writes(
            &mut batch,
            &parent,
            &VoteData {
                pseudonym: "alias2".into(),
                ratio_left: 0,
                ratio_right: 1,
                ..sample_vote(2, "alpha", "beta", 0, 1)
            },
            uuid,
        )
        .unwrap();
        batch.commit().unwrap();

        let g = &load_node_state(&db, &parent).unwrap().unwrap().local_ranking;
        let edge_total: f64 = g.edges.values().sum();
        assert_eq!(edge_total, 1.0);
        assert_eq!(g.uuid_votes.len(), 1);
    }

    #[test]
    fn load_caps_recent_votes_at_query_time() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let parent = ItemId::root();

        let mut batch = db.batch();
        for i in 0..RECENT_VOTES_CAP + 10 {
            vote_writes(
                &mut batch,
                &parent,
                &sample_vote(i as i64, "alpha", "beta", 1, 0),
                "u1",
            )
            .unwrap();
        }
        batch.commit().unwrap();

        assert_eq!(
            node(&parent).recent_votes().len(&db).unwrap(),
            RECENT_VOTES_CAP + 10
        );

        let node_state = load_node_state(&db, &parent).unwrap().unwrap();
        assert_eq!(node_state.local_ranking.recent_votes.len(), RECENT_VOTES_CAP as usize);
        assert_eq!(
            node_state
                .local_ranking
                .recent_votes
                .first()
                .map(|v| v.ts),
            Some(10)
        );
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
