# Dust Component Plugin SDK 0.1

The public WIT ABI and this SDK contract are licensed Apache-2.0; see
[`LICENSES/Apache-2.0.txt`](../LICENSES/Apache-2.0.txt). Dust editor implementation code remains
GPL-3.0-or-later.

The authoritative ABI is [`wit/dust-plugin.wit`](../wit/dust-plugin.wit), package `dust:plugin@0.1.0`, world `editor-plugin`. Third-party editor plugins must be WebAssembly Components implementing that world. Core Wasm modules, WASI applications, native libraries, and subprocess shims are not plugins.

## Lifecycle

1. Dust reads and bounds the manifest from its immutable content-addressed store without reading Component bytes. Every manifest has an explicit contribution schema version and declares namespaced input contexts, typed commands/argument schemas, suggested mappings, and key groups (empty arrays are valid).
2. The worker canonicalizes and validates the static index. The application installs exact plugin/load/checksum/generation provenance, registers unavailable typed command proxies, and compiles `Bundled < Plugin < User`. When user declarations depend on enabled plugins at startup, Dust waits for every expected index and accepts one complete roster transactionally.
3. Only after static acceptance does the worker read the Component, verify the declared SHA-256 and Component Model binary, and instantiate it with generated WIT bindings, no WASI, cleared ambient authority, Store memory/table/instance limits, fuel, and an epoch deadline.
4. `activate()` returns command names. Every returned name must equal a static manifest declaration. Dust replaces the unavailable proxy for `plugin.<plugin-id>.<command>` with an active handler for that exact generation.
5. `execute-command(command, arguments)` runs in the persistent worker-owned instance. Traps, fuel exhaustion, deadline interruption, malformed output, or a host-bound violation fail and isolate that plugin.
6. Disable, failure, package rollback/removal, or worker loss removes exact-generation command, context, mapping, group, pending-hint, and runtime ownership. Every load has a monotonic owner ID: indexed/loaded/failed/executed events, status changes, prepared-token acknowledgments, and conditional disables are accepted only for the current ID. Rejected stale loads cannot reinstall contributions, register commands, execute, commit a package token, overwrite status, or disable a replacement runtime. Cancellation removes foreground ownership immediately; if the bounded worker queue is full, an exact conditional disable remains pending and must be ordered before a same-ID replacement load. Current loads and pending disables are each bounded by the validated plugin queue capacity; excess loads fail explicitly. Worker disconnection clears static/runtime ownership and aborts unacknowledged prepared tokens.
7. Static acceptance never commits a package lock. The package worker publishes it only after the exact runtime generation reports `Loaded`. Live add/remove/change of configured plugin entries is currently rejected before mutation and requires restart.

## Host authority

Capabilities are deny-by-default and checked on every host call:

- `read-current-buffer`: returns one immutable UTF-8 snapshot with stable buffer and revision IDs.
- `propose-edits`: accepts bounded byte ranges against that exact snapshot. Dust converts valid output into its ordinary workspace-edit preview; the component never mutates text or disk directly.
- `show-message`: accepts bounded terminal-sanitized text.
- `read-config`: reads only the plugin's bounded namespace.

There is no ambient filesystem, network, environment, clock, random, process, terminal, VFS, LSP, or DAP access. Project trust does not grant a component additional host capabilities.

## Command and edit requirements

Command names and manifest IDs must form valid Dust command-ID segments. Arguments and results are bounded by the host. Edit offsets are UTF-8 byte offsets, must be ordered and on code-point boundaries, and must carry the snapshot's buffer/revision. Stale proposals are discarded. Accepted proposals create dirty buffers and never save files.

## Package requirements

Registry sources require an exact version and SHA-256; Git sources require an exact commit and tree hash. Development path sources are marked non-reproducible. Dust verifies store objects and Component binaries before lock publication. The official HTTPS registry client is fixed to `https://dust.blurskye.dev/registry/v1`, follows no redirects, applies independent deadlines and streamed byte ceilings, verifies artifact SHA-256, and uses an integrity-checked bounded offline cache. Direct Git network fetching remains explicitly unsupported until an equivalent non-CLI exact-commit/tree implementation exists.
