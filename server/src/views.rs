//! Durable page-view counters (path → count).
//!
//! Backed by RocksDB via the workspace `durable` crate, sharing the app store DB
//! with [`crate::entity_store::EntityStore`] and [`crate::projection_store::ProjectionStore`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use durable::{Db, DurableMap};

#[derive(Debug, thiserror::Error)]
pub enum ViewStoreError {
    #[error("durable error: {0}")]
    Durable(#[from] durable::DurableError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store lock poisoned")]
    Poisoned,
}

struct ViewStoreInner {
    _db: Db,
    counts: DurableMap<String, u64>,
}

/// Disk-backed map of request path → view count.
#[derive(Clone)]
pub struct ViewStore {
    inner: Arc<Mutex<ViewStoreInner>>,
}

impl ViewStore {
    /// Open (or create) view counts in `dir` (standalone store for tests).
    pub fn open(dir: &Path) -> Result<Self, ViewStoreError> {
        std::fs::create_dir_all(dir)?;
        let db = Db::open(dir)?;
        Self::from_db(&db)
    }

    /// Attach view counts to an already-open database.
    pub fn from_db(db: &Db) -> Result<Self, ViewStoreError> {
        let counts = DurableMap::new(db, "view_counts")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(ViewStoreInner {
                _db: db.clone(),
                counts,
            })),
        })
    }

    pub fn record_view(&self, path: String) -> Result<(), ViewStoreError> {
        let mut inner = self.inner.lock().map_err(|_| ViewStoreError::Poisoned)?;
        let current = inner.counts.get(&path)?.unwrap_or(0);
        inner.counts.put(path, current + 1)?;
        Ok(())
    }

    pub fn get_views_count(&self, path: &str) -> Result<u64, ViewStoreError> {
        let inner = self.inner.lock().map_err(|_| ViewStoreError::Poisoned)?;
        Ok(inner.counts.get(&path.to_string())?.unwrap_or(0))
    }

    /// Best-effort increment for request handlers (logs failures).
    pub fn increment(&self, path: String) {
        if let Err(e) = self.record_view(path) {
            tracing::warn!(err = %e, "view increment failed");
        }
    }

    /// Read counter for display (returns 0 on store errors).
    pub fn get_views(&self, path: &str) -> u64 {
        self.get_views_count(path).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_and_read_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ViewStore::open(tmp.path()).unwrap();

        store.record_view("/vote".into()).unwrap();
        store.record_view("/vote".into()).unwrap();
        store.record_view("/".into()).unwrap();

        assert_eq!(store.get_views_count("/vote").unwrap(), 2);
        assert_eq!(store.get_views_count("/").unwrap(), 1);
        assert_eq!(store.get_views_count("/missing").unwrap(), 0);
    }

}
