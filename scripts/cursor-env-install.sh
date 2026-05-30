#!/usr/bin/env bash
# Idempotent Cloud Agent / Cursor VM bootstrap for sorter2.
# Installs: RocksDB C++ toolchain (g++), Playwright browsers, Babashka, bbin, clj-paren-repair.
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
cat >"${profile_snippet}" <<'EOF'
export PATH="${HOME}/.local/bin:${PATH}"
export CC="${CC:-gcc}"
export CXX="${CXX:-g++}"
export RUSTFLAGS="${RUSTFLAGS:--C linker=g++}"
EOF
for rc in "${HOME}/.bashrc" "${HOME}/.profile"; do
  if [[ -f "${rc}" ]] && ! grep -qF 'sorter2-env.sh' "${rc}" 2>/dev/null; then
    echo ". ${profile_snippet}" >>"${rc}"
  fi
done

apt_packages=(
  build-essential
  g++
  pkg-config
  libssl-dev
  curl
  ca-certificates
  git
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

# Rust 1.88+ (image may ship older /usr/local/cargo)
need_rustup=false
if ! command -v rustc >/dev/null 2>&1; then
  need_rustup=true
elif ! rustc --version | grep -qE 'rustc 1\.(8[89]|[9-9][0-9]|[1-9][0-9]{2,})\.'; then
  need_rustup=true
fi
if [[ "${need_rustup}" == true ]]; then
  if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88.0
  else
    rustup toolchain install 1.88.0
    rustup default 1.88.0
  fi
  # shellcheck source=/dev/null
  [[ -f "${HOME}/.cargo/env" ]] && source "${HOME}/.cargo/env"
fi

install_babashka() {
  if command -v bb >/dev/null 2>&1; then
    return 0
  fi
  local version url tmp
  version="$(curl -fsSL https://api.github.com/repos/babashka/babashka/releases/latest | grep -m1 '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')"
  url="https://github.com/babashka/babashka/releases/download/v${version}/babashka-${version}-linux-amd64-static.tar.gz"
  tmp="$(mktemp -d)"
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

install_babashka
install_bbin

if ! command -v clj-paren-repair >/dev/null 2>&1; then
  bbin install https://github.com/bhauman/clojure-mcp-light.git \
    --tag v0.2.2 \
    --as clj-paren-repair \
    --main-opts '["-m" "clojure-mcp-light.paren-repair"]'
fi

cd "${ROOT}"
clojure -P -M
clojure -M -e "(com.microsoft.playwright.CLI/main (into-array String [\"install\" \"chromium\" \"--with-deps\"]))"

# Warm RocksDB + release server link (Clojure tests use release binary).
cargo build -p durable --quiet
cargo build --release --package sorter2-server --quiet

echo "cursor-env-install: ok (bb=$(bb --version 2>/dev/null || echo missing), CXX=${CXX})"
