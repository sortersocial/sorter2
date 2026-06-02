//! Durable page-view counters (path → count).
//!
//! Hot path is in-memory; a debounced background worker (started from
//! [`AppState`](crate::state::AppState)) batches dirty paths into RocksDB.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

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

struct MemState {
    counts: HashMap<String, u64>,
    dirty: HashSet<String>,
}

struct ViewStoreInner {
    db: Db,
    counts: DurableMap<String, u64>,
}

type MemStateLock = Arc<Mutex<MemState>>;

/// In-memory view counts with batched persistence to RocksDB.
#[derive(Clone)]
pub struct ViewStore {
    memory: MemStateLock,
    inner: Arc<Mutex<ViewStoreInner>>,
    flush_tx: mpsc::Sender<()>,
    /// Taken once by [`Self::spawn_flush_worker`] to run the debounced writer.
    flush_rx: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
}

impl ViewStore {
    /// Open (or create) view counts in `dir` (standalone store for tests).
    pub fn open(dir: &Path) -> Result<Self, ViewStoreError> {
        std::fs::create_dir_all(dir)?;
        let db = Db::open(dir)?;
        Self::from_db(&db)
    }

    /// Load counters from durable storage. Call [`Self::spawn_flush_worker`] at
    /// runtime startup to enable async batched writes.
    pub fn from_db(db: &Db) -> Result<Self, ViewStoreError> {
        let counts = DurableMap::new(db, "view_counts")?;
        let mut initial = HashMap::new();
        for item in counts.iter() {
            let (path, count) = item?;
            initial.insert(path, count);
        }

        let (flush_tx, flush_rx) = mpsc::channel(64);
        Ok(Self {
            memory: Arc::new(Mutex::new(MemState {
                counts: initial,
                dirty: HashSet::new(),
            })),
            inner: Arc::new(Mutex::new(ViewStoreInner {
                db: db.clone(),
                counts,
            })),
            flush_tx,
            flush_rx: Arc::new(Mutex::new(Some(flush_rx))),
        })
    }

    /// Start the debounced flush loop (requires a Tokio runtime).
    pub fn spawn_flush_worker(&self) {
        let Some(rx) = self.flush_rx.lock().ok().and_then(|mut g| g.take()) else {
            return;
        };
        let memory = self.memory.clone();
        let inner = self.inner.clone();
        tokio::spawn(async move {
            flush_worker_loop(rx, memory, inner).await;
        });
    }

    pub fn record_view(&self, path: String) -> Result<(), ViewStoreError> {
        let mut mem = self.memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        *mem.counts.entry(path.clone()).or_insert(0) += 1;
        mem.dirty.insert(path);
        drop(mem);
        let _ = self.flush_tx.try_send(());
        Ok(())
    }

    pub fn get_views_count(&self, path: &str) -> Result<u64, ViewStoreError> {
        let mem = self.memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        Ok(mem.counts.get(path).copied().unwrap_or(0))
    }

    /// Block until all dirty counters are written to RocksDB.
    pub fn flush(&self) -> Result<(), ViewStoreError> {
        flush_dirty(&self.memory, &self.inner)
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

async fn flush_worker_loop(
    mut flush_rx: mpsc::Receiver<()>,
    memory: MemStateLock,
    inner: Arc<Mutex<ViewStoreInner>>,
) {
    while flush_rx.recv().await.is_some() {
        while flush_rx.try_recv().is_ok() {}

        let memory = memory.clone();
        let inner = inner.clone();
        match tokio::task::spawn_blocking(move || flush_dirty(&memory, &inner)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(err = %e, "view count flush failed"),
            Err(e) => tracing::warn!(err = %e, "view flush task join failed"),
        }
    }
}

fn flush_dirty(
    memory: &MemStateLock,
    inner: &Arc<Mutex<ViewStoreInner>>,
) -> Result<(), ViewStoreError> {
    let snapshot: Vec<(String, u64)> = {
        let mut mem = memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        if mem.dirty.is_empty() {
            return Ok(());
        }
        let paths: Vec<String> = mem.dirty.drain().collect();
        paths
            .into_iter()
            .filter_map(|path| mem.counts.get(&path).map(|&count| (path, count)))
            .collect()
    };

    if snapshot.is_empty() {
        return Ok(());
    }

    let inner = inner.lock().map_err(|_| ViewStoreError::Poisoned)?;
    let mut batch = inner.db.batch();
    for (path, count) in &snapshot {
        inner.counts.put_in_batch(&mut batch, path, count)?;
    }
    batch.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn increment_and_read_from_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ViewStore::open(tmp.path()).unwrap();

        store.record_view("/vote".into()).unwrap();
        store.record_view("/vote".into()).unwrap();
        store.record_view("/".into()).unwrap();

        assert_eq!(store.get_views_count("/vote").unwrap(), 2);
        assert_eq!(store.get_views_count("/").unwrap(), 1);
        assert_eq!(store.get_views_count("/missing").unwrap(), 0);
    }

    #[test]
    fn batched_flush_persists_to_durable_map() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("store");
        {
            let store = ViewStore::open(&store_path).unwrap();
            store.record_view("/vote".into()).unwrap();
            store.record_view("/vote".into()).unwrap();
            store.flush().unwrap();
        }

        let store = ViewStore::open(&store_path).unwrap();
        assert_eq!(store.get_views_count("/vote").unwrap(), 2);
    }

    #[tokio::test]
    async fn background_flush_eventually_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("store");
        {
            let store = ViewStore::open(&store_path).unwrap();
            store.spawn_flush_worker();
            store.record_view("/".into()).unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            store.flush().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;

        let store = ViewStore::open(&store_path).unwrap();
        assert_eq!(store.get_views_count("/").unwrap(), 1);
    }
}
