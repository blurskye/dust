# Target 0.3 clone Component fixture

This is an explicitly **test-only, non-production** Component fixture for the
normative target ABI in `docs/specs/api-next.wit`. Dust production loading,
manifests, the SDK, and `wit/dust-plugin.wit` remain on 0.2.

`target03-clone-guest.core.wasm` is the checked no-WASI core module generated
from `src/lib.rs`; tests wrap it with `wit-component` and instantiate the real
Component through Wasmtime. Its strict target fixture manifest admits retained
and private content cloning. The guest opens and clones both presenters,
observes exact `ui-result` and dimensions callbacks, and publishes fresh cells
for every private view. It also calls one deliberately unimplemented import so
the host's typed `unsupported` rejection is exercised by actual guest code.

Rebuild with the installed stable toolchain and checked lockfile:

```sh
cargo build --locked --offline --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/dust_target03_clone_guest.wasm \
  target03-clone-guest.core.wasm
rm -rf target
sha256sum target03-clone-guest.core.wasm
```

Expected SHA-256:
`499d54fbd933b523ebafa9c689b9c44e7fcc0463570e6f66a26d04f3cf34abb9`.
