# Retained UI Component fixture

`retained-guest.core.wasm` is a deterministic `wasm32-unknown-unknown` core
module generated from `src/lib.rs` and the checked-in WIT copy. Tests wrap the
embedded Component metadata with `wit-component` and instantiate the result
through Dust's production no-WASI Wasmtime Component path. Tests never invoke
Cargo, rustc, a shell, or another compiler.

Reproduction (maintainer-only, outside test execution):

```text
cargo +stable build --locked --release --target wasm32-unknown-unknown
```

Copy `target/wasm32-unknown-unknown/release/dust_retained_guest.wasm` to
`retained-guest.core.wasm`. Expected SHA-256:

```text
2b87dac4f0aa9998ec86d41ee26e1eb329d7cedf09d0a39efacaa88c61b601ec
```

The fixture has no WASI imports. It drives retained open/full-tree replacement,
placement, focus, close, private open/local-geometry/full-snapshot/patch/action/close,
typed queue exhaustion, asynchronous event draining, a trap after enqueue, and
fuel/epoch interruption after enqueue, bounded later-execution host-event delivery/acknowledgement, queued private-storage CAS/get/stale-CAS/list/delete/missing-result feedback, picker-source request/two-chunk/completion/late-cancellation flow, and read-only virtual-document initial Unicode/full plus exact-revision patch requests. Fixture source and WIT/SDK artifacts are licensed
under Apache-2.0; the repository test harness remains GPL-3.0-or-later.
