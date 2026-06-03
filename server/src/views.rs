//! Page-view analytics: single-writer actor, batched `views.jsonl`, materialized counters in RocksDB.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use durable::{Db, DurableMap};

use crate::{
    events::{ViewEvent, ViewRecord},
    fetch::now_ms,
    view_log::{ViewLog, ViewLogError},
};

const META_LAST_APPLIED_SEQ: &str = "last_applied_view_seq";
const SYNC_EVERY_BATCHES: u64 = 20;

#[derive(Debug, thiserror::Error)]
pub enum ViewStoreError {
    #[error("view log error: {0}")]
    Log(#[from] ViewLogError),
    #[error("durable error: {0}")]
    Durable(#[from] durable::DurableError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store lock poisoned")]
    Poisoned,
}

struct MemState {
    counts: HashMap<String, u64>,
    dirty_paths: HashSet<String>,
    meta_dirty: bool,
    last_applied_seq: u64,
}

struct ViewStoreInner {
    db: Db,
    counts: DurableMap<String, u64>,
    meta: DurableMap<String, u64>,
}

struct ViewCommand {
    path: String,
    ts: i64,
}

type MemStateLock = Arc<Mutex<MemState>>;

/// Client handle for the single view writer actor.
#[derive(Clone)]
pub struct ViewStore {
    memory: MemStateLock,
    inner: Arc<Mutex<ViewStoreInner>>,
    record_tx: mpsc::Sender<ViewCommand>,
    record_rx: Arc<Mutex<Option<mpsc::Receiver<ViewCommand>>>>,
}

impl ViewStore {
    pub fn open(dir: &Path) -> Result<Self, ViewStoreError> {
        std::fs::create_dir_all(dir)?;
        let db = Db::open(dir)?;
        Self::from_db(&db)
    }

    pub fn from_db(db: &Db) -> Result<Self, ViewStoreError> {
        let counts = DurableMap::new(db, "view_counts")?;
        let meta = DurableMap::new(db, "view_meta")?;
        let mut initial = HashMap::new();
        for item in counts.iter() {
            let (path, count) = item?;
            initial.insert(path, count);
        }
        let last_applied_seq = meta
            .get(&META_LAST_APPLIED_SEQ.to_string())?
            .unwrap_or(0);

        let (record_tx, record_rx) = mpsc::channel(4096);
        Ok(Self {
            memory: Arc::new(Mutex::new(MemState {
                counts: initial,
                dirty_paths: HashSet::new(),
                meta_dirty: false,
                last_applied_seq,
            })),
            inner: Arc::new(Mutex::new(ViewStoreInner {
                db: db.clone(),
                counts,
                meta,
            })),
            record_tx,
            record_rx: Arc::new(Mutex::new(Some(record_rx))),
        })
    }

    pub fn last_applied_seq(&self) -> Result<u64, ViewStoreError> {
        let mem = self.memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        Ok(mem.last_applied_seq)
    }

    /// Replay tail of `views.jsonl` into in-memory counters (startup catch-up).
    pub async fn catch_up(&self, view_log: &ViewLog) -> Result<(), ViewStoreError> {
        let after_seq = self.last_applied_seq()?;
        view_log
            .replay_from(after_seq, |record| self.apply_record(&record))
            .await?;
        Ok(())
    }

    fn apply_record(&self, record: &ViewRecord) -> Result<(), ViewLogError> {
        let ViewEvent::PageView { path } = &record.event;
        let mut mem = self
            .memory
            .lock()
            .map_err(|_| ViewLogError::Apply("view store lock poisoned".into()))?;
        *mem.counts.entry(path.clone()).or_insert(0) += 1;
        mem.dirty_paths.insert(path.clone());
        mem.last_applied_seq = record.seq;
        mem.meta_dirty = true;
        Ok(())
    }

    /// Start the single-writer actor (requires a Tokio runtime).
    pub fn spawn_worker(&self, view_log: Arc<ViewLog>) {
        let Some(rx) = self.record_rx.lock().ok().and_then(|mut g| g.take()) else {
            return;
        };
        let memory = self.memory.clone();
        let inner = self.inner.clone();
        tokio::spawn(view_worker_loop(rx, view_log, memory, inner));
    }

    pub fn record_view(&self, path: String) -> Result<(), ViewStoreError> {
        self.record_tx.try_send(ViewCommand {
            path,
            ts: now_ms(),
        }).map_err(|_| {
            ViewStoreError::Log(ViewLogError::Apply("view writer backlog full".into()))
        })
    }

    pub fn get_views_count(&self, path: &str) -> Result<u64, ViewStoreError> {
        let mem = self.memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        Ok(mem.counts.get(path).copied().unwrap_or(0))
    }

    pub fn flush(&self) -> Result<(), ViewStoreError> {
        flush_dirty(&self.memory, &self.inner)
    }

    pub fn increment(&self, path: String) {
        if let Err(e) = self.record_view(path) {
            tracing::warn!(err = %e, "view record failed");
        }
    }

    pub fn get_views(&self, path: &str) -> u64 {
        self.get_views_count(path).unwrap_or(0)
    }
}

async fn view_worker_loop(
    mut rx: mpsc::Receiver<ViewCommand>,
    view_log: Arc<ViewLog>,
    memory: MemStateLock,
    inner: Arc<Mutex<ViewStoreInner>>,
) {
    let mut batches_since_sync = 0_u64;
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }

        match append_batch_and_project(&view_log, &memory, &batch).await {
            Ok(()) => {
                batches_since_sync += 1;
                if batches_since_sync >= SYNC_EVERY_BATCHES {
                    if let Err(e) = view_log.sync().await {
                        tracing::warn!(err = %e, "view log sync failed");
                    }
                    batches_since_sync = 0;
                }
            }
            Err(e) => tracing::warn!(err = %e, "view batch append failed"),
        }

        let memory_for_flush = memory.clone();
        let inner_for_flush = inner.clone();
        match tokio::task::spawn_blocking(move || flush_dirty(&memory_for_flush, &inner_for_flush)).await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(err = %e, "view count flush failed"),
            Err(e) => tracing::warn!(err = %e, "view flush task join failed"),
        }
    }
}

