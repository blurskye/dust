# Target 0.3 clone Component fixture

This is an explicitly **test-only, non-production** Component fixture for the
normative target ABI in `docs/specs/api-next.wit`. Dust production loading,
manifests, the SDK, and `wit/dust-plugin.wit` remain on 0.2.

`target03-clone-guest.core.wasm` is the checked no-WASI core module generated
from `src/lib.rs`; tests wrap it with `wit-component` and instantiate the real
Component through Wasmtime. It opens and clones retained content and observes
exact `ui-result` callbacks. It also calls one deliberately unimplemented
import so the host's typed `unsupported` rejection is exercised by actual guest
code.

Rebuild with the installed stable toolchain and checked lockfile:

```sh
cargo build --locked --offline --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/dust_target03_clone_guest.wasm \
  target03-clone-guest.core.wasm
rm -rf target
sha256sum target03-clone-guest.core.wasm
```

Expected SHA-256:
`5aeedf6233f86a9b753829835cae404699b6238a04d0eebcb27530f5b3e784e2`.
