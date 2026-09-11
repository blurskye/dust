# Target 0.3 clone Component fixture

This is an explicitly **test-only, non-production** Component fixture for the
normative target ABI in `docs/specs/api-next.wit`. Dust production loading,
manifests, the SDK, and `wit/dust-plugin.wit` remain on 0.2.

`target03-clone-guest.core.wasm` is the checked no-WASI core module generated
from `src/lib.rs`; tests wrap it with `wit-component` and instantiate the real
Component through Wasmtime. Its strict target fixture manifest admits retained
and private content cloning. The guest opens and clones both presenters,
observes exact `ui-result` and dimensions callbacks, and publishes fresh cells
for every private view. Hostile commands exercise oversized retained/private
payloads, wrong/stale private identities, out-of-bounds and wide-overflow cells,
result-only authority refusal, and execution/callback rollback after an accepted
queue call. It also drives checked move, tile, dock, fullscreen, hide, focus and
close-view requests through the exact live Component source, and calls one
deliberately unimplemented import so the host's typed `unsupported` rejection
is exercised by actual guest code.

Rebuild with the installed stable toolchain and checked lockfile:

```sh
cargo build --locked --offline --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/dust_target03_clone_guest.wasm \
  target03-clone-guest.core.wasm
rm -rf target
sha256sum target03-clone-guest.core.wasm
```

Expected SHA-256:
`9d1caf84261a71dc3123cea6bcd652e6a92105dd74b9995486c47a34d319d545`.
