//! Durable reducer projection.
//!
//! A rebuildable, point-addressable RocksDB materialization of the in-memory
//! reducer (see [`crate::storage_schema`]). The JSONL event log remains the
//! source of truth. Reads reconstruct an in-memory [`GlobalTree`] scope on
//! demand; writes are precise point updates (no blob rewrite).

use durable::{Db, Durability, Write};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GlobalTree, NodeState},
    storage_schema::{
        entity_content_clear_writes, entity_content_writes, load_node_state, node, NodeSchemaFields,
        Store, StoreFields,
    },
};

const PROJECTION_CURSOR_KEY: &str = "cursor";
const PROJECTION_SCHEMA_KEY: &str = "schema_version";
const PROJECTION_SCHEMA_VERSION: u64 = 6;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionStoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage decode error: {0}")]
    Storage(String),
}

#[derive(Clone)]
pub struct ProjectionStore {
    db: Db,
}

impl ProjectionStore {
    pub fn open(dir: &std::path::Path) -> Result<Self, ProjectionStoreError> {
        std::fs::create_dir_all(dir)?;
        let db = Db::open(dir)?;
        Self::from_db(&db)
    }

    pub fn from_db(db: &Db) -> Result<Self, ProjectionStoreError> {
        let store = Self { db: db.clone() };
        let version = Store::root()
            .proj_meta()
            .key(&PROJECTION_SCHEMA_KEY.to_string())
            .get(db)?;
        if version != Some(PROJECTION_SCHEMA_VERSION) {
            store.reset()?;
        }
        crate::identity::seed_default_pseudonym(db)
            .map_err(ProjectionStoreError::Durable)?;
        Ok(store)
    }

    pub(crate) fn db(&self) -> &Db {
        &self.db
    }

    pub fn last_applied_event_count(&self) -> Result<u64, ProjectionStoreError> {
        Ok(Store::root()
            .proj_meta()
            .key(&PROJECTION_CURSOR_KEY.to_string())
            .get(&self.db)?
            .unwrap_or(0))
    }

    /// A reified write that advances the projection cursor.
    pub(crate) fn cursor_write(&self, seq: u64) -> Write {
        Store::root()
            .proj_meta()
            .key(&PROJECTION_CURSOR_KEY.to_string())
            .set(&seq)
    }

    /// Clear rebuildable projection data and reset storage schema metadata.
    pub fn reset(&self) -> Result<(), ProjectionStoreError> {
        let root = Store::root();
        self.db.apply(
            &[root.nodes().clear(), root.proj_meta().clear()],
            Durability::SyncWal,
        )?;
        self.db.run(
            root.proj_meta()
                .key(&PROJECTION_SCHEMA_KEY.to_string())
                .set(&PROJECTION_SCHEMA_VERSION),
            Durability::SyncWal,
        )?;
        Ok(())
    }

    pub fn load_node(&self, id: &ItemId) -> Result<Option<NodeState>, ProjectionStoreError> {
        Ok(load_node_state(&self.db, id)?)
    }

    /// Load the full tree (every node). Used for offline checks/tests.
    pub fn load_tree(&self) -> Result<GlobalTree, ProjectionStoreError> {
        let keys = Store::root().nodes().keys(&self.db)?;
        let mut tree = GlobalTree::default();
        for key in keys {
            let id = parse_node_key(&key)?;
            if let Some(node_state) = self.load_node(&id)? {
                tree.nodes.insert(id, node_state);
            }
        }
        if tree.nodes.is_empty() {
            Ok(GlobalTree::new())
        } else {
            tree.ensure_node(&ItemId::root());
            Ok(tree)
        }
    }

    /// Load `id` plus its direct children into `tree` (the read scope).
    pub fn hydrate_scope(
        &self,
        tree: &mut GlobalTree,
        id: &ItemId,
    ) -> Result<(), ProjectionStoreError> {
        let Some(node_state) = self.load_node(id)? else {
            tree.ensure_path(id);
            return Ok(());
        };

        let children: Vec<ItemId> = node_state.children.iter().cloned().collect();
        tree.nodes.insert(node_state.id.clone(), node_state);

        for child in children {
            if let Some(child_node) = self.load_node(&child)? {
                tree.nodes.insert(child_node.id.clone(), child_node);
            } else {
                tree.ensure_node(&child);
            }
        }
        Ok(())
    }

