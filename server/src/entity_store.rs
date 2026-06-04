//! Off-heap storage for full entity payloads (Reddit API JSON).
//!
//! Derived [`crate::reducer::EntityData`] stays in the in-memory tree; raw JSON
//! lives in RocksDB via the workspace `durable` crate.

use std::path::Path;
use std::sync::{Arc, Mutex};

use durable::{Batch, Db, DurableMap};
use serde_json::Value;

use crate::{
    path_types::ItemId,
    storage_dto::{decode_entity_payload, encode_entity_payload, StoredEntityRecord},
};

#[derive(Debug, thiserror::Error)]
pub enum EntityStoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::DurableError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage decode error: {0}")]
    Storage(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store lock poisoned")]
    Poisoned,
}

struct EntityStoreInner {
    _db: Db,
    payloads: DurableMap<String, StoredEntityRecord>,
    meta: DurableMap<String, u64>,
}

const ENTITY_SCHEMA_META_KEY: &str = "entity_schema_version";
const ENTITY_SCHEMA_VERSION: u64 = 1;

/// Disk-backed map of entity id → raw JSON payload.
#[derive(Clone)]
pub struct EntityStore {
    inner: Arc<Mutex<EntityStoreInner>>,
}

impl EntityStore {
    /// Open (or create) the entity database under `dir`.
    pub fn open(dir: &Path) -> Result<Self, EntityStoreError> {
        std::fs::create_dir_all(dir)?;
        let db = Db::open(dir)?;
        Self::from_db(&db)
    }

    /// Create an entity store backed by an already-open database.
    pub fn from_db(db: &Db) -> Result<Self, EntityStoreError> {
        let mut payloads = DurableMap::new(db, "entity_payloads")?;
        let mut meta = DurableMap::new(db, "entity_meta")?;
        match meta.get(&ENTITY_SCHEMA_META_KEY.to_string())? {
            Some(ENTITY_SCHEMA_VERSION) => {}
            Some(_) | None => {
                payloads.clear()?;
                meta.clear()?;
                meta.put(ENTITY_SCHEMA_META_KEY.to_string(), ENTITY_SCHEMA_VERSION)?;
            }
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(EntityStoreInner {
                _db: db.clone(),
                payloads,
                meta,
            })),
        })
    }

    /// Clear rebuildable entity payloads and reset storage schema metadata.
    pub fn reset(&self) -> Result<(), EntityStoreError> {
        let mut inner = self.inner.lock().map_err(|_| EntityStoreError::Poisoned)?;
        inner.payloads.clear()?;
        inner.meta.clear()?;
        inner
            .meta
            .put(ENTITY_SCHEMA_META_KEY.to_string(), ENTITY_SCHEMA_VERSION)?;
        Ok(())
    }

    /// Persist a payload for `id` (overwrites any existing entry).
    pub fn put(&self, id: &ItemId, payload: &Value) -> Result<(), EntityStoreError> {
        let mut inner = self.inner.lock().map_err(|_| EntityStoreError::Poisoned)?;
        inner
            .payloads
            .put(id.as_str().to_string(), encode_entity_payload(payload))
            .map_err(EntityStoreError::from)
    }

    /// Add a payload write to the caller's RocksDB batch.
    pub fn put_in_batch(
        &self,
        batch: &mut Batch,
        id: &ItemId,
        payload: &Value,
    ) -> Result<(), EntityStoreError> {
        let inner = self.inner.lock().map_err(|_| EntityStoreError::Poisoned)?;
        inner
            .payloads
            .put_in_batch(
                batch,
                &id.as_str().to_string(),
                &encode_entity_payload(payload),
            )
            .map_err(EntityStoreError::from)
    }

    /// Load a stored payload, if present.
    pub fn get(&self, id: &ItemId) -> Result<Option<Value>, EntityStoreError> {
        let inner = self.inner.lock().map_err(|_| EntityStoreError::Poisoned)?;
        match inner.payloads.get(&id.as_str().to_string())? {
            Some(record) => decode_entity_payload(record)
                .map(Some)
                .map_err(EntityStoreError::Storage),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let store = EntityStore::open(tmp.path()).unwrap();
        let id = ItemId::parse("reddit.com/r/rust").unwrap();
        let payload = json!({"kind": "t5", "data": {"display_name": "rust"}});

        store.put(&id, &payload).unwrap();
        let loaded = store.get(&id).unwrap().unwrap();
        assert_eq!(loaded, payload);
    }
}
