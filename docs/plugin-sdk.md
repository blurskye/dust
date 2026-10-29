# Dust Component Plugin SDK 0.2

The public WIT ABI and this SDK contract are licensed Apache-2.0; see
[`LICENSES/Apache-2.0.txt`](../LICENSES/Apache-2.0.txt). Dust editor implementation code remains
GPL-3.0-or-later.

The authoritative ABI is [`wit/dust-plugin.wit`](../wit/dust-plugin.wit), package `dust:plugin@0.2.0`, world `editor-plugin`. Third-party editor plugins must be WebAssembly Components implementing that world. Core Wasm modules, WASI applications, native libraries, and subprocess shims are not plugins. This is an intentional pre-release ABI break: 0.1 Components must be rebuilt against the 0.2 WIT. The same pre-release slice removes the native `explorer.delete.confirm` command and its `ExplorerDeleteConfirm` application effect; deletion now submits the typed shared confirmation directly, so integrations must not dispatch that removed phrase-era surface.

## Shipped boundary and authoring availability

The checkout ships the production WIT, generated host bindings, strict manifest loader and a maintained no-WASI Rust guest fixture. It does **not** ship a separate published Rust SDK crate, package-schema generator, dev runner or mock-host/simulator. The fixture is the current buildable example; its pinned `wit-bindgen` dependency is not a promise of a generally published SDK package.

The incompatible target is [`docs/plugin-abi-next.wit`](plugin-abi-next.wit), package `dust:plugin@0.3.0`. It is not imported by production and cannot be selected through a manifest or command-line flag. Production has no 0.3 `features`, `limits`, `handle-events`, `deactivate`, interaction, snapshot-token, grant, service, cancellation or handle-release path. A future promotion requires a replacement host, rebuilt maintained guest/SDK/fixtures and explicit ABI/unsupported-feature tests; changing a version string, reusing the world name, adapting integers or structurally matching types is not compatibility.

The production fixture mirrors the authoritative WIT byte-for-byte and is wrapped and instantiated by tests. The separate `target03-clone-guest` and `src/extensions/target03.rs` lane is compiled only under `cfg(test)`: it exercises a partial target UI protocol with an ad hoc fixture admission record. It is not an accepted production Component, a schema-2 package, a complete target host or X-08 promotion evidence.

## Theme support

Component plugins currently have **no theme-contribution API**: contribution
schema 1 has no `themes` field, and WIT exposes no global palette setter.
Private/retained UI can use host semantic styles, but this does not register a
selectable editor theme. Custom themes currently use validated Dust TOML files
in the user's `themes/` directory; see [the theme guide](user-guide-linux.md#select-a-theme).
Native Neovim/Lua themes are not executable Dust plugins. Declarative plugin
palette contributions and theme-only package installation remain unimplemented.

## Current plugin manifest (contribution schema 1)

Production reads a strict TOML `PluginManifest`; it does not read `dust-package.toml`. Unknown fields fail because every manifest record is closed. The required top-level fields are `id`, `version`, `component_sha256`, `capabilities`, `commands` and `contributions`. Plugin IDs are 1–64 lowercase ASCII letters/digits beginning with a letter; versions are nonempty control-free strings up to 128 bytes but are not currently parsed as SemVer; the component digest is 64 hexadecimal characters and is compared case-insensitively. `capabilities` is a set containing only the nine names below. `commands` is a map of local command IDs (the same 1–64 character shape), bounded by `plugins.maximum_commands` (default 256, accepted configuration range 1–4096).

Each command record has required `title` (1–256 bytes after nonempty-trim validation), `context` (`application`, `workspace`, `view`, `buffer` or `selection`) and `activation = "active_only"`; `description` defaults to empty and is at most 2,048 bytes. `arguments` defaults to an empty map, admits at most 32 names, and each closed declaration has `kind = "string" | "boolean" | "integer"` plus `required` defaulting to false. The public command is `plugin.<plugin-id>.<local-command>`.

Configured entries are `[plugins.entries.<expected-id>]` records with required relative `manifest`, relative `component` and Boolean `enabled` fields. Neither path may be absolute or contain `..`; they resolve below `plugins.store`, which itself resolves below the user state directory when relative. The entry key must equal the parsed manifest ID. Safe mode skips configured loading.

