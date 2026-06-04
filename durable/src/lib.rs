//! Durable - RocksDB-backed persistent data structures for Rust

use rocksdb::{Options, WriteBatch, WriteOptions, DB as RocksDB};
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use std::{collections::HashMap, path::Path};
use thiserror::Error;

pub mod map;
pub mod vec;
pub use map::DurableMap;
pub use vec::DurableVec;

/// Error types for Durable operations
#[derive(Error, Debug)]
pub enum DurableError {
    #[error("RocksDB error: {0}")]
    RocksDB(#[from] rocksdb::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("Key not found")]
    KeyNotFound,

    #[error("Collection not found: {0}")]
    CollectionNotFound(String),

    #[error("Data corruption: {0}")]
    Corruption(String),
}

pub type Result<T> = std::result::Result<T, DurableError>;

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes)
        .map_err(|e| DurableError::Serialization(e.to_string()))?;
    Ok(bytes)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ciborium::de::from_reader(bytes).map_err(|e| DurableError::Serialization(e.to_string()))
}

/// A trait for types that can be used as nested collections.
pub trait DurableCollection {
    /// Creates a new instance of the collection from a database handle
    /// and a pre-determined, unique key prefix.
    ///
    /// This is the key method that allows `DurableMap` to instantiate
    /// a nested collection handle.
    fn from_prefix(db: Db, prefix: Vec<u8>) -> Self;
}

/// The main database handle
#[derive(Clone)]
pub struct Db {
    inner: Arc<RocksDB>,
}

impl Db {
    /// Opens or creates a durable database at the given path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let db = RocksDB::open(&opts, path)?;
        Ok(Db {
            inner: Arc::new(db),
        })
    }

    /// Create a new write batch for atomic operations
    pub fn batch(&self) -> Batch {
        Batch {
            db: self.clone(),
            inner: WriteBatch::default(),
            len_deltas: HashMap::new(),
            pending_puts: HashMap::new(),
        }
    }

    /// Get the underlying RocksDB handle (for advanced usage)
    pub(crate) fn rocks(&self) -> &RocksDB {
        &self.inner
    }

    /// Get a new unique collection ID for nested collections
    pub fn new_collection_id(&self) -> Result<u64> {
        let key = b"__global_meta:next_collection_id";

        // Get current value
        let current_bytes = self.rocks().get(key)?;
        let current_id = match current_bytes {
            Some(bytes) => {
                if bytes.len() != 8 {
                    return Err(DurableError::Corruption(
                        "Invalid collection ID bytes size".into(),
                    ));
                }
                let id_bytes: [u8; 8] = bytes[..8]
                    .try_into()
                    .map_err(|_| DurableError::Corruption("Invalid collection ID bytes".into()))?;
                u64::from_le_bytes(id_bytes)
            }
            None => 0,
        };

        let next_id = current_id + 1;

        // Try to atomically update - use compare-and-swap semantics
        let mut batch = WriteBatch::default();
        batch.put(key, &next_id.to_le_bytes());

        // Durable assumes a single writer process; external callers must
        // serialize collection creation.
        self.rocks().write(batch)?;
        self.rocks().flush_wal(true)?;

        Ok(current_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Write through RocksDB WAL and force it to durable storage before return.
    SyncWal,
    /// Write through RocksDB WAL without forcing an fsync.
    WalOnly,
    /// Skip RocksDB WAL. Use only for rebuildable projections backed by another log.
    DisableWal,
}

/// A write batch for atomic operations
pub struct Batch {
    db: Db,
    inner: WriteBatch,
    len_deltas: HashMap<Vec<u8>, i64>,
    pending_puts: HashMap<Vec<u8>, Vec<u8>>,
}

impl Batch {
    /// Add a raw key/value put to this batch.
    pub fn put(&mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.inner.put(key, value);
    }

    /// Add a raw key delete to this batch.
    pub fn delete(&mut self, key: impl AsRef<[u8]>) {
        self.inner.delete(key);
    }

    pub(crate) fn track_map_put(&mut self, db_key: Vec<u8>, len_key: Vec<u8>, is_new: bool) {
        if self.pending_puts.insert(db_key, len_key.clone()).is_none() && is_new {
            *self.len_deltas.entry(len_key).or_insert(0) += 1;
        }
    }

    /// Commit all operations in this batch atomically
    pub fn commit(self) -> Result<()> {
        self.commit_with(Durability::SyncWal)
    }

    /// Commit all operations with an explicit durability policy.
    pub fn commit_with(mut self, durability: Durability) -> Result<()> {
        for (len_key, delta) in self.len_deltas {
            if delta == 0 {
                continue;
            }
            let current = match self.db.rocks().get(&len_key)? {
                Some(bytes) => {
                    if bytes.len() != 8 {
                        return Err(DurableError::Corruption("Invalid length bytes size".into()));
                    }
                    let len_bytes: [u8; 8] = bytes[..8]
                        .try_into()
                        .map_err(|_| DurableError::Corruption("Invalid length bytes".into()))?;
                    u64::from_le_bytes(len_bytes)
                }
                None => 0,
            };
            let next = if delta.is_negative() {
                current
                    .checked_sub(delta.unsigned_abs())
                    .ok_or_else(|| DurableError::Corruption("length underflow".into()))?
            } else {
                current + delta as u64
            };
            self.inner.put(&len_key, next.to_le_bytes());
        }

        match durability {
            Durability::SyncWal => {
                self.db.rocks().write(self.inner)?;
                self.db.rocks().flush_wal(true)?;
            }
            Durability::WalOnly => {
                self.db.rocks().write(self.inner)?;
            }
            Durability::DisableWal => {
                let mut opts = WriteOptions::default();
                opts.disable_wal(true);
                self.db.rocks().write_opt(self.inner, &opts)?;
            }
        }
        Ok(())
    }
}
