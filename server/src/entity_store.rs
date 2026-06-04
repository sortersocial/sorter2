//! Off-heap storage for full entity payloads (Reddit API JSON).
//!
//! Derived [`crate::reducer::EntityData`] is stored on the node; the raw JSON
//! lives here, in the shared durable [`Store`] schema.

use std::path::Path;

use durable::{Batch, Db, Durability};
use serde_json::Value;

use crate::{
    path_types::ItemId,
    storage_dto::{decode_entity_payload, encode_entity_payload},
    storage_schema::{Store, StoreFields},
};

const ENTITY_SCHEMA_KEY: &str = "schema_version";
const ENTITY_SCHEMA_VERSION: u64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum EntityStoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage decode error: {0}")]
    Storage(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Disk-backed map of entity id → raw JSON payload.
#[derive(Clone)]
pub struct EntityStore {
    db: Db,
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
        let store = Self { db: db.clone() };
        let version = Store::root()
            .entity_meta()
            .key(&ENTITY_SCHEMA_KEY.to_string())
            .get(db)?;
        if version != Some(ENTITY_SCHEMA_VERSION) {
            store.reset()?;
        }
        Ok(store)
    }

    /// Clear rebuildable entity payloads and reset storage schema metadata.
    pub fn reset(&self) -> Result<(), EntityStoreError> {
        let root = Store::root();
        self.db.apply(
            &[root.entities().clear(), root.entity_meta().clear()],
            Durability::SyncWal,
        )?;
        self.db.run(
            root.entity_meta()
                .key(&ENTITY_SCHEMA_KEY.to_string())
                .set(&ENTITY_SCHEMA_VERSION),
            Durability::SyncWal,
        )?;
        Ok(())
    }

    /// Persist a payload for `id` (overwrites any existing entry).
    pub fn put(&self, id: &ItemId, payload: &Value) -> Result<(), EntityStoreError> {
        self.db.run(
            Store::root()
                .entities()
                .key(&id.as_str().to_string())
                .set(&encode_entity_payload(payload)),
            Durability::SyncWal,
        )?;
        Ok(())
    }

    /// Add a payload write to the caller's batch.
    pub fn put_in_batch(
        &self,
        batch: &mut Batch,
        id: &ItemId,
        payload: &Value,
    ) -> Result<(), EntityStoreError> {
        batch.write(
            Store::root()
                .entities()
                .key(&id.as_str().to_string())
                .set(&encode_entity_payload(payload)),
        );
        Ok(())
    }

    /// Load a stored payload, if present.
    pub fn get(&self, id: &ItemId) -> Result<Option<Value>, EntityStoreError> {
        match Store::root()
            .entities()
            .key(&id.as_str().to_string())
            .get(&self.db)?
        {
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
        let id = ItemId::from_url("https://reddit.com/r/rust").unwrap();
        let payload = json!({"kind": "t5", "data": {"display_name": "rust"}});

        store.put(&id, &payload).unwrap();
        let loaded = store.get(&id).unwrap().unwrap();
        assert_eq!(loaded, payload);
    }
}
