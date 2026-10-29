# Target 0.3 clone Component fixture

This is an explicitly **test-only, non-production** Component fixture for the
normative target ABI in `docs/plugin-abi-next.wit`. Dust production loading,
manifests, the SDK, and `wit/dust-plugin.wit` remain on 0.2. The matching host
lane is behind `cfg(test)` and cannot be selected by a production manifest,
configuration entry or CLI flag.

`target03-clone-guest.core.wasm` is the checked no-WASI core module generated
from `src/lib.rs`; tests wrap it with `wit-component` and instantiate the real
Component through Wasmtime. Its ad hoc fixture manifest embeds the current schema-1
`PluginManifest` plus test-only `target_api`, `clone_view` and `tile_view`
fields; it is not `dust-package.toml` schema 2 and is not an installable package.
The guest opens and clones both presenters,
observes exact `ui-result` and dimensions callbacks, and publishes fresh cells
for every private view. Hostile commands exercise oversized retained/private
payloads, titles, notices and guest results; wrong/stale private identities;
out-of-bounds and wide-overflow cells; result-only authority refusal; and
execution/callback rollback after an accepted queue call, including a real fuel
trap. The partial test host returns `unsupported` for most target imports and
implements only enough negotiation, notices and UI/event behavior for this
fixture; its local test bounds are not production or X-07 conformance claims.
It also drives checked move, tile, dock, fullscreen, hide, focus and
close-view requests through the exact live Component source, and calls
deliberately unimplemented snapshot and retained-replacement paths so the host's
typed `unsupported` rejection is exercised by actual guest code. This fixture
does not qualify grants, services, package migration, target providers/storage,
all host functions, all limits or the X-08 promotion gate.

Rebuild with the installed stable toolchain and checked lockfile:

```sh
cargo build --locked --offline --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/dust_target03_clone_guest.wasm \
  target03-clone-guest.core.wasm
rm -rf target
sha256sum target03-clone-guest.core.wasm
```

Expected SHA-256:
`2b74a38adc00bfad1f32da7f5c369c42359edace5c40765a25aeab20cfff05a6`.
