#!/usr/bin/env bash
# Idempotent Cloud Agent / Cursor VM bootstrap for sorter2.
# Installs: RocksDB C++ toolchain (g++, libclang), Playwright browsers, Babashka, bbin, clj-paren-repair.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCAL_BIN="${HOME}/.local/bin"
export PATH="${LOCAL_BIN}:${PATH}"
export CXX="${CXX:-g++}"
export CC="${CC:-gcc}"
export RUSTFLAGS="${RUSTFLAGS:--C linker=g++}"

mkdir -p "${LOCAL_BIN}"

profile_snippet="${HOME}/.config/cursor/sorter2-env.sh"
mkdir -p "$(dirname "${profile_snippet}")"
detect_java_home() {
  if [[ -n "${JAVA_HOME:-}" && -x "${JAVA_HOME}/bin/java" ]]; then
    return 0
  fi
  if command -v java >/dev/null 2>&1; then
    JAVA_HOME="$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")"
    export JAVA_HOME
    return 0
  fi
  if [[ -d /usr/lib/jvm/default-java ]]; then
    export JAVA_HOME=/usr/lib/jvm/default-java
    return 0
  fi
  return 1
}

cat >"${profile_snippet}" <<'EOF'
# Prefer rustup (~/.cargo/bin); do not prepend /usr/local/cargo (often stale 1.83).
[[ -f "${HOME}/.cargo/env" ]] && . "${HOME}/.cargo/env"
export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:${PATH}"
export CC="${CC:-gcc}"
export CXX="${CXX:-g++}"
export RUSTFLAGS="${RUSTFLAGS:--C linker=g++}"
if [[ -z "${LIBCLANG_PATH:-}" ]]; then
  for candidate in /usr/lib/llvm-*/lib; do
    if [[ -e "${candidate}/libclang.so" ]]; then
      export LIBCLANG_PATH="${candidate}"
      break
    fi
  done
fi
if [[ -z "${JAVA_HOME:-}" ]]; then
  if command -v java >/dev/null 2>&1; then
    export JAVA_HOME="$(dirname "$(dirname "$(readlink -f "$(command -v java)")")")"
  elif [[ -d /usr/lib/jvm/default-java ]]; then
    export JAVA_HOME=/usr/lib/jvm/default-java
  fi
fi
EOF
for rc in "${HOME}/.bashrc" "${HOME}/.profile"; do
  if [[ -f "${rc}" ]] && ! grep -qF 'sorter2-env.sh' "${rc}" 2>/dev/null; then
    echo ". ${profile_snippet}" >>"${rc}"
  fi
done

rust_toolchain_channel() {
  local channel="1.88.0"
  if [[ -f "${ROOT}/rust-toolchain.toml" ]]; then
    local parsed
    parsed="$(
      grep -E '^\s*channel\s*=' "${ROOT}/rust-toolchain.toml" \
        | head -1 \
        | sed -E 's/.*=\s*"?([^"]+)"?.*/\1/' \
        | tr -d ' '
    )"
    [[ -n "${parsed}" ]] && channel="${parsed}"
  fi
  printf '%s' "${channel}"
}

# Install Rust before apt/playwright so `cargo test` works while the rest of bootstrap runs.
ensure_rust_toolchain() {
  local channel triple tc_dir
  channel="$(rust_toolchain_channel)"
  triple="${channel}-x86_64-unknown-linux-gnu"
  tc_dir="${RUSTUP_HOME:-${HOME}/.rustup}/toolchains/${triple}"

  if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain "${channel}"
  fi
  # shellcheck source=/dev/null
  [[ -f "${HOME}/.cargo/env" ]] && source "${HOME}/.cargo/env"
  export PATH="${HOME}/.cargo/bin:${PATH}"

  if ! rustc -vV >/dev/null 2>&1; then
    rustup toolchain uninstall "${channel}" 2>/dev/null || true
    rm -rf "${tc_dir}"
  fi

  # Idempotent; repairs partial toolchains ("Missing manifest") when rust-toolchain.toml overrides.
  if ! rustup toolchain install "${channel}"; then
    rm -rf "${tc_dir}"
    rustup toolchain install "${channel}"
  fi
  rustup default "${channel}" 2>/dev/null || true
  if ! rustc -vV >/dev/null 2>&1; then
    echo "cursor-env-install: rustc unavailable after rustup (channel=${channel})" >&2
    return 1
  fi
}
ensure_rust_toolchain

