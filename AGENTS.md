# AGENTS.md

## Cursor Cloud specific instructions

### Product

Single Rust web app **`sorter2-server`**: pairwise voting, rank-centrality rankings, JSONL event log on disk. No database, no docker-compose, no separate frontend dev server.

### Toolchain (non-obvious)

- **Bootstrap script**: `./scripts/cursor-env-install.sh` (also run via `.cursor/environment.json` on Cloud Agent boot) installs Playwright Chromium, Babashka, bbin, `clj-paren-repair`, and warms the RocksDB build.
- **Rust 1.88+** is required (some transitive crates need a recent Cargo). The image may ship older `/usr/local/cargo` (1.83); use **rustup** and `rustup default 1.88.0` before building.
- **RocksDB / `durable`**: Ubuntu’s default `c++` is often **clang** without libc++ headers. Set **`CXX=g++`** and **`RUSTFLAGS="-C linker=g++"`** (or `CC=gcc`) before `cargo build` / `cargo test` — both are set in the bootstrap script and `.cursor/environment.json`.
- **System packages** for builds: `build-essential`, `g++`, `pkg-config`, `libssl-dev`, `openjdk-21-jre-headless` (for `reqwest` / OpenSSL, `librocksdb-sys`, and **bbin** / Clojure JVM). The bootstrap sets **`JAVA_HOME`** when Java is present.
- **Clojure CLI 1.12.0.1530** (used in CI): install from https://clojure.org/guides/install_clojure — needed for `./scripts/clj-test.sh` / Kaocha tests.
- **Babashka / bbin / clj-paren-repair**: installed by `cursor-env-install.sh` into `~/.local/bin` (bb tasks in `bb.edn`, delimiter repair for Clojure edits).
- **Playwright** (Spel browser tests in `test/vote_compare.clj`): Chromium via `clojure -M -e "(com.microsoft.playwright.CLI/main ...)"` — run once after clone or use the bootstrap script.

### Commands (see also `TEST.sh`)

| Task | Command |
|------|---------|
| Rust unit + integration tests | `cargo test --all` |
| Full suite (Rust + Clojure smoke) | `./TEST.sh` |
| Clojure smoke only | `./scripts/clj-test.sh` |
| Cloud VM bootstrap | `./scripts/cursor-env-install.sh` |
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
