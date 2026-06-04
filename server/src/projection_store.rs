//! Durable reducer projection.
//!
//! This is a rebuildable RocksDB-backed materialization of the in-memory
//! reducer. The JSONL event log remains the source of truth.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use durable::{Db, Durability, DurableMap};
use serde_json::Value;

use crate::{
    entity_store::EntityStore,
    events::Event,
    path_types::ItemId,
    reducer::{GlobalTree, NodeState},
    storage_dto::{decode_node, encode_node, StoredNodeRecord},
};

const META_LAST_APPLIED_EVENT_COUNT: &str = "last_applied_event_count";
const META_PROJECTION_SCHEMA_VERSION: &str = "projection_schema_version";
const PROJECTION_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionStoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::DurableError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage decode error: {0}")]
    Storage(String),
    #[error("projection lock poisoned")]
    Poisoned,
}

struct ProjectionStoreInner {
    _db: Db,
    nodes: DurableMap<String, StoredNodeRecord>,
    meta: DurableMap<String, u64>,
}

#[derive(Clone)]
pub struct ProjectionStore {
    inner: Arc<Mutex<ProjectionStoreInner>>,
}

impl ProjectionStore {
    pub fn open(dir: &std::path::Path) -> Result<Self, ProjectionStoreError> {
        std::fs::create_dir_all(dir)?;
        let db = Db::open(dir)?;
        Self::from_db(&db)
    }

    pub fn from_db(db: &Db) -> Result<Self, ProjectionStoreError> {
        let mut nodes = DurableMap::new(db, "nodes")?;
        let mut meta = DurableMap::new(db, "projection_meta")?;
        match meta.get(&META_PROJECTION_SCHEMA_VERSION.to_string())? {
            Some(PROJECTION_SCHEMA_VERSION) => {}
            Some(_) | None => {
                nodes.clear()?;
                meta.clear()?;
                meta.put(
                    META_PROJECTION_SCHEMA_VERSION.to_string(),
                    PROJECTION_SCHEMA_VERSION,
                )?;
            }
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(ProjectionStoreInner {
                _db: db.clone(),
                nodes,
                meta,
            })),
        })
    }

    pub fn last_applied_event_count(&self) -> Result<u64, ProjectionStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ProjectionStoreError::Poisoned)?;
        Ok(inner
            .meta
            .get(&META_LAST_APPLIED_EVENT_COUNT.to_string())?
            .unwrap_or(0))
    }

    pub fn load_tree(&self) -> Result<GlobalTree, ProjectionStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ProjectionStoreError::Poisoned)?;
        let mut tree = GlobalTree::default();
        for item in inner.nodes.iter() {
            let (_, node) = item?;
            tree.nodes.insert(node.id.clone(), node);
        }
        if tree.nodes.is_empty() {
            Ok(GlobalTree::new())
        } else {
            tree.ensure_node(&ItemId::root());
            Ok(tree)
        }
    }

    pub fn load_node(&self, id: &ItemId) -> Result<Option<NodeState>, ProjectionStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ProjectionStoreError::Poisoned)?;
        inner
            .nodes
            .get(&id.as_str().to_string())?
            .map(decode_node)
            .transpose()
            .map_err(ProjectionStoreError::Storage)
    }

    pub fn hydrate_scope(
        &self,
        tree: &mut GlobalTree,
        id: &ItemId,
    ) -> Result<(), ProjectionStoreError> {
        let Some(node) = self.load_node(id)? else {
            tree.ensure_path(id);
            return Ok(());
        };

        let children: Vec<ItemId> = node.children.iter().cloned().collect();
        tree.nodes.insert(node.id.clone(), node);

        for child in children {
            if let Some(child_node) = self.load_node(&child)? {
                tree.nodes.insert(child_node.id.clone(), child_node);
            } else {
                tree.ensure_node(&child);
            }
        }
        Ok(())
    }

    pub fn hydrate_event(
        &self,
        tree: &mut GlobalTree,
        event: &Event,
    ) -> Result<(), ProjectionStoreError> {
        for id in affected_nodes(event) {
            if tree.get(&id).is_none() {
                if let Some(node) = self.load_node(&id)? {
                    tree.nodes.insert(node.id.clone(), node);
                }
            }
        }
        Ok(())
    }

    pub fn scope_tree(&self, id: &ItemId) -> Result<GlobalTree, ProjectionStoreError> {
        let mut tree = GlobalTree::new();
        self.hydrate_scope(&mut tree, id)?;
        Ok(tree)
    }

    pub fn persist_event(
        &self,
        tree: &GlobalTree,
        event_count: u64,
        event: &Event,
    ) -> Result<(), ProjectionStoreError> {
        self.persist_batch(tree, event_count, affected_nodes(event), &[], None)
    }

    pub fn persist_next_event(
        &self,
        tree: &GlobalTree,
        event: &Event,
    ) -> Result<u64, ProjectionStoreError> {
        let event_count = self.last_applied_event_count()? + 1;
        self.persist_event(tree, event_count, event)?;
        Ok(event_count)
    }

    pub fn persist_batch(
        &self,
        tree: &GlobalTree,
        event_count: u64,
        ids: BTreeSet<ItemId>,
        entity_payloads: &[(ItemId, Value)],
        entity_store: Option<&EntityStore>,
    ) -> Result<(), ProjectionStoreError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ProjectionStoreError::Poisoned)?;
        let mut batch = inner._db.batch();

        for id in ids {
            if let Some(node) = tree.get(&id) {
                inner.nodes.put_in_batch(
                    &mut batch,
                    &id.as_str().to_string(),
                    &encode_node(node),
                )?;
            }
        }

        if !entity_payloads.is_empty() {
            let entity_store = entity_store.ok_or_else(|| {
                ProjectionStoreError::Storage("entity payloads require entity store".into())
            })?;
            for (id, payload) in entity_payloads {
                entity_store
                    .put_in_batch(&mut batch, id, payload)
                    .map_err(|e| {
                        ProjectionStoreError::Storage(format!("entity payload batch failed: {e}"))
                    })?;
            }
        }

        inner.meta.put_in_batch(
            &mut batch,
            &META_LAST_APPLIED_EVENT_COUNT.to_string(),
            &event_count,
        )?;
        batch.commit_with(Durability::DisableWal)?;
        Ok(())
    }
}

