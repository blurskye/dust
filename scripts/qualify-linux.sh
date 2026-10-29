#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-implementation}"
case "$mode" in
  implementation)
    cargo fmt --all -- --check
    cargo check --locked
    cargo run --quiet --locked -- --check-config
    cargo run --quiet --locked -- --check-api-compatibility api/compat-v1.json
    ;;
  test)
    cargo test --locked --all-targets
    ;;
  clippy)
    cargo clippy --locked --all-targets -- -D warnings
    ;;
  pty)
    test -x target/release/dust || { echo 'release binary required' >&2; exit 2; }
    python3 tests/pty_smoke.py --binary target/release/dust
    ;;
  release)
    cargo build --release --locked
    ;;
  bun-runtime)
    scripts/qualify-bun-runtime.sh
    ;;
  nix-lsp)
    scripts/qualify-nix-lsp-fallback.sh
    ;;
  advisory)
    if ! cargo audit --version >/dev/null 2>&1; then echo 'cargo-audit unavailable; not executed' >&2; exit 77; fi
    cargo audit --deny warnings
    ;;
  fuzz)
    if ! cargo fuzz --version >/dev/null 2>&1 || [[ ! -d fuzz ]]; then echo 'cargo-fuzz target corpus unavailable; not executed' >&2; exit 77; fi
    for target in $(cargo fuzz list); do cargo fuzz run "$target" -- -max_total_time=30; done
    ;;
  soak)
    iterations="${DUST_SOAK_ITERATIONS:-20}"
    for ((i=0;i<iterations;i++)); do cargo run --quiet --locked --example foreground_bench >/dev/null; done
    cargo run --quiet --locked --example workspace_search_bench >/dev/null
    ;;
  limited-terminal)
    TERM=dumb NO_COLOR=1 cargo run --quiet --locked -- --clean --health >/dev/null
    TERM=xterm-256color NO_COLOR=1 cargo run --quiet --locked -- --clean --check-config >/dev/null
    ;;
  *) echo "unknown qualification stage: $mode" >&2; exit 2;;
esac
