#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/perf
export CC="${CC:-gcc}"
export CXX="${CXX:-g++}"
exec clojure -M:kaocha --config-file perf-tests.edn --focus-meta :perf "$@"
