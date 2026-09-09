# Dust Component Plugin SDK 0.1

The public WIT ABI and this SDK contract are licensed Apache-2.0; see
[`LICENSES/Apache-2.0.txt`](../LICENSES/Apache-2.0.txt). Dust editor implementation code remains
GPL-3.0-or-later.

The authoritative ABI is [`wit/dust-plugin.wit`](../wit/dust-plugin.wit), package `dust:plugin@0.1.0`, world `editor-plugin`. Third-party editor plugins must be WebAssembly Components implementing that world. Core Wasm modules, WASI applications, native libraries, and subprocess shims are not plugins.

## Theme support

Component plugins currently have **no theme-contribution API**: contribution
schema 1 has no `themes` field, and WIT exposes no global palette setter.
Private/retained UI can use host semantic styles, but this does not register a
selectable editor theme. Custom themes currently use validated Dust TOML files
in the user's `themes/` directory; see [the theme guide](user-guide-linux.md#select-a-theme).
Native Neovim/Lua themes are not executable Dust plugins. Declarative plugin
palette contributions and theme-only package installation remain unimplemented.

## Lifecycle

1. Dust reads and bounds the manifest from its immutable content-addressed store without reading Component bytes. Every manifest has an explicit contribution schema version and declares namespaced input contexts, typed commands/argument schemas, suggested mappings, key groups, retained/private content types, bounded subscriptions, supported provider metadata, and optional storage quota (empty arrays are valid). A content declaration owns its stable `plugin.<owner>.<content>` ID, title, presenter kind, default and allowed ordinary placements, focusability, `1..=16` instance ceiling, input context, and declared command linkage.
2. The worker canonicalizes and validates the static index. The application installs exact plugin/load/checksum/generation provenance, registers unavailable typed command proxies, and compiles `Bundled < Plugin < User`. When user declarations depend on enabled plugins at startup, Dust waits for every expected index and accepts one complete roster transactionally.
3. Only after static acceptance does the worker read the Component, verify the declared SHA-256 and Component Model binary, and instantiate it with generated WIT bindings, no WASI, cleared ambient authority, Store memory/table/instance limits, fuel, and an epoch deadline.
4. `activate()` returns command names. Every returned name must equal a static manifest declaration. Dust replaces the unavailable proxy for `plugin.<plugin-id>.<command>` with an active handler for that exact generation.
5. `execute-command(command, arguments)` runs in the persistent worker-owned instance. Traps, fuel exhaustion, deadline interruption, malformed output, or a host-bound violation fail and isolate that plugin.
6. Disable, failure, package rollback/removal, or worker loss removes exact-generation command, context, mapping, group, pending-hint, subscription/event/ack state, opaque UI handle, retained tree, graph Content/View/Window, and runtime ownership. Every load has a monotonic owner ID: indexed/loaded/failed/executed events, status changes, prepared-token acknowledgments, and conditional disables are accepted only for the current ID. Rejected stale loads cannot reinstall contributions, register commands, execute, commit a package token, overwrite status, or disable a replacement runtime. Cancellation removes foreground ownership immediately; if the bounded worker queue is full, an exact conditional disable remains pending and must be ordered before a same-ID replacement load. Current loads and pending disables are each bounded by the validated plugin queue capacity; excess loads fail explicitly. Worker disconnection clears static/runtime ownership and aborts unacknowledged prepared tokens.
7. Static acceptance never commits a package lock. The package worker publishes it only after the exact runtime generation reports `Loaded`. Live add/remove/change of configured plugin entries is currently rejected before mutation and requires restart.

A retained content declaration has this manifest shape:

```toml
[[contributions.content_types]]
id = "plugin.demo.panel"
title = "Demo panel"
presenter = "retained_declarative"
default_placement = "floating"
allowed_placements = ["docked", "floating", "hidden"]
focusable = true
maximum_instances = 1
context = "plugin.demo.panel-input"
command = "plugin.demo.open"
```

The context and command must be declared in the same statically accepted manifest. A private-cell presenter additionally requires the deny-by-default `private_cells` capability and an explicit policy:

