# sorter2 storage & memory plan

## Goal

Fit a **decent chunk of Reddit** into a **256MB** Fly VM while keeping the product simple: one Rust binary, no external database service.

**RAM should be bounded by query shape**, not dataset size — ideally one rank-centrality graph in memory at a time, plus runtime overhead.

**`events.jsonl` remains the main database.** Everything on disk elsewhere is a **rebuildable projection**.

---

## Architecture (target)

```
                    ┌─────────────────────────────────┐
                    │  events.jsonl (source of truth) │
                    └───────────────┬─────────────────┘
                                    │
                         append on every mutation
                                    │
                                    ▼
         ┌──────────────────────────────────────────────┐
         │  apply event → durable projection (on disk)     │
         │  (same semantics as today's in-memory reducer)  │
         └──────────────────────────────────────────────┘
                    │                    │
                    ▼                    ▼
         ┌──────────────────┐   ┌──────────────────────────┐
         │ entity_payloads  │   │ reducer state per scope   │
         │ (fat Reddit JSON)│   │ nodes, children, edges, … │
         └──────────────────┘   └──────────────────────────┘
                    │                    │
                    └────────┬───────────┘
                             ▼
         ┌──────────────────────────────────────────────┐
         │  RAM per request (or small LRU cache)         │
         │  • one scope's GroupState for rank-centrality │
         │  • children + EntityData (small views)        │
         │  • RC scratch allocations                     │
         │  → compute → render → drop / evict            │
         └──────────────────────────────────────────────┘
```

This is **event sourcing / CQRS**:

| Layer | Role |
|-------|------|
| **JSONL** | Canonical write log; audit; disaster recovery |
| **Durable (RocksDB)** | Materialized read model + payload store; rebuildable from JSONL |
| **RAM** | One (or few) hot scopes for ranking and render |

Durable is **not** a second source of truth. If projection and log diverge, **stream JSONL and rebuild durable**.

The plan is sound, but only if the projection layer is treated as a crash-recoverable index:

- JSONL append must mean "the bytes are recoverable after process or VM crash" (`flush` alone is not enough; use `sync_data` / fsync-equivalent on the append path or make an explicit weaker durability tradeoff).
- Durable projection writes are allowed to lag JSONL, but startup must catch up from the last applied event before serving reads.
- Event application must be exactly-once for the durable projection. Votes add edge weights, so accidentally replaying the same event twice changes rankings.
- Projection schema should stay boring and explicit: persist nodes, children, edge weights, voted pairs, and capped recent votes directly. Avoid clever nested abstractions if they make rebuilds, migrations, or audits harder.

---

## What we have today (baseline)

| Piece | Status |
|-------|--------|
| `events.jsonl` append-only log | ✓ source of truth |
| `EventLog::replay` streaming one event at a time | ✓ no full `Vec<Event>` at startup |
| `entity_store` / `entity_db` (RocksDB via `durable`) | ✓ fat payloads off-heap |
| `EntityData` in `GlobalTree` | ✓ small derived views in RAM |
| Full `GlobalTree` replayed at boot | ✗ all nodes, all scopes' `GroupState` in RAM |
| Rank-centrality | ✓ already scoped per parent; reads in-memory `GroupState` |

**Measured RSS (release, ~395 imports, 2.8MB JSONL):**

| Scenario | RSS |
|----------|-----|
| Empty data dir | ~14 MB (+ RocksDB baseline) |
| After boot with data | ~21–22 MB |
| Pre-offload (in-tree payloads + vec replay) | ~25–34 MB |

Payload offload + streaming replay helped startup peak, but **the full in-memory reducer** is still the scaling ceiling.

---

## What lives where (target)

### JSONL (`events.jsonl`)

All mutations, append-only:

- `VoteRecorded` — scope, pair, ratios
- `EntityImported` — id, full upstream payload
- `NodeEnsured` — register path
- (legacy / other event types as present in log)

### Durable / RocksDB (`{data_dir}/…`)

Single embedded DB directory. Collections (names tentative):

| Collection | Contents | Notes |
|------------|----------|-------|
| `entity_payloads` | `ItemId → JSON string` | **Done.** Fat Reddit API blobs |
| `nodes` | `ItemId → { data: EntityData, children: … }` | Small; no raw payload |
| `scopes/{parent}/…` | `GroupState` materialization | edges, voted_pairs, item_to_idx, recent_votes (capped) |

Nested layout can follow durable's `Map → Map → Vec` patterns (see `durable/docs/001.md` Sorter sketch).

### RAM

| Resident | When |
|----------|------|
| Tokio, Axum, reqwest, RocksDB block cache (tuned) | always |
| **One scope slice** | per request (or LRU of few scopes, byte-capped) |
| Rank-centrality temporaries | during render for that scope |

