use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GroupState, NodeState, VoteData},
};

pub const NODE_RECORD_VERSION: u32 = 1;
pub const ENTITY_RECORD_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versioned<T> {
    pub version: u32,
    pub payload: T,
}

impl<T> Versioned<T> {
    pub fn new(version: u32, payload: T) -> Self {
        Self { version, payload }
    }
}

pub type StoredNodeRecord = Versioned<StoredNodeV1>;
pub type StoredEntityRecord = Versioned<StoredEntityV1>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEntityV1 {
    pub json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredNodeV1 {
    pub id: String,
    pub data: Option<StoredEntityDataV1>,
    pub children: Vec<String>,
    pub local_ranking: StoredGroupStateV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEntityDataV1 {
    pub title: String,
    pub author: Option<String>,
    pub body_html: Option<String>,
    pub thumb_url: Option<String>,
    pub image_url: Option<String>,
    pub link_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroupStateV1 {
    pub items: Vec<String>,
    pub edges: Vec<StoredEdgeV1>,
    pub voted_pairs: Vec<StoredPairV1>,
    pub recent_votes: Vec<StoredVoteV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEdgeV1 {
    pub from: usize,
    pub to: usize,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct StoredPairV1 {
    pub left: usize,
    pub right: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVoteV1 {
    pub ts: i64,
    pub a: String,
    pub b: String,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub body: String,
    pub principal: String,
    pub delegate: Option<String>,
    pub thread_tag: String,
}

pub fn encode_node(node: &NodeState) -> StoredNodeRecord {
    let mut children: Vec<String> = node
        .children
        .iter()
        .map(|id| id.as_str().to_string())
        .collect();
    children.sort();

    Versioned::new(
        NODE_RECORD_VERSION,
        StoredNodeV1 {
            id: node.id.as_str().to_string(),
            data: node.data.as_ref().map(encode_entity_data),
            children,
            local_ranking: encode_group_state(&node.local_ranking),
        },
    )
}

pub fn decode_node(record: StoredNodeRecord) -> Result<NodeState, String> {
    if record.version != NODE_RECORD_VERSION {
        return Err(format!(
            "unsupported node record version: {}",
            record.version
        ));
    }
    let payload = record.payload;
    let id = parse_stored_id(&payload.id)?;
    let mut children = HashSet::new();
    for child in payload.children {
        children.insert(parse_stored_id(&child)?);
    }
    Ok(NodeState {
        id,
        data: payload.data.map(decode_entity_data),
        children,
        local_ranking: decode_group_state(payload.local_ranking)?,
    })
}

pub fn encode_entity_payload(payload: &Value) -> StoredEntityRecord {
    Versioned::new(
        ENTITY_RECORD_VERSION,
        StoredEntityV1 {
            json: payload.clone(),
        },
    )
}

pub fn decode_entity_payload(record: StoredEntityRecord) -> Result<Value, String> {
    if record.version != ENTITY_RECORD_VERSION {
        return Err(format!(
            "unsupported entity record version: {}",
            record.version
        ));
    }
    Ok(record.payload.json)
}

fn encode_entity_data(data: &EntityData) -> StoredEntityDataV1 {
    StoredEntityDataV1 {
        title: data.title.clone(),
        author: data.author.clone(),
        body_html: data.body_html.clone(),
        thumb_url: data.thumb_url.clone(),
        image_url: data.image_url.clone(),
        link_url: data.link_url.clone(),
    }
}

fn decode_entity_data(data: StoredEntityDataV1) -> EntityData {
    EntityData {
        title: data.title,
        author: data.author,
        body_html: data.body_html,
        thumb_url: data.thumb_url,
        image_url: data.image_url,
        link_url: data.link_url,
    }
}

fn encode_group_state(state: &GroupState) -> StoredGroupStateV1 {
    let mut edges: Vec<StoredEdgeV1> = state
        .edges
        .iter()
        .map(|(&(from, to), &weight)| StoredEdgeV1 { from, to, weight })
        .collect();
    edges.sort_by_key(|edge| (edge.from, edge.to));

    let mut voted_pairs: Vec<StoredPairV1> = state
        .voted_pairs
        .iter()
        .map(|&(left, right)| StoredPairV1 { left, right })
        .collect();
    voted_pairs.sort_by_key(|pair| (pair.left, pair.right));

    StoredGroupStateV1 {
        items: state
            .idx_to_item
            .iter()
            .map(|id| id.as_str().to_string())
            .collect(),
        edges,
        voted_pairs,
        recent_votes: state.recent_votes.iter().map(encode_vote).collect(),
    }
}

fn decode_group_state(state: StoredGroupStateV1) -> Result<GroupState, String> {
    let mut idx_to_item = Vec::with_capacity(state.items.len());
    let mut item_to_idx = HashMap::new();
    for (idx, item) in state.items.iter().enumerate() {
        let id = parse_stored_id(item)?;
        item_to_idx.insert(id.clone(), idx);
        idx_to_item.push(id);
    }

    let mut edges = HashMap::new();
    for edge in state.edges {
        edges.insert((edge.from, edge.to), edge.weight);
    }

    let mut voted_pairs = HashSet::new();
    for pair in state.voted_pairs {
        voted_pairs.insert((pair.left, pair.right));
    }

    let mut recent_votes = VecDeque::with_capacity(state.recent_votes.len().min(200));
    for vote in state.recent_votes {
        recent_votes.push_back(decode_vote(vote)?);
    }

    Ok(GroupState {
        item_to_idx,
        idx_to_item,
        edges,
        voted_pairs,
        recent_votes,
    })
}

fn encode_vote(vote: &VoteData) -> StoredVoteV1 {
    StoredVoteV1 {
        ts: vote.ts,
        a: vote.a.as_str().to_string(),
        b: vote.b.as_str().to_string(),
        ratio_left: vote.ratio_left,
        ratio_right: vote.ratio_right,
        body: vote.body.clone(),
        principal: vote.principal.clone(),
        delegate: vote.delegate.clone(),
        thread_tag: vote.thread_tag.clone(),
    }
}

fn decode_vote(vote: StoredVoteV1) -> Result<VoteData, String> {
    Ok(VoteData {
        ts: vote.ts,
        a: parse_stored_id(&vote.a)?,
        b: parse_stored_id(&vote.b)?,
        ratio_left: vote.ratio_left,
        ratio_right: vote.ratio_right,
        body: vote.body,
        principal: vote.principal,
        delegate: vote.delegate,
        thread_tag: vote.thread_tag,
    })
}

fn parse_stored_id(s: &str) -> Result<ItemId, String> {
    if s.is_empty() {
        return Ok(ItemId::root());
    }
    ItemId::from_storage(s)
        .or_else(|| ItemId::parse(s))
        .ok_or_else(|| format!("invalid stored item id: {s}"))
}