`contributions.schema_version` defaults to 1 and must equal 1. Its arrays default empty and its optional `storage` record defaults absent:

| Contribution | Current accepted shape and principal bounds |
|---|---|
| `input_contexts` | At most 64 `{id,title,parent_surface?}` records. IDs are `plugin.<owner>.<local>`; title is at most 256 bytes/128 display columns; parent, when present, is `panel`, `picker`, `explorer` or `terminal`. |
| `keymaps` | At most 256 `{id,context,mode="",keys,command,arguments={}}` records. A sequence is at most 1,024 bytes/64 tokens. Command arguments must match the static declaration. Only same-plugin commands or the documented retained/private host-command allowlist are accepted. |
| `key_groups` | At most 64 `{id,context,mode="",prefix,label,icon?,order=0}` records. Prefix uses the key-sequence bounds; label is 1–256 bytes; an icon is at most 32 bytes/eight characters and has nonzero display width. |
| `content_types` | At most 64 closed records with namespaced `id`, nonempty `title`, `presenter`, `default_placement`, nonempty `allowed_placements`, `focusable=true`, `maximum_instances=1` (accepted 1–16), `context`, `command`, and presenter-matched `private_cells?`. Placements are `docked`, `floating`, `fullscreen`, `hidden`. |
| `subscriptions` | At most 32 `{id,kind,maximum_queued,maximum_rate_per_second,coalesce=true}` records. Kinds are `workspace-metadata-changed`, `active-surface-changed`, `buffer-summary-changed` and `plugin-surface-changed`; queue is 1–64, rate 1–120 and aggregate queued capacity at most 128. The matching capability and at least one declaration are both required. |
| `providers` | At most 32 `{id,kind,command,maximum_concurrent_requests,maximum_items,maximum_bytes,deadline_milliseconds}` records. Only `kind = "picker-source"` has a runtime contract; its command has no required arguments, concurrency is 1–8, items 1–4,096, bytes 1–1 MiB and deadline 10–30,000 ms. Aggregate declared provider bytes are at most 4 MiB. |
| `virtual_documents` | At most 32 `{id,title,language,mode,maximum_concurrent_documents,maximum_document_bytes,maximum_document_lines,maximum_patch_bytes,maximum_patches_per_request,deadline_milliseconds,command,context}` records. The only current mode spelling is `"ReadOnly"`; command has no required arguments, concurrency is 1–16, bytes 1–1 MiB, lines 1–100,000, patch bytes 1–256 KiB, patches/request 1–256 and deadline 10–30,000 ms. Aggregate declared document bytes are at most 4 MiB. |
| `storage` | One optional `{maximum_items,maximum_value_bytes,maximum_total_bytes}` record: 1–4,096 items, 1–256 KiB/value and 1–4 MiB total, with value no larger than total. It and `plugin_storage` must either both be present or both be absent. |

All contribution IDs are at most 128 bytes, namespaced to the plugin and unique across contribution kinds. Static conflicts, capability/declaration mismatch, invalid context or command linkage, duplicate key sequences/prefixes, and limit-plus-one fail before Component bytes are read. [`src/extensions/manifest.rs`](../src/extensions/manifest.rs) and [`src/extensions/index.rs`](../src/extensions/index.rs) remain authoritative for less common field-level checks.

## Shared confirmation (delivered first template)

`queue-confirmation` requests one ordinary host-owned dialog with a bounded title,
wrapped message and a Yes/No default. `take-confirmation-results` returns exactly one
later `Yes`, `No`, `Cancelled`, or typed failure outcome during an admitted execution
of the requesting command. Destructive requests should default to `No`. Global capacity
is reserved before the guest call; when all 64 interaction/result slots are occupied,
`queue-confirmation` returns `queue-full` and creates no token. A request is bound to
its original document incarnation, editor, workspace, configuration, input capture,
navigation, invocation, plugin and load. A still-live guest receives terminal
`source-retired` if those original sources change; a dead/replaced load is discarded
without delivery to its replacement. The continuation receives no current-buffer
snapshot and may only replace/patch an existing view from the same plugin load: edit,
filesystem, storage, provider, virtual-document, new-interaction and focus/window
requests are refused by host calls before a token or proposal is accepted. The full whitespace-preserving message must fit before buttons
or submission are available. The host visibly prefixes plugin provenance and plugins
cannot use protected trust/reset UI.