**Not** in RAM at steady state: all subreddits, all vote graphs, all payloads.

---

## Write path

Order matters:

1. Append event to `events.jsonl` (must durably succeed first)
2. Apply event to durable projection (same logic as today's `apply_event`)
3. Invalidate / update in-memory scope cache if that scope is hot

Journal worker already serializes votes disk → tree; extend to **disk → durable** instead of (eventually) **disk → full GlobalTree**.

```rust
// conceptual
append(jsonl, event)?;
apply_to_durable(event)?;
scope_cache.invalidate(scope_for(event));
```

On failure after (1): replay from log repairs projection on next boot or via `replay-index` command.

Because JSONL and RocksDB cannot be committed atomically together, durable must record a projection cursor alongside the projection:

- Prefer a monotonically increasing event sequence number in each JSONL event.
- Acceptable first version: byte offset + line checksum, as long as truncation and partial trailing lines are handled deliberately.
- Update projection data and cursor in the same RocksDB `WriteBatch`.
- On startup, read the cursor, scan only the JSONL tail after that cursor, apply missing events, then serve.
- If the cursor is missing, corrupt, or points past the log, rebuild durable from JSONL.

This keeps the append-first rule simple: if the process dies after JSONL append but before RocksDB apply, catch-up repairs it; if it dies after RocksDB apply but before cursor update, the batch should not expose a cursor that skips work.

---

## Read path

For a page under parent scope `P` (e.g. `reddit.com/r/rust`):

1. **Load scope** from durable (or scope LRU hit)
   - `EntityData` + children for listing
   - `GroupState` for ranking and pair selection
2. **Run rank-centrality** on that `GroupState` (requires RAM — that's fine)
3. **Render**
4. **Drop** scope from RAM or return to LRU

Payload fetch (rare): `entity_store.get(id)` only when render needs fields not in `EntityData`.

---

## Startup & recovery

### Normal startup

```
open entity_db (RocksDB)
open / validate scope indexes in same DB
read projection cursor
stream only unapplied JSONL tail into durable
do NOT replay JSONL into RAM
serve requests (cold scopes loaded on demand)
```

### Rebuild projection

```
stream events.jsonl → apply_event → durable
(one line at a time; same as EventLog::replay today)
```

Run when:

- First deploy of projection layer
- Detected corruption / missing durable dir
- Manual `replay-index` after restoring JSONL from backup

JSONL is the only file you need to trust for recovery.

---

## Scope cache (RAM bound)

**Strict mode:** one scope in RAM at a time — simplest, lowest RAM.

**Practical mode:** LRU cache with **byte budget** (e.g. 64–128MB for scopes on a 256MB VM):

- Evict least-recently-used scope's `GroupState` + child views
- Reload from durable on next visit

Eviction policy is independent of storage engine.

---

## Rank-centrality

No change to the algorithm. It already assumes a whole `GroupState` for one parent scope.

Moving reducer to durable does **not** remove RC memory cost — it removes **holding every scope's graph at once**.

Optional later: materialized score vectors on disk, invalidated on vote. Not required for v1 of this plan.

---

## Durable mutations (future API)

Separate **intent** from **apply** for batching and testability:

```rust
let m = rankings.path().key("rust").key(day).push_end(score);
db.apply(m)?;  // or batch.apply(&[m1, m2, m3])
```

Benefits:

- One RocksDB `WriteBatch` / one WAL flush per vote or import batch
- Serializable ops for tests
- Aligns with JSONL events at the app layer and storage ops at the durable layer

Keep chained `entry().push()` as sugar over `path().…; apply()`.

Type safety: use type-state path builders if we want compile-time nesting; erased `Vec<Op>` only if we accept runtime errors at apply.

---

## Storage engine: RocksDB vs alternatives

**Current choice:** RocksDB via vendored `durable/` workspace crate.

| Engine | Verdict for sorter2 |
|--------|---------------------|
| **RocksDB** | Good default for LSM, prefix scans, write-heavy votes + bulk imports. C++ dep, tune block cache for 256MB. |
| **sled** | Pure Rust appeal; production reliability history gives pause. Not a priority switch. |
| **fjall** | Pure Rust LSM; evaluate with benchmarks if leaving RocksDB. |
| **redb** | Lighter pure Rust; fine for payload-only store; less ideal for heavy scattered writes across scopes. |
| **SQLite** | Wrong shape for nested fractal tree; OK for a single KV table only. |

**Switching engines matters less than:**

1. Batched durable writes
2. Not materializing full reducer in RAM
3. Scope-local load/evict

RocksDB stays **narrow**: blob attic + materialized reducer projection. Not a replacement for JSONL.

---

## Scaling story (before vs after)

| | In-memory reducer (today) | Target (JSONL + durable projection) |
|--|---------------------------|-------------------------------------|
| **RAM grows with** | Total nodes + all scopes + (was) payloads | Hot scope count × scope size + runtime |
| **Disk grows with** | JSONL (+ entity_db payloads today) | JSONL + full durable projection |
| **Startup** | O(events) replay into RAM | O(1) open DB |
| **Fails when** | RSS > VM limit | Scope too large for one RC graph, or disk full |
| **Recovery** | Replay JSONL | Replay JSONL → rebuild durable |

**Rough RAM per active subreddit (~500 posts, moderate votes):**

| Component | Order of magnitude |
|-----------|-------------------|
| `EntityData` × 500 | 0.5–2 MB |
| `GroupState` | 1–5 MB |
| RC scratch | 1–5 MB |
| Runtime + tuned RocksDB | 15–25 MB |
| **Total one hot scope** | **~20–40 MB** |

Multiple subreddits fit on 256MB with LRU eviction, not all resident at once.

---

## Implementation phases

### Phase 0 — Done

- [x] Vend `durable` as workspace crate (`durable/`)
- [x] `entity_store`: payloads in `{data_dir}/entity_db`
- [x] Remove `entity_raw` from `NodeState`
- [x] `EventLog::replay`: stream JSONL, one `Event` at a time
- [x] `apply_event` in `state.rs` for replay semantics

### Phase 1 — Durable projection (write path)

- [ ] Single `Db` under `{data_dir}/store` (payloads + reducer)
- [ ] JSONL append path uses explicit durable sync semantics (`sync_data` / fsync-equivalent) or documents any weaker mode
- [ ] Add projection cursor metadata (event sequence, or byte offset + checksum)
- [ ] `apply_event` writes to durable collections (nodes, scopes) and cursor in one RocksDB `WriteBatch`
- [ ] Journal + Reddit import paths use same apply
- [ ] Batched writes where possible (one flush per vote / per import batch)
- [ ] Idempotency tests: replay/catch-up never double-applies vote edge weights
- [ ] Tests: apply event → read back from durable

### Phase 2 — Stop full tree at boot

- [ ] Startup: open durable, catch up from projection cursor, then serve
- [ ] No `GlobalTree::new()` + full replay into RAM on normal boot
- [ ] `replay-index` command / flag: stream JSONL → durable (offline rebuild)
- [ ] Recovery tests: crash after JSONL append, crash after projection write, corrupt/missing cursor
- [ ] Integration tests use replay-index fixture or temp DB

### Phase 3 — Scope load on read

- [ ] `ScopeView { parent, children, group }` loaded from durable
- [ ] HTML / vote / pair / fetch handlers take `ScopeView` instead of `&GlobalTree`
- [ ] Remove or shrink `Arc<RwLock<GlobalTree>>`

### Phase 4 — Scope cache

- [ ] LRU with byte budget for hot scopes
- [ ] Invalidate on write to that scope
- [ ] Metrics: cache hit/miss, evictions, scope load time

### Phase 5 — Durable API polish (optional)

- [ ] Reified path/mutation API + `WriteBatch` integration in `durable`
- [ ] RocksDB tuning preset for 256MB Fly (`block_cache`, write buffers)
- [ ] Document `replay-index` in AGENTS.md

---

## Non-goals (for now)

- Distributed replication or multi-writer
- Replacing JSONL as canonical store
- SQL query layer over state
- Materialized rank scores on disk (unless RC latency forces it)
- Switching from RocksDB to sled without benchmarks

---

## Open questions

1. **One DB or two?** `{data_dir}/entity_db` today vs single `{data_dir}/store` — merge on Phase 1?
2. **Event identity** — add event sequence numbers now, or start with byte offset + checksum?
3. **Append durability mode** — pay fsync cost per mutation/batch, or document an acknowledged data-loss window?
4. **Scope key encoding** — string `ItemId` paths vs hashed; must match event `scope` field.
5. **Child scope wiring** — Reddit `apply_entity_under_parent` creates children without full path ensure; durable schema must preserve this.
6. **Clojure smoke tests** — still read `events.jsonl`; durable is internal. No change expected.
7. **Fly volume** — `/data` holds JSONL + RocksDB; monitor disk alongside RAM.

---

## Summary

**JSONL = write log. Durable = full reducer on disk + fat payloads. RAM = one rank-centrality graph (or a small LRU of scopes).**

Option B from design discussions is not separate from “reducer in durable” — it **is** reducer in durable, with JSONL still owning writes and recovery. The incremental work is: projection on apply, drop full tree at boot, scope load on read, then cache with eviction.