```toml
capabilities = ["private_cells"]

[[contributions.content_types]]
id = "plugin.demo.cells"
title = "Demo cells"
presenter = "private_cells"
default_placement = "floating"
allowed_placements = ["docked", "floating", "hidden"]
focusable = true
maximum_instances = 1
context = "plugin.demo.cells-input"
command = "plugin.demo.cells-action"
private_cells = { maximum_cells = 4096, maximum_patch_cells = 2048, maximum_dirty_rectangles = 64, maximum_styles = 32, maximum_hit_regions = 64, maximum_bytes = 65536, cursor = true }
```

Smaller values are recommended. Zero, host-ceiling excess, policy/presenter mismatch, absent capability, namespace theft, and aggregate private declarations above 8,192 cells reject before Component bytes. User `[windows]` rules may name a plugin content ID only after its enabled static index proves that declaration.

## Host authority

Capabilities are deny-by-default and checked on every host call:

- `read-current-buffer`: returns one immutable UTF-8 snapshot with stable buffer and revision IDs.
- `propose-edits`: accepts bounded byte ranges against that exact snapshot. Dust converts valid output into its ordinary workspace-edit preview; the component never mutates text or disk directly.
- `show-message`: accepts bounded terminal-sanitized text.
- `read-config`: reads only the plugin's bounded namespace.
- `private-cells`: permits only statically declared private presenters within their accepted per-content maxima; it grants no global terminal or protected-layer access.
- `event-subscriptions`: requires one or more static namespaced subscriptions. The delivered kinds are `workspace-metadata-changed`, `active-surface-changed`, `buffer-summary-changed`, and `plugin-surface-changed`; queue/rate maxima and mandatory coalescing are accepted before Component bytes.
- `picker-providers`: permits only statically declared `picker-source` requests under the declaration's concurrency/item/byte/deadline policy. Other provider kinds remain statically reserved but runtime-unsupported.
- `plugin-storage`: requires one static quota declaration and permits only queued logical-key storage calls. It grants no path, namespace, general filesystem, secret, or cross-plugin access.
- `queue-ui-request`: enqueues at most 64 retained-window proposals during one component execution. It never mutates the graph in the component worker. A trapped/failed execution publishes none of its queued proposals. The application later checks the exact active load/checksum/catalog generation, expected graph or tree revision, declared content/placement/instance policy, active workspace tab, graph ceilings, close order, and protected compositor boundary before clone-validate-commit.
- `take-ui-events`: drains bounded asynchronous results on a later execution. Events expose only an opaque generation-bound lifecycle handle plus accepted revisions; they never expose Rust references or raw graph authority. UI failures carry a stable `ui-error-code` (`queue-full`, stale graph/tree/handle, foreign generation, undeclared content/action, disallowed placement, hidden focus, cross-tab, instance/graph ceiling, invalid tree/placement, close order, generation loss, wrong presenter, stale surface/dimensions, invalid local bounds/grapheme/style/cursor/hit region, overlap/split-wide, patch ceiling, or internal rejection) plus a sanitized detail capped at 4 KiB. Guests branch on the code, not detail text.

`take-host-events` delivers at most 64 immutable envelopes only during a later command execution. Each envelope has a host-issued ID, monotonic sequence, typed kind, revision, bounded payload, and coalesced/dropped counters. `acknowledge-host-events` accepts at most 64 unique nonzero IDs; only the exact active generation can retire them. Unchanged observations do not enqueue or execute a guest, and high-frequency changes replace the pending envelope for their kind. There is no synchronous event callback.

### Picker-source provider v1

A statically accepted `picker-source` declaration owns its full provider ID, exact plugin action command, concurrency ceiling (1–8), item ceiling (up to 4,096), aggregate bytes (up to 1 MiB), and deadline (10–30,000 ms). `picker.plugin.open provider=plugin.demo.provider.files query=…` starts a host-owned request while leaving bundled file/grep/command pickers unchanged. The request is delivered only during the declaration command's later `execute-command` call and contains a host ID, bounded query/context, workspace/buffer revisions, cancellation generation, deadline, and accepted ceilings.

Guests queue strictly sequenced chunks containing only local item ID, primary/secondary text, and opaque action bytes plus `more`, `complete`, `truncated`, or typed-error terminal state. Items cannot name commands. The broker clone-validates exact plugin/load/provider/request ownership, context revisions, deadline, sequence, duplicate IDs, and per-declaration count/bytes before commit. It owns deterministic query matching/ranking and projects accepted rows through the existing generic picker. Re-query/close/unload cancels ownership before late chunks; exact replacement generations survive stale cleanup. A successful item selection may invoke only the declaration's exact plugin command. Opaque action data is retained for identity/limits but is not passed to command arguments in v1.