The host also exposes the current immutable UI admission revision needed by queued
retained-window proposals. The maintained retained guest uses a real two-step flow:
its first command opens a custom retained button view, activating that button invokes
the confirmation command, and the result-only execution replaces the same view with
distinct Yes/No/Cancelled state. A separate probe exercises typed `queue-full` against
the actually saturated aggregate host lane.
This delivers only the confirmation template. Input, richer picker payloads,
list/tree/table, form and preview/output templates remain planned; retained/private
UI remains available for custom compositions and behavior.

## Lifecycle

1. Dust reads and bounds the manifest from its immutable content-addressed store without reading Component bytes. Every manifest has an explicit contribution schema version and declares namespaced input contexts, typed commands/argument schemas, suggested mappings, key groups, retained/private content types, bounded subscriptions, supported provider metadata, and optional storage quota (empty arrays are valid). A content declaration owns its stable `plugin.<owner>.<content>` ID, title, presenter kind, default and allowed ordinary placements, focusability, `1..=16` instance ceiling, input context, and declared command linkage.
2. The worker canonicalizes and validates the static index. The application installs exact plugin/load/checksum/generation provenance, registers unavailable typed command proxies, and compiles `Bundled < Plugin < User`. When user declarations depend on enabled plugins at startup, Dust waits for every expected index and accepts one complete roster transactionally.
3. Only after static acceptance does the worker read the Component, verify the declared SHA-256 and Component Model binary, and instantiate it with generated WIT bindings, no WASI, cleared ambient authority, Store memory/table/instance limits, fuel, and an epoch deadline.
4. `activate()` returns command names. Every returned name must equal a static manifest declaration. Dust replaces the unavailable proxy for `plugin.<plugin-id>.<command>` with an active handler for that exact generation.
5. `execute-command(command, arguments)` runs in the persistent worker-owned instance. Traps, fuel exhaustion, deadline interruption, malformed output, or a host-bound violation fail and isolate that plugin.
6. Disable, failure, package rollback/removal, or worker loss removes exact-generation command, context, mapping, group, pending-hint, subscription/event/ack state, opaque UI handle, retained tree, graph Content/View/Window, and runtime ownership. Every load has a monotonic owner ID: indexed/loaded/failed/executed events, status changes, prepared-token acknowledgments, and conditional disables are accepted only for the current ID. Rejected stale loads cannot reinstall contributions, register commands, execute, commit a package token, overwrite status, or disable a replacement runtime. Cancellation removes foreground ownership immediately; if the bounded worker queue is full, an exact conditional disable remains pending and must be ordered before a same-ID replacement load. Current loads and pending disables are each bounded by the validated plugin queue capacity; excess loads fail explicitly. Worker disconnection clears static/runtime ownership and aborts unacknowledged prepared tokens.
7. Static acceptance never commits a package lock. The package worker publishes it only after the exact runtime generation reports `Loaded`. Live add/remove/change of configured plugin entries is currently rejected before mutation and requires restart.

Bundled runtime policy defaults are:

| Setting | Default | Current configuration admission |
|---|---:|---:|
| `plugins.queue_capacity` | 8 | 1–64; this is both the worker queue/current-load ceiling and the plugin-storage worker capacity |
| `plugins.maximum_component_bytes` | 64 MiB | 1 KiB–256 MiB |
| `plugins.maximum_commands` | 256 | 1–4,096 |
| `plugins.fuel` | 10,000,000 | Any nonzero `u64` |
| `plugins.epoch_deadline_ticks` | 10 | `u64`; current command execution schedules one epoch increment after `ticks * 10 ms` |
| `plugins.maximum_memory_bytes` | 64 MiB | `usize` passed to Wasmtime Store limits |
| `plugins.maximum_table_elements` | 10,000 | `u32` passed to Wasmtime Store limits |
| `plugins.maximum_instances` | 16 | `u32` passed to Wasmtime Store limits |

The last four Store/deadline values do not currently have the target X-07 range validator. These are production defaults and parser behavior, not a claim that all configurable production values satisfy target 0.3 ceilings. Manifest/broker ceilings in this document remain hard current checks where stated.

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

## Exact production WIT surface and authority