async fn append_batch_and_project(
    view_log: &ViewLog,
    memory: &MemStateLock,
    batch: &[ViewCommand],
) -> Result<(), ViewStoreError> {
    let records = {
        let mut mem = memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        let mut records = Vec::with_capacity(batch.len());
        for cmd in batch {
            mem.last_applied_seq += 1;
            let seq = mem.last_applied_seq;
            *mem.counts.entry(cmd.path.clone()).or_insert(0) += 1;
            mem.dirty_paths.insert(cmd.path.clone());
            mem.meta_dirty = true;
            records.push(ViewRecord::new(
                seq,
                cmd.ts,
                ViewEvent::PageView {
                    path: cmd.path.clone(),
                },
            ));
        }
        records
    };

    view_log.append_batch(&records).await?;
    Ok(())
}

fn flush_dirty(memory: &MemStateLock, inner: &Arc<Mutex<ViewStoreInner>>) -> Result<(), ViewStoreError> {
    let snapshot: (Vec<(String, u64)>, u64, bool) = {
        let mut mem = memory.lock().map_err(|_| ViewStoreError::Poisoned)?;
        if mem.dirty_paths.is_empty() && !mem.meta_dirty {
            return Ok(());
        }
        let paths: Vec<String> = mem.dirty_paths.drain().collect();
        let counts = paths
            .into_iter()
            .filter_map(|path| mem.counts.get(&path).map(|&count| (path, count)))
            .collect();
        let seq = mem.last_applied_seq;
        let meta_dirty = mem.meta_dirty;
        mem.meta_dirty = false;
        (counts, seq, meta_dirty)
    };

    let (counts, seq, meta_dirty) = snapshot;
    if counts.is_empty() && !meta_dirty {
        return Ok(());
    }

    let inner = inner.lock().map_err(|_| ViewStoreError::Poisoned)?;
    let mut batch = inner.db.batch();
    for (path, count) in &counts {
        inner.counts.put_in_batch(&mut batch, path, count)?;
    }
    if meta_dirty {
        inner
            .meta
            .put_in_batch(&mut batch, &META_LAST_APPLIED_SEQ.to_string(), &seq)?;
    }
    batch.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use crate::events::ViewEvent;

    #[test]
    fn increment_and_read_from_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ViewStore::open(tmp.path()).unwrap();

        store.apply_record(&ViewRecord::new(
            1,
            1,
            ViewEvent::PageView { path: "/vote".into() },
        ))
        .unwrap();
        store.apply_record(&ViewRecord::new(
            2,
            2,
            ViewEvent::PageView { path: "/vote".into() },
        ))
        .unwrap();
        store.apply_record(&ViewRecord::new(
            3,
            3,
            ViewEvent::PageView { path: "/".into() },
        ))
        .unwrap();

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
            store.apply_record(&ViewRecord::new(
                1,
                1,
                ViewEvent::PageView { path: "/vote".into() },
            ))
            .unwrap();
            store.apply_record(&ViewRecord::new(
                2,
                2,
                ViewEvent::PageView { path: "/vote".into() },
            ))
            .unwrap();
            store.flush().unwrap();
        }

        let store = ViewStore::open(&store_path).unwrap();
        assert_eq!(store.get_views_count("/vote").unwrap(), 2);
        assert_eq!(store.last_applied_seq().unwrap(), 2);
    }

    #[tokio::test]
    async fn worker_appends_to_view_log_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("store");
        let log_path = tmp.path().join("views.jsonl");
        let view_log = Arc::new(ViewLog::new(&log_path));

        {
            let store = ViewStore::open(&store_path).unwrap();
            store.spawn_worker(view_log.clone());
            store.record_view("/vote".into()).unwrap();
            store.record_view("/vote".into()).unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            store.flush().unwrap();
            view_log.sync().await.unwrap();
            assert_eq!(store.get_views_count("/vote").unwrap(), 2);
            assert_eq!(store.last_applied_seq().unwrap(), 2);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;

        let text = std::fs::read_to_string(&log_path).unwrap();
        assert!(text.contains("\"seq\":1"));
        assert!(text.contains("\"seq\":2"));

        let store = ViewStore::open(&store_path).unwrap();
        assert_eq!(store.get_views_count("/vote").unwrap(), 2);
        assert_eq!(store.last_applied_seq().unwrap(), 2);
    }

    #[tokio::test]
    async fn catch_up_replays_view_log_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("store");
        let log_path = tmp.path().join("views.jsonl");
        let view_log = ViewLog::new(&log_path);
        view_log
            .append_batch(&[
                ViewRecord::new(1, 1, ViewEvent::PageView { path: "/vote".into() }),
                ViewRecord::new(2, 2, ViewEvent::PageView { path: "/vote".into() }),
            ])
            .await
            .unwrap();

        let store = ViewStore::open(&store_path).unwrap();
        store.catch_up(&view_log).await.unwrap();
        store.flush().unwrap();

        assert_eq!(store.get_views_count("/vote").unwrap(), 2);
        assert_eq!(store.last_applied_seq().unwrap(), 2);
    }
}