    pub fn scope_tree(&self, id: &ItemId) -> Result<GlobalTree, ProjectionStoreError> {
        let mut tree = GlobalTree::new();
        self.hydrate_scope(&mut tree, id)?;
        Ok(tree)
    }

    /// Cache Reddit display content outside the event log (must be evicted per policy).
    pub fn put_ephemeral_content(
        &self,
        id: &ItemId,
        view: &EntityData,
        fetched_at: i64,
    ) -> Result<(), ProjectionStoreError> {
        let mut batch = self.db.batch();
        entity_content_writes(&mut batch, id, view, fetched_at);
        batch
            .commit_with(Durability::DisableWal)
            .map_err(ProjectionStoreError::from)?;
        Ok(())
    }

    /// Drop cached display content older than `cutoff_ms` (votes and tree structure remain).
    pub fn evict_content_older_than(&self, cutoff_ms: i64) -> Result<usize, ProjectionStoreError> {
        let keys = Store::root().nodes().keys(&self.db)?;
        let mut batch = self.db.batch();
        let mut evicted = 0usize;
        for key in keys {
            let id = parse_node_key(&key)?;
            let np = node(&id);
            let Some(fetched_at) = np.fetched_at().get(&self.db)? else {
                continue;
            };
            if fetched_at > 0 && fetched_at < cutoff_ms {
                entity_content_clear_writes(&mut batch, &id);
                evicted += 1;
            }
        }
        if evicted > 0 {
            batch
                .commit_with(Durability::DisableWal)
                .map_err(ProjectionStoreError::from)?;
        }
        Ok(evicted)
    }
}

fn parse_node_key(key: &str) -> Result<ItemId, ProjectionStoreError> {
    if key.is_empty() {
        return Ok(ItemId::root());
    }
    ItemId::from_storage(key)
        .or_else(|| ItemId::parse(key))
        .ok_or_else(|| ProjectionStoreError::Storage(format!("invalid node key: {key}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{events::Event, projection_apply, reducer::EntityData};

    fn record(seq: u64, event: Event) -> crate::events::EventRecord {
        crate::events::EventRecord::new(seq, crate::events::event_timestamp(&event), event)
    }

    #[test]
    fn applies_and_loads_reducer_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        let store = ProjectionStore::from_db(&db).unwrap();

        let event = Event::vote_recorded(1, "alpha", "beta", 2, 1, "");
        projection_apply::apply_records(&store, &[record(1, event)]).unwrap();
        assert_eq!(store.last_applied_event_count().unwrap(), 1);

        let loaded = store.load_tree().unwrap();
        let root = loaded.get(&ItemId::root()).unwrap();
        assert_eq!(crate::ranking::ranked_items(&root.votes).len(), 2);
        assert!(root.children.contains(&ItemId::opaque("alpha")));
    }

    #[test]
    fn hydrates_scope_with_child_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        let store = ProjectionStore::from_db(&db).unwrap();

        let event = Event::vote_recorded(1, "alpha", "beta", 2, 1, "");
        projection_apply::apply_records(&store, &[record(1, event)]).unwrap();

        let scoped = store.scope_tree(&ItemId::root()).unwrap();
        let root = scoped.get(&ItemId::root()).unwrap();
        assert_eq!(root.children.len(), 2);
        assert!(scoped.get(&ItemId::opaque("alpha")).is_some());
    }

    #[test]
    fn evicts_stale_ephemeral_content() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        let store = ProjectionStore::from_db(&db).unwrap();
        let id = ItemId::from_url("https://reddit.com/r/rust").unwrap();
        store
            .put_ephemeral_content(
                &id,
                &EntityData {
                    title: "Rust".into(),
                    author: None,
                    body_html: None,
                    thumb_url: None,
                    image_url: None,
                    link_url: None,
                },
                1_000,
            )
            .unwrap();
        assert!(store.load_node(&id).unwrap().unwrap().data.is_some());
        assert_eq!(store.evict_content_older_than(2_000).unwrap(), 1);
        assert!(store.load_node(&id).unwrap().unwrap().data.is_none());
    }
}
