# Clojure performance tests

These tests generate synthetic `events.jsonl` graphs, run the real release server,
and record startup, RSS, read latency, and write throughput numbers under
`target/perf/*.edn`.

They are marked `^:perf` and are skipped by the normal `./scripts/clj-test.sh`
suite. Run them explicitly:

```bash
./scripts/clj-perf-test.sh
```

Useful smaller smoke run:

```bash
SORTER2_PERF_MEMORY_SCOPE_COUNT=5 \
SORTER2_PERF_STARTUP_SMALL_SCOPE_COUNT=2 \
SORTER2_PERF_STARTUP_LARGE_SCOPE_COUNT=5 \
SORTER2_PERF_READ_SCOPE_COUNT=5 \
SORTER2_PERF_WRITE_SCOPE_COUNT=5 \
SORTER2_PERF_MEMORY_VOTES_PER_SCOPE=4 \
SORTER2_PERF_STARTUP_SMALL_VOTES_PER_SCOPE=4 \
SORTER2_PERF_STARTUP_LARGE_VOTES_PER_SCOPE=4 \
SORTER2_PERF_READ_VOTES_PER_SCOPE=4 \
SORTER2_PERF_WRITE_VOTES_PER_SCOPE=4 \
SORTER2_PERF_READ_COUNT=2 \
SORTER2_PERF_WRITE_COUNT=2 \
./scripts/clj-perf-test.sh
```

Default sizes are intentionally large enough to expose today's full-replay /
full-`GlobalTree` behavior. The target assertions describe the desired
post-refactor shape:

- startup RSS bounded by hot scope size
- startup time bounded by unapplied JSONL tail size
- single-scope reads bounded in RSS and latency
- vote writes stay fast with a huge unrelated graph present

Tune thresholds with:

- `SORTER2_PERF_MAX_STARTUP_RSS_KB`
- `SORTER2_PERF_MAX_STARTUP_RATIO`
- `SORTER2_PERF_MAX_READ_P95_MS`
- `SORTER2_PERF_MAX_READ_RSS_DELTA_KB`
- `SORTER2_PERF_MIN_WRITE_RPS`
- `SORTER2_PERF_MAX_WRITE_P95_MS`