The manifest capability spellings are `read_current_buffer`, `propose_edits`, `show_message`, `read_config`, `private_cells`, `event_subscriptions`, `picker_providers`, `plugin_storage` and `virtual_documents`. A listed capability is admitted with the configured plugin manifest and checked again by its corresponding host call. It is not a 0.3 permission declaration or a host-issued grant. A private-cell presenter requires `private_cells`; events, providers, storage and virtual documents enforce both directions, rejecting capability-without-declaration and declaration-without-capability. Project trust does not add a capability.

The 0.2 host imports are exactly:

| Import | Current source, success and delivery contract |
|---|---|
| `current-buffer` | Capability-gated snapshot of the invocation's admitted current buffer; string error when denied or absent. |
| `propose-edits` | Capability-gated staged byte edits against a buffer/revision; at most 1,024 calls, 4,096 edits and 1 MiB replacement bytes across the execution. The returned ordinal is not an editor-commit receipt. |
| `show-message` | Capability-gated notice; control characters are removed, text is capped to 4,096 characters and at most 64 messages survive the execution. |
| `config-get` | Capability-gated lookup in the bounded configuration supplied for this invocation; denial/missing key returns `none`. |
| `current-ui-revision` | Current immutable UI admission revision or `none`; no graph handle or mutation authority. No separate manifest capability exists. |
| `queue-ui-request` / `take-ui-events` | At most 64 staged requests in one successful execution; results drain on a later execution as typed `ui-error`/revision/opaque-handle events. Admission is by static content/action/placement declarations, not a generic UI capability. |
| `queue-confirmation` / `take-confirmation-results` | One host-owned request per execution when the shared lane is available; a typed answer/failure is delivered during a later result-only execution. No permission grant results. |
| `take-host-events` / `acknowledge-host-events` | `event_subscriptions` only; later delivery of at most 64 envelopes and acknowledgement of at most 64 unique delivered nonzero IDs. |
| `queue-storage-request` / `take-storage-results` | `plugin_storage` plus accepted quota; at most 64 staged logical-key requests and later typed worker results. |
| `take-picker-provider-requests` / `queue-picker-provider-chunk` | `picker_providers` plus accepted `picker-source`; at most 64 delivered requests/chunks, with exact request and sequence validation. |
| `take-virtual-document-requests` / `queue-virtual-document-proposal` | `virtual_documents` plus accepted read-only source; at most 64 delivered requests/proposals with exact document/request/cancellation identity. |

The world exports only `activate() -> result<list<string>, string>` and `execute-command(command, list<string>) -> result<string, string>`. There is no 0.2 event callback export: asynchronous values are supplied only when the host later admits another execution of the relevant command. Invocation input admits at most 256 positional strings, each at most 65,536 bytes, and at most 1,024 configuration entries; a successful guest result is at most 1 MiB. Activation output is capped to `plugins.maximum_commands`, each name is capped to 256 characters, and every retained name must be statically declared. Host calls using plain `string` failures and guest string errors have no stable typed error-code contract; only the UI, confirmation, provider, storage and virtual-document records define closed typed codes.

A trap, fuel/deadline interruption, guest error, malformed output or host execution violation discards that execution's staged effects and fails the current load. Production removes the runtime after the first such execution failure; it does not implement the target rolling three-failures-in-60-seconds retirement rule. 0.2 also has no `deactivate` callback. Disable/unload revokes generation-owned state and sends a best-effort internal worker removal, subject to queue ordering; it cannot promise guest cleanup code.

`queue-ui-request` never mutates the graph in the component worker. A trapped/failed execution publishes none of its queued proposals. The application later checks the exact active load/checksum/catalog generation, expected graph or tree revision, declared content/placement/instance policy, active workspace tab, graph ceilings, close order, and protected compositor boundary before clone-validate-commit. `take-ui-events` exposes only an opaque generation-bound lifecycle handle plus accepted revisions; it never exposes Rust references or raw graph authority. UI failures carry a stable `ui-error-code` (`queue-full`, stale graph/tree/handle, foreign generation, undeclared content/action, disallowed placement, hidden focus, cross-tab, instance/graph ceiling, invalid tree/placement, close order, generation loss, wrong presenter, stale surface/dimensions, invalid local bounds/grapheme/style/cursor/hit region, overlap/split-wide, patch ceiling, or internal rejection) plus a sanitized detail capped at 4 KiB. Guests branch on the code, not detail text.

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