fn parent_from_event_scope(scope: &str) -> ItemId {
    if scope.contains('/') {
        ItemId::parse(scope).unwrap_or_else(|| ItemId::from_legacy_scope(scope))
    } else {
        ItemId::from_legacy_scope(scope)
    }
}

fn add_path_nodes(ids: &mut BTreeSet<ItemId>, id: &ItemId) {
    ids.insert(ItemId::root());
    ids.insert(id.clone());
    for path in id.breadcrumb_paths() {
        if let Some(parent) = path.parent() {
            ids.insert(parent);
        }
        ids.insert(path);
    }
}

pub(crate) fn affected_nodes(event: &Event) -> BTreeSet<ItemId> {
    let mut ids = BTreeSet::new();
    match event {
        Event::VoteRecorded { a, b, scope, .. } => {
            let parent = parent_from_event_scope(scope);
            add_path_nodes(&mut ids, &parent);
            if let Some(a) = ItemId::from_storage(a) {
                add_path_nodes(&mut ids, &a);
            }
            if let Some(b) = ItemId::from_storage(b) {
                add_path_nodes(&mut ids, &b);
            }
            ids.insert(parent);
        }
        Event::NodeEnsured { id } | Event::EntityImported { id, .. } => {
            if let Some(id) = ItemId::parse(id).or_else(|| ItemId::from_url(id)) {
                add_path_nodes(&mut ids, &id);
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::VoteData;

    #[test]
    fn persists_and_loads_reducer_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProjectionStore::open(tmp.path()).unwrap();
        let parent = ItemId::root();
        let vote = VoteData::from_recorded(1, "alpha", "beta", 2, 1).unwrap();
        let mut tree = GlobalTree::new();
        tree.apply_vote(&parent, vote);
        let event = Event::VoteRecorded {
            ts: 1,
            a: "alpha".into(),
            b: "beta".into(),
            ratio_left: 2,
            ratio_right: 1,
            scope: String::new(),
        };

        store.persist_event(&tree, 1, &event).unwrap();
        assert_eq!(store.last_applied_event_count().unwrap(), 1);

        let loaded = store.load_tree().unwrap();
        let root = loaded.get(&ItemId::root()).unwrap();
        assert_eq!(root.local_ranking.idx_to_item.len(), 2);
        assert!(root.children.contains(&ItemId::parse("alpha").unwrap()));
    }

    #[test]
    fn hydrates_scope_with_child_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProjectionStore::open(tmp.path()).unwrap();
        let parent = ItemId::root();
        let vote = VoteData::from_recorded(1, "alpha", "beta", 2, 1).unwrap();
        let mut tree = GlobalTree::new();
        tree.apply_vote(&parent, vote);
        let event = Event::VoteRecorded {
            ts: 1,
            a: "alpha".into(),
            b: "beta".into(),
            ratio_left: 2,
            ratio_right: 1,
            scope: String::new(),
        };
        store.persist_event(&tree, 1, &event).unwrap();

        let mut hydrated = GlobalTree::new();
        store.hydrate_scope(&mut hydrated, &ItemId::root()).unwrap();
        let root = hydrated.get(&ItemId::root()).unwrap();
        assert!(root.children.contains(&ItemId::parse("alpha").unwrap()));
        assert!(hydrated.get(&ItemId::parse("alpha").unwrap()).is_some());
    }

    #[test]
    fn scope_tree_is_request_local() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProjectionStore::open(tmp.path()).unwrap();
        let parent = ItemId::root();
        let vote = VoteData::from_recorded(1, "alpha", "beta", 2, 1).unwrap();
        let mut tree = GlobalTree::new();
        tree.apply_vote(&parent, vote);
        let event = Event::VoteRecorded {
            ts: 1,
            a: "alpha".into(),
            b: "beta".into(),
            ratio_left: 2,
            ratio_right: 1,
            scope: String::new(),
        };
        store.persist_event(&tree, 1, &event).unwrap();

        let scoped = store.scope_tree(&ItemId::root()).unwrap();
        assert_eq!(scoped.get(&ItemId::root()).unwrap().children.len(), 2);
        assert_eq!(store.load_tree().unwrap().nodes.len(), 3);
    }
}
