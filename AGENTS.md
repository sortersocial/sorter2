# AGENTS.md

## Cursor Cloud specific instructions

### Product

Single Rust web app **`sorter2-server`**: pairwise voting, rank-centrality rankings, JSONL event log on disk. No database, no docker-compose, no separate frontend dev server.

### Toolchain (non-obvious)

- **Rust 1.88+** is required (some transitive crates need a recent Cargo). The image may ship older `/usr/local/cargo` (1.83); use **rustup** and `rustup default 1.88.0` before building.
- **System packages** for builds: `pkg-config`, `libssl-dev` (for `reqwest` / OpenSSL in integration tests and release builds).
- **Clojure CLI 1.12.0.1530** (optional but used in CI): install from https://clojure.org/guides/install_clojure — only needed for `./scripts/clj-test.sh` / Kaocha smoke test.

### Commands (see also `TEST.sh`)

| Task | Command |
|------|---------|
| Rust unit + integration tests | `cargo test --all` |
| Full suite (Rust + Clojure smoke) | `./TEST.sh` |
| Clojure smoke only | `./scripts/clj-test.sh` |
| Run dev server | `cargo run --package sorter2-server` |
| Release binary | `cargo build --release --package sorter2-server` → `target/release/sorter2-server` |

### Running the server

Environment variables (defaults in `server/src/state.rs`):

- `PORT` — default `8080`
- `SORTER2_DATA_DIR` — default `./data` (created on startup)
- `SORTER2_EVENT_LOG` — default `{data_dir}/events.jsonl`

Health check: `GET /healthz` → `ok`.

Core UI flow: `POST /ui` with form field `__rpc__` (JSON). Example vote:

```bash
curl -sf -X POST http://127.0.0.1:8080/ui \
  --data-urlencode '__rpc__={"action":"record_vote","a":"alpha","b":"beta","ratio_left":2,"ratio_right":1}'
```

Browser UI loads **Idiomorph** from `unpkg.com`; outbound network is needed for full in-browser morphing (Rust tests do not need it).

### Lint

No dedicated linter in-repo. `cargo test --all` is the primary quality gate; CI also runs Kaocha (`clojure -M:kaocha`).

### Long-running processes

Use **tmux** for `cargo run --package sorter2-server` (dev server). Rebuild after `cargo` dependency changes — there is no hot reload for Rust.

### Gotchas

- First `cargo test` / `cargo build --release` is slow; Clojure smoke test always does a release build.
- `legacy/` and `ideas/` are not part of the workspace build.
