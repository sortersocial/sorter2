# Durable

Small RocksDB-backed typed collections for this workspace.

`durable` is not a general-purpose database layer. It is a single-process,
single-writer wrapper that gives the app ergonomic persistent maps/vectors,
explicit write batches, and configurable durability for rebuildable indexes.

## Contract

- **Storage engine:** RocksDB default column family.
- **Serialization:** CBOR through Serde (`ciborium`).
- **Collections:** `DurableMap<K, V>` and `DurableVec<T>`.
- **Writer model:** one writer process. Serialize writes at the app layer.
- **Atomicity:** a `Batch` commits its RocksDB `WriteBatch` atomically.
- **Durability modes** (via [`Batch::commit_with`] only):
  - `Durability::SyncWal` writes the WAL and fsyncs it before returning.
  - `Durability::WalOnly` writes through RocksDB WAL without forcing fsync.
  - `Durability::DisableWal` skips RocksDB WAL; use only when another durable
    source of truth can rebuild the data.

**Important:** convenience methods on `DurableMap` / `DurableVec` (`put`,
`insert`, `remove`, `clear`, nested `entry().or_default()`, etc.) always commit
with a WAL **fsync** (`flush_wal(true)`). They do not accept a durability
argument. Use explicit batches when you need `WalOnly` or `DisableWal`.

Sorter uses `DisableWal` for projection writes after fsyncing `events.jsonl`,
because the projection is rebuildable from the event log.

## Example

```rust
use durable::{Db, DurableMap};

fn main() -> durable::Result<()> {
    let db = Db::open("my_db")?;
    let mut scores = DurableMap::<String, u32>::new(&db, "scores")?;

    scores.put("alice".to_string(), 100)?;
    scores.put("bob".to_string(), 85)?;

    assert_eq!(scores.get(&"alice".to_string())?, Some(100));
    Ok(())
}
```

## Explicit batches

Use a batch when multiple collection writes must commit together.

```rust
use durable::{Db, Durability, DurableMap};

fn main() -> durable::Result<()> {
    let db = Db::open("my_db")?;
    let scores = DurableMap::<String, u32>::new(&db, "scores")?;
    let meta = DurableMap::<String, u64>::new(&db, "meta")?;

    let mut batch = db.batch();
    scores.put_in_batch(&mut batch, &"alice".to_string(), &100)?;
    scores.put_in_batch(&mut batch, &"bob".to_string(), &85)?;
    meta.put_in_batch(&mut batch, &"last_seq".to_string(), &2)?;
    batch.commit_with(Durability::SyncWal)?;
    Ok(())
}
```

`DurableMap::put_in_batch` updates map length metadata at batch commit time, so
multiple new keys in one batch are counted correctly.

## Schema evolution

`durable` only serializes typed values. Long-lived applications should store
their own versioned DTOs, for example:

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct Versioned<T> {
    version: u32,
    payload: T,
}
```

For rebuildable projections, prefer deleting the projection and replaying the
canonical log when the DTO version changes.

## Prefix scans vs sorted keys

Collections store keys under a fixed RocksDB prefix (`map:{name}:entry:…`).
[`DurableMap::iter`], [`DurableMap::clear`], and [`Db::scan_prefix`] perform
cheap prefix scans **within one collection**.

Keys are CBOR-encoded Serde values, so lexicographic order is **not** semantic
sort order for arbitrary structs or URL paths. There is no `DurableIndex` or
`.range()` API yet — nested collections (via `entry().or_default()`) are the
supported way to group related keys for prefix iteration.

## What this is not

- Not multi-process safe.
- Not distributed.
- Not SQL.
- Not a replacement for an application-level recovery plan.
- Not a promise that arbitrary Rust structs can evolve safely on disk.

## Testing

Run:

```bash
cargo test -p durable
```