apt_packages=(
  build-essential
  g++
  clang
  libclang-dev
  pkg-config
  libssl-dev
  curl
  ca-certificates
  git
  openjdk-21-jre-headless
)

if command -v apt-get >/dev/null 2>&1; then
  if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${apt_packages[@]}"
  elif [[ "$(id -u)" -eq 0 ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${apt_packages[@]}"
  fi
fi

detect_java_home || true

if [[ -z "${LIBCLANG_PATH:-}" ]]; then
  for candidate in /usr/lib/llvm-*/lib; do
    if [[ -e "${candidate}/libclang.so" ]]; then
      export LIBCLANG_PATH="${candidate}"
      break
    fi
  done
fi

install_babashka() {
  if command -v bb >/dev/null 2>&1; then
    return 0
  fi
  local version url tmp release_json
  tmp="$(mktemp -d)"
  release_json="${tmp}/release.json"
  curl -fsSL -o "${release_json}" https://api.github.com/repos/babashka/babashka/releases/latest
  version="$(grep -m1 '"tag_name"' "${release_json}" | sed -E 's/.*"v([^"]+)".*/\1/')"
  url="https://github.com/babashka/babashka/releases/download/v${version}/babashka-${version}-linux-amd64-static.tar.gz"
  curl -fsSL "${url}" -o "${tmp}/bb.tar.gz"
  tar xzf "${tmp}/bb.tar.gz" -C "${tmp}"
  install -m755 "${tmp}/bb" "${LOCAL_BIN}/bb"
  rm -rf "${tmp}"
}

install_bbin() {
  if command -v bbin >/dev/null 2>&1; then
    return 0
  fi
  curl -fsSL -o "${LOCAL_BIN}/bbin" https://raw.githubusercontent.com/babashka/bbin/v0.2.4/bbin
  chmod +x "${LOCAL_BIN}/bbin"
}

install_clojure_cli() {
  if command -v clojure >/dev/null 2>&1; then
    return 0
  fi
  local installer=/tmp/linux-install-clojure.sh
  curl -fsSL -o "${installer}" \
    https://download.clojure.org/install/linux-install-1.12.0.1530.sh
  if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    sudo bash "${installer}"
  elif [[ "$(id -u)" -eq 0 ]]; then
    bash "${installer}"
  else
    echo "cursor-env-install: need root/sudo to install Clojure CLI" >&2
    return 1
  fi
  rm -f "${installer}"
}

install_babashka
install_bbin
install_clojure_cli

detect_java_home || {
  echo "cursor-env-install: warning: JAVA_HOME not set; skipping bbin/clj-paren-repair" >&2
}

if detect_java_home && ! command -v clj-paren-repair >/dev/null 2>&1; then
  bbin install https://github.com/bhauman/clojure-mcp-light.git \
    --tag v0.2.2 \
    --as clj-paren-repair \
    --main-opts '["-m" "clojure-mcp-light.paren-repair"]'
fi

cd "${ROOT}"
clojure -P -M
clojure -M -e "(com.microsoft.playwright.CLI/main (into-array String [\"install\" \"chromium\" \"--with-deps\"]))"

# Warm RocksDB + release server link (Clojure tests use release binary).
if ! cargo build -p durable --quiet; then
  echo "cursor-env-install: cargo build failed (need libclang-dev? CXX=g++? LIBCLANG_PATH?)" >&2
  exit 1
fi
if ! cargo build --release --package sorter2-server --quiet; then
  echo "cursor-env-install: release build failed" >&2
  exit 1
fi

echo "cursor-env-install: ok (bb=$(bb --version 2>/dev/null || echo missing), CXX=${CXX})"
