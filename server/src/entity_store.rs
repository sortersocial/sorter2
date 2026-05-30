//! Off-heap storage for full entity payloads (Reddit API JSON).
//!
//! Derived [`crate::reducer::EntityData`] stays in the in-memory tree; raw JSON
//! lives in RocksDB via the workspace `durable` crate.

use std::path::Path;
use std::sync::{Arc, Mutex};

use durable::{Db, DurableMap};
use serde_json::Value;

use crate::path_types::ItemId;

#[derive(Debug, thiserror::Error)]
pub enum EntityStoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::DurableError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store lock poisoned")]
    Poisoned,
}

struct EntityStoreInner {
    _db: Db,
    payloads: DurableMap<String, String>,
}

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
        let payloads = DurableMap::new(&db, "entity_payloads")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(EntityStoreInner { _db: db, payloads })),
        })
    }

    /// Persist a payload for `id` (overwrites any existing entry).
    pub fn put(&self, id: &ItemId, payload: &Value) -> Result<(), EntityStoreError> {
        let json = serde_json::to_string(payload)?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| EntityStoreError::Poisoned)?;
        inner
            .payloads
            .put(id.as_str().to_string(), json)
            .map_err(EntityStoreError::from)
    }

    /// Load a stored payload, if present.
    pub fn get(&self, id: &ItemId) -> Result<Option<Value>, EntityStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| EntityStoreError::Poisoned)?;
        match inner.payloads.get(&id.as_str().to_string())? {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
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