There is no synchronous provider callback, provider-side editor/graph mutation, or replacement of bundled picker sources. Preview/action/diagnostics/completion/hover/decoration/status/virtual-document providers remain pending.

### Read-only virtual documents — broker Stage 1

With the deny-by-default `virtual_documents` capability, a plugin may statically declare bounded, namespaced read-only document sources. A declaration fixes title, language, exact no-required-argument command/context, concurrent-document count, document byte/line ceilings, patch byte/count ceilings, and deadline before Component bytes are read. The queued WIT protocol exposes only host request/document handles, a bounded opaque key, cancellation generation, current revision, and accepted limits. It never exposes paths, URIs, editor/graph IDs, cells, or callbacks.

The exact-generation broker accepts initial full text and exact-revision full/ordered UTF-8-safe patch proposals using clone-validate-commit. It rejects controls other than tab/newline, title/language changes, stale/foreign/late proposals, codepoint-splitting or overlapping patches, and budget violations atomically. Trap/fuel/deadline failures publish no proposals. Stage 2A adds an explicit runtime-only `VirtualReadOnly` buffer origin and central transaction guards (including undo/redo), host-revision-only text replacement, save/reload rejection, recovery exclusion, and path-free session omission. Stage 2B wires accepted initial and exact-revision refresh snapshots through a host-private bounded editor bridge. `buffer.virtual.open source=plugin.<owner>.<source> key=<opaque>` schedules only the static source command; accepted text opens as ordinary tiled editor content, and `buffer.virtual.refresh` preserves buffer/view/window identities and cursor/scroll. Closing the final view cancels exact broker ownership. Only tiled/pathless/read-only text is delivered. The additive `health.virtual.open` / `.refresh` host source dogfoods the same publication bridge with bounded Markdown while keeping the existing health surface. Non-tiled or writable virtual documents are unsupported, and runtime ownership/text remains absent from persistence.

### Plugin-private storage v1

A plugin requesting `plugin_storage` must declare `[contributions.storage]` with `maximum_items`, `maximum_value_bytes`, and `maximum_total_bytes`. Host maxima are 4,096 items, 256 KiB/value, and 4 MiB total. Keys use a deliberately narrow normalized v1 alphabet: 1–128 bytes of lowercase ASCII, digits, dot, underscore, or dash; leading/trailing dot, repeated dot, empty, `.`/`..`, controls, slash, backslash, uppercase, and non-ASCII reject. An empty prefix is allowed only for bounded listing.

`queue-storage-request` accepts at most 64 get, compare-and-set, expected-revision delete, or sorted prefix-list requests per successful Component command. A trap/fuel/deadline failure publishes none. Results arrive only through `take-storage-results` on a later command. Records have nonzero monotonic revisions; CAS with no expected revision requires absence, and all other mutations require exact revision ownership. Stable typed errors distinguish queue, key/value, stale revision, quota, unavailable worker, corruption, and generation loss.

A dedicated bounded worker derives a fixed SHA-256 filename from the host-owned plugin ID, validates a strict checksummed schema under Dust's user state directory, clone-validates quotas, and writes create-new temporary files with flush/sync and same-directory atomic rename. It rejects symlinks, non-files, oversize, malformed schemas, duplicate keys/revisions, checksum mismatch, and revision exhaustion without replacing prior state. Guest keys never become paths. Runtime requests/results are exact `(plugin, PluginLoadId)` ownership and are canceled by an internal epoch on session replacement; persistent records are stable by plugin ID across valid upgrades, disable, uninstall, and package rollback. Manual operator deletion is currently the only data-erasure path. This storage is not encrypted and must not contain secrets.

There is no ambient filesystem, network, environment, clock, random, process, terminal, VFS, LSP, or DAP access. Project trust does not grant a component additional host capabilities.

## Retained declarative UI v1

