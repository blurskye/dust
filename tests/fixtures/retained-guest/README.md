# Maintained Dust component example

`retained-guest.core.wasm` is built from `src/lib.rs` and a byte-identical copy of
production `wit/dust-plugin.wit`: **dust:plugin@0.4.0**, world `editor-plugin`.
It has no WASI imports. Tests wrap its embedded metadata with `wit-component` and
instantiate it with the real production host; tests never invoke a compiler.

Build from the repository root:

```sh
nix develop .#guest
cd tests/fixtures/retained-guest
cargo build --locked --release --target wasm32-unknown-unknown --target-dir target
cp target/wasm32-unknown-unknown/release/dust_retained_guest.wasm retained-guest.core.wasm
```

Keep the fixture-digest test assertions current when rebuilding. The `guest`
Nix shell includes the Wasm target; the editor's normal build shell does not.
When changing the ABI, copy `wit/dust-plugin.wit` from the repository root to this
fixture's `wit/dust-plugin.wit` before rebuilding.

The example demonstrates retained and private-cell panels, confirmations, storage,
picker chunks, read-only virtual documents, traps and runaway interruption. Its
`drive` handler also demonstrates typed command values/results, dynamic registration,
run/wrap/call-next, named modes/capture/layers, anchored marks, occurrence batches,
menu/inline completion, incoming LSP filters, timers and HTTP denial. Host tests
instantiate it twice to verify wrapper ordering and held input.

Guest source, WIT and SDK documentation are Apache-2.0. The repository's test
harness is GPL-3.0-or-later.
