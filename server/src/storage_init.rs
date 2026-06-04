//! Coordinated open/migration for rebuildable durable partitions.
//!
//! Entity payloads and the reducer projection share one [`durable::Db`] but keep
//! independent schema-version keys. Resetting only one partition on a version
//! mismatch can leave the projection cursor ahead of the JSONL tail while entity
//! payloads were wiped — replay then skips those events. Migrations here reset
//! the minimum set of partitions and always rewind the projection when entities
//! are rebuilt.

use std::path::Path;

use durable::Db;

use crate::{
    entity_store::{
        EntityStore, EntityStoreError, ENTITY_SCHEMA_KEY, ENTITY_SCHEMA_VERSION,
    },
    projection_store::{
        ProjectionStore, ProjectionStoreError, PROJECTION_SCHEMA_KEY,
        PROJECTION_SCHEMA_VERSION,
    },
    storage_schema::{Store, StoreFields},
};

/// Open the shared store database and ensure rebuildable partitions match their
/// schema versions.
pub fn open_store(
    dir: &Path,
) -> Result<(EntityStore, ProjectionStore), StorageInitError> {
    std::fs::create_dir_all(dir)?;
    let db = Db::open(dir)?;
    open_from_db(&db)
}

/// Ensure schema versions on an already-open database.
pub fn open_from_db(db: &Db) -> Result<(EntityStore, ProjectionStore), StorageInitError> {
    let entity_version = Store::root()
        .entity_meta()
        .key(&ENTITY_SCHEMA_KEY.to_string())
        .get(db)?;
    let projection_version = Store::root()
        .proj_meta()
        .key(&PROJECTION_SCHEMA_KEY.to_string())
        .get(db)?;

    let entity_ok = entity_version == Some(ENTITY_SCHEMA_VERSION);
    let projection_ok = projection_version == Some(PROJECTION_SCHEMA_VERSION);

    let entity_store = EntityStore::from_db(db)?;
    let projection_store = ProjectionStore::from_db(db)?;

    if entity_ok && projection_ok {
        return Ok((entity_store, projection_store));
    }

    if !entity_ok {
        tracing::warn!(
            stored = ?entity_version,
            expected = ENTITY_SCHEMA_VERSION,
            "entity store schema mismatch; clearing entities and projection"
        );
        entity_store.reset()?;
        projection_store.reset()?;
    } else if !projection_ok {
        tracing::warn!(
            stored = ?projection_version,
            expected = PROJECTION_SCHEMA_VERSION,
            "projection schema mismatch; clearing projection"
        );
        projection_store.reset()?;
    }

    Ok((entity_store, projection_store))
}

#[derive(Debug, thiserror::Error)]
pub enum StorageInitError {
    #[error("entity store error: {0}")]
    Entity(#[from] EntityStoreError),
    #[error("projection store error: {0}")]
    Projection(#[from] ProjectionStoreError),
    #[error("durable error: {0}")]
    Durable(#[from] durable::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        entity_store::EntityStore,
        events::{Event, EventRecord},
        path_types::ItemId,
        projection_apply,
        projection_store::ProjectionStore,
        storage_schema::{Store, StoreFields},
    };
    use durable::Db;
    use serde_json::json;

    #[test]
    fn entity_schema_mismatch_clears_projection_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();
        let entity_store = EntityStore::from_db(&db).unwrap();
        let event = Event::VoteRecorded {
            ts: 1,
            a: "alpha".into(),
            b: "beta".into(),
            ratio_left: 2,
            ratio_right: 1,
            scope: String::new(),
        };
        projection_apply::apply_records(
            &projection_store,
            &entity_store,
            &[EventRecord::new(1, 1, event)],
        )
        .unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);

        db.run(
            Store::root()
                .entity_meta()
                .key(&ENTITY_SCHEMA_KEY.to_string())
                .set(&1_u64),
            durable::Durability::SyncWal,
        )
        .unwrap();

        let (_entity, projection) = open_from_db(&db).unwrap();
        assert_eq!(projection.last_applied_event_count().unwrap(), 0);
    }

    #[test]
    fn projection_schema_mismatch_leaves_entities() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        let entity_store = EntityStore::from_db(&db).unwrap();
        let id = ItemId::parse("reddit.com/r/rust").unwrap();
        entity_store
            .put(&id, &json!({"data": {"display_name": "rust"}}))
            .unwrap();
        db.run(
            Store::root()
                .entity_meta()
                .key(&ENTITY_SCHEMA_KEY.to_string())
                .set(&ENTITY_SCHEMA_VERSION),
            durable::Durability::SyncWal,
        )
        .unwrap();

        db.run(
            Store::root()
                .proj_meta()
                .key(&PROJECTION_SCHEMA_KEY.to_string())
                .set(&1_u64),
            durable::Durability::SyncWal,
        )
        .unwrap();

        let (entity, projection) = open_from_db(&db).unwrap();
        assert_eq!(projection.last_applied_event_count().unwrap(), 0);
        assert!(entity.get(&id).unwrap().is_some());
    }
}