The current public presenter is a bounded flat retained tree with stable `u64` node IDs and `container`, `text`, `list-node`, and `button` kinds. Nodes use semantic styles, bounded text, optional statically declared button-command actions, and child-ID lists. Full-tree replacement carries an expected previous tree revision. The host rejects zero/stale revisions, duplicate/missing/unreachable nodes, cycles, depth above 32, more than 256 nodes, more than 1,024 child references, per-node text above 4 KiB, aggregate text above 64 KiB, undeclared actions, and non-button actions.

Open/place/focus/close requests carry an expected monotonic WindowGraph revision. One opaque lifecycle handle intentionally owns one associated host-private Content/View/Window triple; those three identities are never serialized or exposed in WIT. The handle is valid only for the exact plugin ID, `PluginLoadId`, checksum/catalog generation, active workspace tab, and owned declaration. Retained snapshots are rendered from immutable application state inside the final compositor rectangle. Tiny rectangles clip; hidden focus, stale/foreign/cross-generation handles, cross-tab operations, disallowed placements, excess instances, conflicting fullscreen, and graph ceilings reject without partial mutation. Host trust/permission/crash regions remain above plugin windows and suppress ordinary hit testing.

The host owns focused stable-node identity and bounded list scroll per handle. Full-tree replacement retains focus only when that node remains selectable, otherwise choosing the first list/button node, and clamps scroll. Plugins may suggest mappings in their declared dynamic context to the narrow host command allowlist `surface.retained.focus.next`, `surface.retained.focus.previous`, `surface.retained.scroll.up`, `surface.retained.scroll.down`, `surface.retained.activate`, and `surface.retained.mouse.select`; arbitrary host commands remain invalid static contributions. Activation revalidates the current button action against the exact manifest generation and enters ordinary asynchronous plugin-command dispatch. Mouse hit testing is compositor-clipped and protected host regions win.

## Private cell surface v1

Private cells are a lower-level retained presenter, not a framebuffer. After open, Dust asynchronously reports only local width/height and a monotonic dimensions revision. Hidden, zero-sized, missing, or stale projections are not writable. `replace-private` installs a full local snapshot; `patch-private` carries expected surface/dimensions revisions, non-overlapping dirty rectangles, style updates, cells, optional cursor replacement, and optional hit-region replacement. Geometry changes invalidate old cells and publish a later local-only geometry event.

A lead cell contains exactly one printable nonzero-width grapheme. Width-two leads require the adjacent explicit empty continuation cell; patches cannot split them. Coordinates, dirty rectangles, cursor, and non-overlapping hit regions stay inside local dimensions. Style IDs are unique and nonzero. Hit regions currently resolve only the declaration's exact plugin command. Full state is clone-validated before publication; stale/invalid/overlapping/out-of-bounds/excess requests preserve the prior snapshot. Rendering maps local cells through the compositor rectangle, clips again, and shows the local cursor only while that exact graph window is focused. Protected host regions paint last and suppress hits.

`surface.private.activate` resolves the focused private cursor and `surface.private.mouse.activate` resolves a compositor-clipped local mouse hit. They enqueue a bounded semantic action event and schedule the declared plugin command through the asynchronous worker. Plugins may suggest only those commands plus the six `surface.retained.*` commands; arbitrary host commands remain invalid contributions. No global coordinate is delivered.

There is no immediate-mode callback, synchronous input/render hook, global framebuffer/global-cell access, plugin protected layer, external-TUI attachment, or ambient WASI. The checked-in no-WASI fixture exercises retained and private open/geometry/snapshot/patch/action/close through production Wasmtime without a compiler or subprocess during tests. First-party replacement remains pending.

## Command and edit requirements

Command names and manifest IDs must form valid Dust command-ID segments. Arguments and results are bounded by the host. Edit offsets are UTF-8 byte offsets, must be ordered and on code-point boundaries, and must carry the snapshot's buffer/revision. Stale proposals are discarded. Accepted proposals create dirty buffers and never save files.

## Package requirements

Registry sources require an exact version and SHA-256; Git sources require an exact commit and tree hash. Development path sources are marked non-reproducible. Dust verifies store objects and Component binaries before lock publication. The official HTTPS registry client is fixed to `https://dust.blurskye.dev/registry/v1`, follows no redirects, applies independent deadlines and streamed byte ceilings, verifies artifact SHA-256, and uses an integrity-checked bounded offline cache. Direct Git network fetching remains explicitly unsupported until an equivalent non-CLI exact-commit/tree implementation exists.
