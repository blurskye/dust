#!/usr/bin/env bash
# Opt-in qualification for Dust's private NixOS compatibility runtime. The test
# realizes the generic locked runtime in an isolated state store, installs all
# five checksum-pinned upstream LSP artifacts through the real Rust installer,
# and proves initialize, completion, shutdown, and exit for every server. It
# needs Nix, Cargo, and upstream/cache network access; it never needs root or a
# project flake and never writes the bundled lock file.
set -euo pipefail

cd "$(dirname "$0")/.."
for command in nix cargo sha256sum; do
    command -v "$command" >/dev/null || {
        printf 'missing required command: %s\n' "$command" >&2
        exit 1
    }
done

lock_before=$(sha256sum assets/tools/nix-workspace/flake.lock)
nix flake show --no-update-lock-file --no-write-lock-file \
    path:assets/tools/nix-workspace >/dev/null
cargo test --locked --lib \
    packages::tests::nixos_runtime_completes_protocol_smoke_for_every_catalog_lsp \
    -- --ignored --exact --nocapture
test "$lock_before" = "$(sha256sum assets/tools/nix-workspace/flake.lock)"
printf 'Dust private Nix runtime qualified for all catalog LSP protocol sessions.\n'
