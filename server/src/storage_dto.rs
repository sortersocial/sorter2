//! Versioned leaf value DTOs persisted in durable collections.
//!
//! Node structure (children, uuid votes, recent votes) lives as point-addressable
//! durable collections (see [`crate::storage_schema`]). This module defines the
//! small leaf values: ephemeral entity views and individual votes.

use serde::{Deserialize, Serialize};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, VoteData},
};

pub const VOTE_RECORD_VERSION: u32 = 2;
pub const ENTITY_DATA_VERSION: u32 = 1;
pub const SESSION_DATA_VERSION: u32 = 1;

/// Browser session stored in durable (operational; not event-logged).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDataV1 {
    pub version: u32,
    pub uuid: String,
    pub current_pseudonym: String,
    pub expires_at: i64,
}

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
    #[serde(default)]
    pub over_18: bool,
    pub thumb_url: Option<String>,
    pub image_url: Option<String>,
    pub link_url: Option<String>,
}

/// One vote stored in a node's `recent_votes` list or `uuid_votes` map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVoteV1 {
    pub version: u32,
    pub ts: i64,
    pub a: String,
    pub b: String,
    pub ratio_left: i32,
    pub ratio_right: i32,
    pub pseudonym: String,
    pub trust_weight: f64,
}

pub fn encode_entity_data(data: &EntityData) -> StoredEntityDataV1 {
    StoredEntityDataV1 {
        version: ENTITY_DATA_VERSION,
        title: data.title.clone(),
        author: data.author.clone(),
        body_html: data.body_html.clone(),
        over_18: data.over_18,
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
        over_18: data.over_18,
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
        pseudonym: vote.pseudonym.clone(),
        trust_weight: vote.trust_weight,
    }
}

pub fn decode_vote(vote: StoredVoteV1) -> Result<VoteData, String> {
    Ok(VoteData {
        ts: vote.ts,
        a: parse_stored_id(&vote.a)?,
        b: parse_stored_id(&vote.b)?,
        ratio_left: vote.ratio_left,
        ratio_right: vote.ratio_right,
        pseudonym: vote.pseudonym,
        trust_weight: vote.trust_weight,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_data_roundtrip_preserves_over_18() {
        let data = EntityData {
            title: "adult post".into(),
            author: Some("alice".into()),
            body_html: Some("<p>body</p>".into()),
            over_18: true,
            thumb_url: Some("https://example.com/thumb.jpg".into()),
            image_url: Some("https://example.com/image.jpg".into()),
            link_url: Some("https://example.com/out".into()),
        };

        let decoded = decode_entity_data(encode_entity_data(&data));
        assert!(decoded.over_18);
        assert_eq!(decoded.title, data.title);
        assert_eq!(decoded.image_url, data.image_url);
    }
}
