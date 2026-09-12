# Retained UI Component fixture

`retained-guest.core.wasm` is a deterministic `wasm32-unknown-unknown` core
module generated from `src/lib.rs` and the checked-in byte-identical copy of
production `wit/dust-plugin.wit` (`dust:plugin@0.2.0`, world `editor-plugin`).
Tests wrap the embedded Component metadata with `wit-component` and instantiate
the result through Dust's production no-WASI Wasmtime Component path. Tests
never invoke Cargo, rustc, a shell, or another compiler. This fixture is the
maintained 0.2 authoring example, not a published SDK crate or evidence for the
incompatible target 0.3 host/package contract.

Reproduction (maintainer-only, outside test execution):

```text
cargo +stable build --locked --release --target wasm32-unknown-unknown
```

Copy `target/wasm32-unknown-unknown/release/dust_retained_guest.wasm` to
`retained-guest.core.wasm`. Expected SHA-256:

```text
3773ae204a9b1fe0436f2252161663562be895aea2392b1b4bd187c43af74416
```

The fixture has no WASI imports. It drives retained open/full-tree replacement,
placement, focus, close, private open/local-geometry/full-snapshot/patch/action/close,
typed queue exhaustion, asynchronous event draining, a trap after enqueue, and
fuel/epoch interruption after enqueue, bounded later-execution host-event delivery/acknowledgement, queued private-storage CAS/get/stale-CAS/list/delete/missing-result feedback, picker-source request/two-chunk/completion/late-cancellation flow, read-only virtual-document initial Unicode/full plus exact-revision patch requests, and the two-step confirmation example: one command opens a retained button view, its real action requests the host dialog, and a result-only execution replaces that existing view with Yes/No/Cancelled state. Its capacity probe consumes result-only callbacks without issuing another request and emits a guest-owned ordinary invocation ordinal, so the 65th ordinary probe unambiguously observes typed `queue-full` from the actual aggregate 64-item host lane. Fixture source and WIT/SDK artifacts are licensed
under Apache-2.0; the repository test harness remains GPL-3.0-or-later.
