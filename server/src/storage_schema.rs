//! Durable schema for sorter2 — the projection laid out as point-addressable
//! durable collections instead of one blob per node.
//!
//! Votes are stored as deduped `uuid_votes` entries plus an append-only
//! `recent_votes` audit list. Edge weights for rank centrality are derived
//! from `uuid_votes` on read, not stored in RocksDB.

use std::collections::HashSet;

use durable::{Batch, Db, Durable, Leaf, List, Map};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, NodeState, ScopeVotes, VoteData, UuidVoteKey, uuid_vote_key},
    storage_dto::{
        decode_entity_data, decode_vote, encode_entity_data, encode_vote, parse_stored_id,
        SessionDataV1, StoredEntityDataV1, StoredVoteV1,
    },
};

/// One node in the fractal tree, exploded into precisely-updatable collections.
#[derive(Durable)]
#[allow(dead_code)]
pub struct NodeSchema {
    pub present: Leaf<bool>,
    pub data: Leaf<StoredEntityDataV1>,
    pub children: Map<String, Leaf<bool>>,
    pub uuid_votes: Map<UuidVoteKey, Leaf<StoredVoteV1>>,
    pub recent_votes: List<Leaf<StoredVoteV1>>,
    pub fetched_at: Leaf<i64>,
}

#[derive(Durable)]
#[allow(dead_code)]
pub struct Store {
    pub nodes: Map<String, NodeSchema>,
    pub sessions: Map<String, Leaf<SessionDataV1>>,
    pub oauth_links: Map<String, Leaf<String>>,
    pub pseudonyms: Map<String, Leaf<String>>,
    pub user_pseudonyms: Map<String, List<Leaf<String>>>,
    pub user_weights: Map<String, Leaf<f64>>,
    pub proj_meta: Map<String, Leaf<u64>>,
    pub view_counts: Map<String, Leaf<u64>>,
    pub view_meta: Map<String, Leaf<u64>>,
}

pub fn encode_session(data: &SessionDataV1) -> SessionDataV1 {
    data.clone()
}

pub fn decode_session(data: SessionDataV1) -> SessionDataV1 {
    data
}

pub fn oauth_link_key(provider: &str, provider_id: &str) -> String {
    format!("{provider}:{provider_id}")
}

pub fn user_trust_weight(db: &Db, uuid: &str) -> durable::Result<f64> {
    Ok(Store::root()
        .user_weights()
        .key(&uuid.to_string())
        .get(db)?
        .unwrap_or(1.0))
}

pub fn load_session(db: &Db, session_id: &str) -> durable::Result<Option<SessionDataV1>> {
    Store::root()
        .sessions()
        .key(&session_id.to_string())
        .get(db)
}

pub fn write_session(batch: &mut Batch, session_id: &str, data: &SessionDataV1) {
    batch.write(
        Store::root()
            .sessions()
            .key(&session_id.to_string())
            .set(&encode_session(data)),
    );
}

pub fn delete_session(batch: &mut Batch, session_id: &str) {
    batch.write(
        Store::root()
            .sessions()
            .key(&session_id.to_string())
            .delete(),
    );
}

pub fn pseudonym_owner(db: &Db, pseudonym: &str) -> durable::Result<Option<String>> {
    Store::root()
        .pseudonyms()
        .key(&pseudonym.to_string())
        .get(db)
}

pub fn oauth_link_owner(db: &Db, provider: &str, provider_id: &str) -> durable::Result<Option<String>> {
    Store::root()
        .oauth_links()
        .key(&oauth_link_key(provider, provider_id))
        .get(db)
}

pub const RECENT_VOTES_CAP: u64 = 200;

fn id_key(id: &ItemId) -> String {
    id.as_str().to_string()
}

pub fn node(id: &ItemId) -> durable::Path<NodeSchema> {
    Store::root().nodes().key(&id_key(id))
}

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

    let votes = load_scope_votes(db, &np)?;

    Ok(Some(NodeState {
        id: id.clone(),
        data: data.map(decode_entity_data),
        children,
        votes,
    }))
}

fn load_scope_votes(db: &Db, np: &durable::Path<NodeSchema>) -> durable::Result<ScopeVotes> {
    let mut votes = ScopeVotes::default();

    for (key, stored) in np.uuid_votes().iter(db)? {
        let vote = decode_vote(stored).map_err(durable::Error::Deserialize)?;
        votes.uuid_votes.insert(key, vote);
    }

    let stored = np.recent_votes().iter(db)?;
    let cap = RECENT_VOTES_CAP as usize;
    let start = stored.len().saturating_sub(cap);
    votes.recent_votes = stored[start..]
        .iter()
        .map(|s| decode_vote(s.clone()).map_err(durable::Error::Deserialize))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(votes)
}

fn parse_storage_id(s: &str) -> durable::Result<ItemId> {
    parse_stored_id(s).map_err(durable::Error::Deserialize)
}

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

pub fn entity_content_writes(batch: &mut Batch, id: &ItemId, view: &EntityData, fetched_at: i64) {
    ensure_path_writes(batch, id);
    batch.write(node(id).data().set(&encode_entity_data(view)));
    batch.write(node(id).fetched_at().set(&fetched_at));
}

pub fn entity_content_clear_writes(batch: &mut Batch, id: &ItemId) {
    batch.write(node(id).data().delete());
    batch.write(node(id).fetched_at().delete());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{seed_default_pseudonym, DEFAULT_ACTOR_UUID, DEFAULT_PSEUDONYM};
    use crate::ranking::edge_weight_sum;

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
    fn vote_roundtrip_reconstructs_ranking() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        seed_default_pseudonym(&db).unwrap();
        let parent = ItemId::root();

        let vote = sample_vote(1, "alpha", "beta", 2, 1);
        let mut batch = db.batch();
        vote_writes(&mut batch, &parent, &vote, DEFAULT_ACTOR_UUID).unwrap();
        batch.commit().unwrap();

        let node_state = load_node_state(&db, &parent).unwrap().unwrap();
        assert_eq!(edge_weight_sum(&node_state.votes), 3.0);
        assert_eq!(node_state.votes.recent_votes.len(), 1);
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

        let votes = &load_node_state(&db, &parent).unwrap().unwrap().votes;
        assert_eq!(edge_weight_sum(votes), 1.0);
        assert_eq!(votes.uuid_votes.len(), 1);
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
        assert_eq!(node_state.votes.recent_votes.len(), RECENT_VOTES_CAP as usize);
        assert_eq!(node_state.votes.recent_votes.first().map(|v| v.ts), Some(10));
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
