//! Coupled open/reset for rebuildable RocksDB stores (entity payloads + projection).
//!
//! Entity and projection data must stay in sync. A single `store_schema_version`
//! meta key drives reset: on mismatch both stores are cleared together so boot
//! catch-up replays the full JSONL log instead of a partial tail.

use durable::{Db, Durability, DurableMap};

use crate::{entity_store::EntityStore, projection_store::ProjectionStore};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::DurableError),
    #[error("entity store error: {0}")]
    Entity(#[from] crate::entity_store::EntityStoreError),
    #[error("projection store error: {0}")]
    Projection(#[from] crate::projection_store::ProjectionStoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

const META_STORE_SCHEMA_VERSION: &str = "store_schema_version";
pub const STORE_SCHEMA_VERSION: u64 = 1;

/// Rebuildable entity + projection stores sharing one RocksDB file.
pub struct RebuildableStores {
    pub entity: EntityStore,
    pub projection: ProjectionStore,
}

/// Open rebuildable stores, resetting both when the coupled schema version mismatches.
pub fn open(db: &Db) -> Result<RebuildableStores, StoreError> {
    let store_meta = DurableMap::<String, u64>::new(db, "store_meta")?;
    let schema_ok = matches!(
        store_meta.get(&META_STORE_SCHEMA_VERSION.to_string())?,
        Some(STORE_SCHEMA_VERSION)
    );

    if !schema_ok {
        reset_rebuildable_data(db)?;
        let mut batch = db.batch();
        store_meta.put_in_batch(
            &mut batch,
            &META_STORE_SCHEMA_VERSION.to_string(),
            &STORE_SCHEMA_VERSION,
        )?;
        batch.commit_with(Durability::SyncWal)?;
    }

    Ok(RebuildableStores {
        entity: EntityStore::from_db(db)?,
        projection: ProjectionStore::from_db(db)?,
    })
}

/// Clear rebuildable projection + entity data and reset coupled schema metadata.
pub fn reset(db: &Db) -> Result<(), StoreError> {
    reset_rebuildable_data(db)?;
    let mut store_meta = DurableMap::new(db, "store_meta")?;
    store_meta.clear()?;
    store_meta.put(
        META_STORE_SCHEMA_VERSION.to_string(),
        STORE_SCHEMA_VERSION,
    )?;
    Ok(())
}

fn reset_rebuildable_data(db: &Db) -> Result<(), StoreError> {
    EntityStore::clear_data(db)?;
    ProjectionStore::clear_data(db)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::{Event, EventRecord},
        path_types::ItemId,
        projection_apply,
    };
    use serde_json::json;

    fn event_record(seq: u64, event: Event) -> EventRecord {
        EventRecord::new(seq, crate::events::event_timestamp(&event), event)
    }

    #[test]
    fn schema_mismatch_resets_both_stores_and_replay_restores_entity_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let stores = open(&db).unwrap();
        let payload = json!({"kind":"t5","data":{"display_name":"rust"}});
        projection_apply::apply_records(
            &stores.projection,
            &stores.entity,
            &[event_record(
                1,
                Event::EntityImported {
                    id: "reddit.com/r/rust".into(),
                    ts: 1,
                    payload: payload.clone(),
                },
            )],
        )
        .unwrap();
        assert!(stores
            .entity
            .get(&ItemId::parse("reddit.com/r/rust").unwrap())
            .unwrap()
            .is_some());
        assert_eq!(stores.projection.last_applied_event_count().unwrap(), 1);

        // Simulate a coupled schema bump on next boot.
        let mut store_meta = durable::DurableMap::new(&db, "store_meta").unwrap();
        store_meta
            .put("store_schema_version".to_string(), 99_u64)
            .unwrap();

        let stores = open(&db).unwrap();
        assert_eq!(stores.projection.last_applied_event_count().unwrap(), 0);
        assert!(stores
            .entity
            .get(&ItemId::parse("reddit.com/r/rust").unwrap())
            .unwrap()
            .is_none());
    }
}
