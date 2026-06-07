//! Versioned leaf value DTOs persisted in durable collections.
//!
//! Node structure (children, edges, voted pairs, recent votes) is no longer a
//! single blob — it lives as point-addressable durable collections (see
//! [`crate::storage_schema`]). This module only defines the small leaf values:
//! ephemeral entity views and individual votes.

use serde::{Deserialize, Serialize};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, VoteData},
};

pub const VOTE_RECORD_VERSION: u32 = 1;
pub const ENTITY_DATA_VERSION: u32 = 1;

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

/// Derived entity view stored at a node's `data` leaf (ephemeral; not logged).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEntityDataV1 {
    pub version: u32,
    pub title: String,
    pub author: Option<String>,
    pub body_html: Option<String>,
    pub thumb_url: Option<String>,
    pub image_url: Option<String>,
    pub link_url: Option<String>,
}

/// One vote stored in a node's `recent_votes` deque.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVoteV1 {
    pub version: u32,
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

pub fn encode_entity_data(data: &EntityData) -> StoredEntityDataV1 {
    StoredEntityDataV1 {
        version: ENTITY_DATA_VERSION,
        title: data.title.clone(),
        author: data.author.clone(),
        body_html: data.body_html.clone(),
        thumb_url: data.thumb_url.clone(),
        image_url: data.image_url.clone(),
        link_url: data.link_url.clone(),
    }
}

pub fn decode_entity_data(data: StoredEntityDataV1) -> EntityData {
    EntityData {
        title: data.title,
        author: data.author,
        body_html: data.body_html,
        thumb_url: data.thumb_url,
        image_url: data.image_url,
        link_url: data.link_url,
    }
}

pub fn encode_vote(vote: &VoteData) -> StoredVoteV1 {
    StoredVoteV1 {
        version: VOTE_RECORD_VERSION,
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

pub fn decode_vote(vote: StoredVoteV1) -> Result<VoteData, String> {
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

pub fn parse_stored_id(s: &str) -> Result<ItemId, String> {
    if s.is_empty() {
        return Ok(ItemId::root());
    }
    ItemId::from_storage(s)
        .or_else(|| ItemId::parse(s))
        .ok_or_else(|| format!("invalid stored item id: {s}"))
}