## Current package and lock formats

The current package workflow does not consume a unified archive manifest. `package.install` and `package.update` take three workspace-rooted files after the workspace is trusted: a JSON `PackageManifest`, the schema-1 plugin TOML above, and Component bytes. The bundled trust prompt requires **`trust this workspace`**, but current configuration schema 1 can replace that phrase; this is a known Partial mismatch with the protected target copy, not permission for a plugin to supply or bypass trust. Safe mode blocks the operation. Each manifest is at most 1 MiB; the package object ceiling is fixed at 64 MiB and a lower configured plugin component ceiling narrows it during activation. The closed JSON package record has required fields:

```json
{
  "name": "publisher/name",
  "version": "1.2.3",
  "component": "plugin.wasm",
  "source": {
    "source": "registry",
    "registry": "https://dust.blurskye.dev/registry/v1",
    "version": "1.2.3",
    "archive_sha256": "<64 hexadecimal characters>"
  },
  "dependencies": []
}
```

`source` is a Serde-tagged variant:

- `registry`: `{source, registry, version, archive_sha256}`; version must be nonempty and not `*`, and the digest must be 64 hexadecimal characters. Metadata obtained through the official registry additionally requires the fixed official origin and exact duplicated identity/version/digest.
- `git`: `{source, url, commit, tree_sha256}`; commit is exactly 40 or 64 hexadecimal characters and the digest is 64 hexadecimal characters. This is only a source descriptor: direct Git network fetching is unsupported and never invokes a Git CLI.
- `path`: `{source, path}` with an absolute path. The standalone resolver can represent it and marks its result non-reproducible, but package staging rejects it; it is not an installable production source.

Each dependency is the closed `{name, source}` record. The local package parser does not currently require `publisher/name` syntax or SemVer for the top-level `name`/`version`; the official registry path adds its own identity equality and character checks, but still does not parse SemVer. The installed package path does not currently resolve or fetch a transitive graph; it records the supplied dependency names, and removal refuses a package named by another active lock entry. The JSON `component` string is required by the serializer but the current install command supplies the actual Component path separately; do not rely on that field for artifact selection. Package/plugin/source digests must all equal the bytes committed under the lowercase SHA-256 store key. The worker validates a Component and calls `activate` before returning a prepared token; the persistent owner then loads the exact generation, and only its `Loaded` event permits lock commit. A prepared/activation failure leaves the old lock active, though an unreferenced staged object can remain until garbage collection.

The active lock is `dust-packages.lock.json`, a closed JSON `PackageLock` with `version: 1`, `packages` and `reproducible`. Each package has `name`, `version`, the tagged `source` above, `content_sha256` and `dependencies`. It has no schema-2 generation/target/artifact map/features/permission digest and no enclosing lock checksum. Writes are owner-private on Unix, bounded (16 MiB in the application/CLI), file-synced and atomically renamed; one previous lock is retained. Startup and rollback re-read each checksum-bound activation TOML, hash and Component-validate each referenced store object before loading. Verification checks lock version 1, unique package names, source/digest shape and every store object's SHA-256; Wasm-prefixed objects must validate as Components. It does not prove publisher signatures or registry service provenance.

In-app `package.registry.search` and `.show` are explicit network discovery actions. `.install` is safe-mode blocked and trust-gated before artifact preparation. Package list/verify/remove/rollback/GC are asynchronous application operations on a fixed-capacity-eight worker; queue failure is reported and does not imply completion. There is no current package-operation cancellation API. GC removes only unreferenced recognized hash/activation files up to the requested bound. The headless `--package-install` option deliberately refuses to commit because it cannot prove persistent runtime activation; headless list/remove/rollback and `--verify-package-lock` operate on current lock version 1. See the [registry client contract](registry-client.md) for wire/cache details.

Neither this JSON descriptor nor lock version 1 is `dust-package.toml`/lock schema 2. Target migration is incompatible and requires the implemented 0.3 host, a schema/lock migrator, rebuilt maintained guest/SDK/fixtures, and explicit compatibility tests; a schema number edit or 0.2 wire shim is invalid.
