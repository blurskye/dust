# Dust Component Plugin SDK 0.4

The production contract is [`wit/dust-plugin.wit`](../wit/dust-plugin.wit),
package **`dust:plugin@0.4.0`**, world `editor-plugin`, shipped with Dust 0.2.0.
Build plugins against this WIT; the plugin ABI version is separate from the editor
release version. Plugins are Wasm Components, not native libraries, WASI programs,
or subprocesses.

The maintained Rust example is [`tests/fixtures/retained-guest`](../tests/fixtures/retained-guest).
Its checked-in core module includes component metadata; tests wrap it with
`wit-component` and execute it through the production Wasmtime host. See its README
for the build command. Other component languages can generate bindings from
the same WIT. No published SDK crate is required.

## Start a Rust plugin

```sh
dust plugin new mywords
cd mywords
cargo build --locked --release --target wasm32-unknown-unknown
```

Install the Rust Wasm target first, or use the checkout's `nix develop .#guest`
shell. The scaffold includes the production WIT, pinned Cargo lockfile, manifest,
and a working buffer-word completion source copied from the maintained example.
Names use lowercase letters and digits, start with a letter, and are at most 64
characters. An existing destination is never overwritten. To edit a file actually
named `plugin`, use `dust -- plugin`.

Follow the generated README to encode the core module with the checkout's
`plugin_pack` example and load the component. The packer accepts either an output
folder for all six examples, or `OUTPUT CORE_WASM MANIFEST` for your own plugin.
It writes the component digest into the packaged manifest; do not load the source
manifest's zero digest.

Open **Plugins** on the dashboard, choose **Browse installed plugins** in the
command palette, or run `:plugins`. The native picker lists version, state, failure
reason and permissions. Enter opens actions to enable/disable for this session,
reload, inspect full status/permissions, or add the plugin folder to the workspace
and open the explorer.
Set `[plugins.entries.<id>].enabled` in config for a permanent enable/disable choice.
Safe mode cannot be bypassed by browser actions. `:pkgsearch <name>` and the
existing package commands install/update registry packages; the browser does not
maintain another package store.

See [the six examples](../examples/plugins/README.md) for practical autopairs,
Tree-sitter-aware surround edits, a directory editor, and buffer-word completion,
plus focused wrapper and named-mode demonstrations.

## Lifecycle and manifest

The manifest is strict TOML. Required top-level fields are `id`, `version`,
`component_sha256`, `capabilities`, `commands`, and `contributions`. Plugin IDs and
local static command IDs contain lowercase ASCII letters/digits, start with a
letter, and have at most 64 characters. The digest is the SHA-256 of the Component,
not its core Wasm input. Unknown manifest fields fail validation.

```toml
id = "demo"
version = "1.0.0"
component_sha256 = "<64 hexadecimal digits>"
capabilities = ["read_current_buffer", "propose_edits", "commands", "show_message"]

[commands.drive]
title = "Run demo"
context = "buffer"
activation = "active_only"

[commands.drive.arguments.action]
kind = "string"
required = false

[contributions]
schema_version = 1
```

Contexts are `application`, `workspace`, `view`, `buffer`, and `selection`.
Argument kinds are `string`, `boolean`, `integer`, `number`, `list`, and `record`.
The public command ID is `plugin.<owner>.<local-command>`. User keymaps override
plugin defaults, which override bundled defaults.

Dust validates declarations before loading component bytes, checks the digest and
store path, then instantiates the component on its worker without WASI imports.
`activate() -> result<list<string>, string>` returns declared local handler names.
`execute-command(command, call) -> result<string, string>` invokes one of those
handlers. The returned string is descriptive output, not the command's typed result.
Use `set-command-result` for structured command results.

Instances persist between calls. Host imports enqueue bounded work; they do not
mutate live editor state or perform network I/O on the editor thread. A guest error
discards that call's staged requests but leaves the instance loaded. Traps,
exhausted fuel, and epoch interruption unload the instance. A foreground callback
that misses its deadline loses its result; unload is not required merely because
the foreground stopped waiting. Disable removes commands, wrappers, modes, layers,
anchors, timers, subscriptions, and completion sources owned by that load. Late
replies cannot affect a replacement instance.

## Typed commands and wrappers

`command-call` contains the full command ID, named arguments, optional buffer/view
IDs, count, register, and wrapper context (`pending`, `after`, `previous`). Counts
must be positive. IDs are opaque; never synthesize an editor or pending ID.

WIT does not support recursive value definitions. `command-value` is therefore a
**postorder tree**: a list of typed nodes, with the final node as root. A list node
contains child indices; a record node contains named child indices. Each child
precedes its parent and is used exactly once. Values support strings, booleans,
signed 64-bit integers, finite numbers, lists, and records. Arguments must have a
record root. Limits are 4,096 nodes, 64 levels, and 1 MiB. For example:

```text
nodes = [text("hello"), integer(3), object([{name:"text", value:0}, {name:"count", value:1}])]
```

- `register-command` adds a namespaced command with typed argument fields, optional
  result kind, access labels, and an already activated local handler. Registration
  never overwrites an existing command.
- `run-command` enters the same native command registry as keys and returns a request
  ID. `take-command-replies` receives its value or error in a later callback.
- `wrap-command` installs an ordered wrapper around an existing command. Lower order
  runs first; equal order preserves registration order. At most 16 wrappers may wrap
  one command. Normal calls always enter at the first wrapper.
- Before native handling, `after` is false. `call-next(call.pending)` continues that
  exact invocation. A before-wrapper may call `set-command-result` with a record
  before `call-next` to replace that invocation's named arguments (not its ID,
  target, count, register or authority). The native command schema validates the
  replacement. The wrapper runs again with `after = true` and the downstream
  typed value in `previous`. Its native transaction/effects remain host-owned until
  the chain completes. Returning without `call-next` replaces/suppresses the command.
- `call-next` cannot be called twice, from an after callback, or with another call's
  ID. A background callback from the same plugin is not a continuation.

Later keys wait while a foreground plugin command is pending. Repeat and macro
recording happen on completion, not queueing. Native edit transactions provide undo.
`edit.apply` accepts integer `revision`, `start`, `end`, and `cursor`, plus `text`:
it applies one checked UTF-8 replacement and positions the cursor atomically. In
Insert mode it joins the native insert undo group. `edit.operator` also accepts
`revision`, `start`, and `end` to consume a guest-resolved text-object range, keeping
native register and operator behavior. These are ordinary commands, not new imports.
Edits from a typed foreground callback are checked against the original document
and applied through the native transaction owner; they do not require a review
panel. Existing review-producing integration calls retain their review workflow.

Access labels are `edit`, `ui`, `files`, `process`, `network`, and native `host-only`.
A command's labels are checked before entering its wrapper chain. Wrappers and
plugin-to-plugin delegation intersect authority rather than widening it. Built-in
editor transactions and approved LSP continuation effects are available to restricted
calls. Other application effects remain host-only until their native owner supports
final-effect authorization. Trust dialogs, grants, package management, and force quit
cannot be replaced by a plugin.

## Input

`enter-mode` selects a named mode with a label, cursor, key layer, and **either** a
base editing policy **or** a capture command. Base policies are Normal, Insert,
Replace, character/line/block Visual, and Multi. Capture receives unmapped keys in
the named string argument `key`, including modifiers and non-text keys.

`push-key-layer`/`pop-key-layer` install and remove temporary named layers. Layer
bindings use the native key parser and namespaced commands belonging to their owner.
A layer has at most 256 bindings; a sequence has at most eight keys. A plugin cannot
remove another plugin's mode or layers. Unload removes its layers even when another
plugin has a newer layer above them. Named modes use the native input and cursor
projection; they do not introduce another key dispatcher.

## Buffers, anchored marks, and events

`current-buffer` returns the invocation's immutable UTF-8 snapshot and revision.
`current-view` returns the matching view/cursor metadata. `propose-edits` uses byte
offsets, not code points or terminal columns. Ranges must be ordered UTF-8 boundaries;
stale revisions and read-only buffers reject mutations.

`replace-marks` replaces one plugin namespace in a buffer at an exact revision.
Each mark has a plugin-local ID, start/end byte positions, and independent left/right
insertion gravity. Native decorations move these anchors at every edit, including
undo/redo; zero-width marks are supported. `read-marks` reads their positions from
the callback snapshot, including their optional decoration fields. See Drawing below
for highlights, virtual text, and signs; retained/private panels provide custom surfaces. At most 4,096 marks
belong to a plugin. Unload clears only its namespaces.

`subscribe-events(handler)` enables autonomous occurrence callbacks. Call
`take-editor-events` to receive a batch with a sequence number and opened, closed,
changed, saved, cursor, mode, and workspace records. Repeated saves and intermediate
edits are not reduced to comparisons of final state. After a gap or a reload, the
batch contains a current editor snapshot instead of an invented edit history.
Snapshots include buffer texts and views; subscribing grants access to that data.
Delivery is bounded to 1 MiB. An oversized snapshot fails explicitly.

The earlier summary subscriptions remain useful for workspace/surface metadata.
Their bounded `take-host-events`/`acknowledge-host-events` imports are distinct from
complete editor occurrence batches and must not be treated as an edit history.

## Completion and incoming LSP filters

`register-completion-source` names a handler and whether its selected insertion can
also appear inline. The callback receives `source`, `request`, and `cursor` arguments
and the current document snapshot. `publish-completions` supplies source, request,
buffer, revision, and bounded items with byte replacement ranges, insertion text,
label/detail, and a snippet flag. Repeated publications replace that source's batch.
Native matching, menu selection, inline display, snippet parsing, acceptance, and
undo are shared with LSP. Stale document/request batches cannot replace current ones.

`filter-lsp` registers a wrapper for a supported incoming method, for example
`textDocument/publishDiagnostics` or `window/showMessage`. Its call arguments include
`event`, `method`, and `server`. Returning without `call-next` suppresses the incoming
message; continuing preserves native handling. Incoming events remain retained while
an asynchronous wrapper waits. A suppressed server edit request receives a negative
reply rather than hanging its server. This is the command wrapper chain, not a
second handler registry.

## Timers, HTTP, and capabilities

`start-timer` schedules a one-shot local handler after 1 ms to one day. `cancel-timer`
accepts its request ID. At most 64 timers are pending; unload cancels them.

`request-http` queues HTTPS work on a bounded worker and returns a request ID.
`take-http-responses` returns status, headers, body, or an error. Requests require
workspace trust, the `http` capability, and an exact manifest-approved hostname:

```toml
capabilities = ["http", { http_host = "api.example.com" }]
```

Only HTTPS on port 443 is accepted. URL credentials and guest-controlled routing
headers are rejected. Redirects are not followed, so they cannot escape the host
grant. Requests have a ten-second timeout and a 1 MiB response limit. The queue has
16 slots. A disabled/replaced load cannot receive a response. A capability is accepted
with the installed manifest; it is not inferred from a command name or workspace
trust, and an untrusted project cannot grant itself network access.

Additional capabilities are `commands`, `input`, `decorations`, `completion`, and
`timers`. Existing capabilities are `read_current_buffer`, `propose_edits`,
`show_message`, `read_config`, `private_cells`, `event_subscriptions`,
`picker_providers`, `plugin_storage`, and `virtual_documents`. Imports check their
capability before queueing; the application rechecks operation ownership and source
state before applying work.

Autonomous deliveries use the same declared handler export. Their arguments contain
`event = "editor" | "commands" | "timer" | "http"`. Drain the corresponding inbox.
Completion requests use their typed source/request/cursor fields instead.

## Panels, storage, and existing contributions

Retained trees and bounded private-cell panels use `queue-ui-request` and
`take-ui-events`. Opening/focusing/placing/closing windows goes through the native
window graph, with checked graph/tree/surface revisions and owner-scoped handles.
Declarative nodes are containers, text, lists, and buttons with semantic styles.
Private-cell policies bound cells, patches, styles, hit regions, and bytes.

Ordinary confirmation remains host-owned. `queue-confirmation` returns a token;
`take-confirmation-results` runs later in a restricted callback that can update an
existing panel but cannot issue new edits, storage/provider work, or another prompt.
Trust and grant prompts are never replaceable.

`queue-storage-request` provides logical-key get, compare-and-set, delete, and prefix
list operations. `take-storage-results` returns revisions and typed errors. The host
chooses the per-plugin filesystem location. A manifest storage policy bounds items,
value bytes, and total bytes; both policy and `plugin_storage` are required.

Picker sources and read-only virtual documents retain their queued request/chunk
and full/patch protocols. Declarations must name a local handler and enforce their
request, item, byte, and deadline limits. The full schemas live in
[`src/plugins/manifest.rs`](../src/plugins/manifest.rs); validation is in
[`src/plugins/index.rs`](../src/plugins/index.rs).

## Surface: interactions, documents, save steps and chrome

These imports use the native owners, not plugin-specific copies of the editor UI.
UI operations require the `ui` command grant (provided by `input`, `show_message`,
or `private_cells`); documents and save steps require `edit` (`propose_edits`).
Both the import and final effect check permission. Delegated callbacks and clicks
retain the initiating authority; presenters cannot turn a UI grant into permission
to execute a process or write files.

- `ui-input(request, handler)` opens a text prompt, optionally masked with `secret`.
  Secret input is not stored in prompt history. `ui-select(request, handler)` opens
  the native picker with stable item IDs, labels and details. Both return a request
  ID. The declared handler later receives `event = "surface-reply"`; drain
  `take-command-replies`. A reply contains the input text or selected item ID, or
  no value on cancellation. An already-open interaction is not replaced.
- `notify` publishes info, warning or error notices through native history.
- `set-presenter(kind, handler)` replaces ordinary input, select or notification
  presentation. `None` restores the host presenter (only the current owner may
  remove its replacement). The handler receives `event = "ui-input"` with
  `title`, `initial`, `secret`; `"ui-select"` with `title`, `items`; or `"notify"`
  with `message`, `level`. Use `set-command-result` to return input text or an
  offered item ID; `false` cancels. To keep a custom panel open for user input,
  return without a result, then call `answer-input(request, text)` or
  `answer-select(request, item)` from a later panel handler. The callback's
  `request` identifies the question; `None` cancels. Questions expire after sixty
  seconds. Notification handlers need no result. Permission prompts and yes/no confirmations are always host-owned.
  Failed presenters fall back to native input/select without losing the draft.
- `open-editable-document(document, handler)` opens a normal editable buffer,
  identified by plugin, source and key. Reopening it preserves unsaved edits.
  The initial `surface-reply` contains its buffer ID. Saving invokes the handler
  with `event = "document-write"`, `buffer`, `revision`, and `text`. Return
  boolean `true` to accept; any other result or error rejects the write and leaves
  the buffer dirty. These documents never fall through to a filesystem save.
- `register-formatter(filetype, step)` and `register-before-save(filetype, step)`
  add named steps ordered by integer order, then name. Registering the same name
  updates it. The shared native chain invokes `event = "format"` with `buffer`,
  `revision`, `text`, byte `start`/`end` (the whole buffer), and `before-save`.
  Return a list of `{ start, end, replacement }` records using
  `set-command-result`. Edits are validated and applied as undoable native edits;
  changed buffers reject stale results. Errors or the two-second callback deadline
  skip that plugin step. Provider writes instead fail visibly. Manual formatting
  uses the same formatter chain without saving.
- `set-chrome-segment` adds or updates a plugin-owned named statusline or buffer-line
  segment; `remove-chrome-segment` removes it. Segments follow native fields in
  ascending order/name order. Styled parts use `default`, `muted`, `accent`,
  `warning`, `error`, `selected`, `border`, or `status`. The optional command runs
  through normal permission-checked dispatch on a left click. Text is cached;
  drawing never calls the guest.

Surface requests share the 64-request/1 MiB per-execution limits. Selects allow
4,096 items, segments 64 styled parts, and each running editor allows 128 save
handlers and 128 named plugin segments. Unload removes presenters, save steps,
segments and pending callbacks; unsaved document buffers remain available.

Themes remain validated TOML data, not executable plugin palette mutations.
Plugin-managed dirty documents/panels protect ordinary close; there is no guest quit
hook and no way to block a forced quit.

## Verification and development

From the repository root, enter the guest shell, which supplies the Wasm target:

```sh
nix develop .#guest
cd tests/fixtures/retained-guest
cargo build --locked --release --target wasm32-unknown-unknown --target-dir target
cp target/wasm32-unknown-unknown/release/dust_retained_guest.wasm retained-guest.core.wasm
```

After changing WIT, rebuild every fixture and copy the production WIT into its guest
source. Host tests cover real component calls, ordered wrappers, input modes/layers,
anchored edits and undo, event gaps, menu/inline completion, LSP method filters,
errors, and permission rejection, alongside the existing panels/storage/provider
and sandbox tests. The full gate is format check, Clippy with warnings denied, and
`cargo test --locked`.

## Drawing: decorations, inline completion, and diagnostics

`mark.decoration` is optional. It adds presentation to the same moving mark, not a
second anchor or a terminal-cell API:

- `highlight` names a theme highlight group over the half-open byte range.
- `virtual-text` has text, a highlight group, and `inline` (before the mark),
  `end-of-line`, or `overlay` placement. Inline text participates in wrapping,
  cursor placement, and mouse hit testing; overlay text does not move source text.
- `sign` has a one-cell icon and highlight group, drawn in the existing sign column.

Use known theme groups such as `Comment`, `@keyword`, or `DiagnosticWarn`. Unknown
names, control characters, multiline decoration text, and invalid UTF-8 ranges
are errors. Each text is limited to 4,096 bytes. Later marks win when overlays
occupy the same cells; selection and diagnostic underlines take precedence over
mark highlights. Replacing a namespace is atomic. Empty marks clear it;
`read-marks` returns decorations and their current positions. Undo/redo moves the
same marks, and unloading removes only that plugin's namespaces.

A completion source registered with `inline = true` can use the existing
`publish-completions` import. The selected item's untyped suffix appears before
the cursor's source text using the same inline drawing path. The cursor stays at
the beginning of the suggestion. Only the first line is previewed; native
acceptance still inserts the complete item, including snippets. Moving the cursor,
editing, dismissing completion, or unloading its source removes the preview.

`publish-diagnostics` replaces a namespace's diagnostics for one buffer and exact
revision. Items contain a half-open UTF-8 byte range, severity (`error`, `warning`,
`information`, `hint`), message, and display source. The source is a label, not an
ownership key: plugins cannot overwrite another plugin or LSP source. An empty
list clears this namespace for the buffer. File and unnamed buffers are supported.
These are native diagnostics: signs, underlines, virtual text, the diagnostics
picker, and `]d`/`[d` use the same store as LSP. Edits move their ranges; closing or
reloading the document invalidates them. Unloading clears the plugin's diagnostics.
Limits are 2,000 items per set, 256 namespaces across plugins, 16 KiB per message,
and 256 bytes per source, within the shared diagnostic-store limits.

Drawing and completion effects require the caller's **ui** command-access grant,
checked again when queued work is applied through the existing effect authority.
The current manifest's `show_message` grant supplies UI access (as do `input` and
`private_cells`); diagnostics imports require `show_message`. Marks additionally
require `decorations`, and completion imports require `completion`. Delegation
cannot widen access. Missing grants produce an error, not an empty success.
The retained guest's `drawing`, `drawing-clear`, `diagnostics`,
`diagnostics-clear`, and `completion` ABI actions demonstrate these calls.

## System access: processes, workspace files, and syntax

System imports return a request ID. Their declared local handler runs with
`event = "system"`; drain `take-system-events` for matching IDs and typed results
or errors. An import error means nothing was queued. An asynchronous error is
returned in that request's event, not silently ignored. Handler calls retain the
initiating command's restricted authority, including across delegation.

### Processes

Declare `process` in manifest capabilities. `start-process` accepts a program,
argument vector, workspace-relative (or absolute in-workspace) cwd, environment
additions, optional initial stdin bytes, and a handler. Programs are not implicitly
shell commands: pass `sh` and `-c` explicitly if needed. Environment additions win
over Dust's inherited environment. A trusted workspace is also required.

The start ID identifies the process. Output events contain raw stdout/stderr byte
chunks, including output without newlines, followed by an exit code (`none` when
terminated by a signal). `write-process-stdin` sends more bytes and optionally closes
stdin; stdin otherwise remains open. `kill-process` terminates the process group.
Both return separate request IDs acknowledging success or failure. Only the owning
plugin can address a process. There are at most eight live processes per plugin,
1 MiB of output per process, a bounded stdin queue (16 chunks, at most 1 MiB each),
and a one-day runtime limit. Limit failures are explicit. Unload and editor shutdown
cancel remaining children through the same job runner used by native tasks.

**The process grant gives ordinary user-process access, not a filesystem sandbox.**
Only cwd is constrained to the workspace; spawned tools can access anything the
user can. The files grant below is narrower.

### Workspace files

Declare `files`. `request-file` supports read, write, create-directory,
list-directory, stat, rename, and delete. Paths resolve relative to the current
workspace root. Absolute paths are accepted only inside that root. Symlinks are
resolved before scope checks; checked filesystem opens reject substituted symlinks
at the operation itself. Read/write bytes are binary, limited to 1 MiB per request;
listings contain at most 4,096 entries and report links as `other` without following
them. Rename does not overwrite an existing destination; directory deletion is
nonrecursive. Parent directories must already exist.

Writes reload matching clean open buffers through native document reload. Writes,
renames and deletes affecting buffers with unsaved changes are refused. If a user
edits during background I/O, those edits are preserved and the result reports the
conflict. A successful disk write does not discard newer edits.

### Tree-sitter

`query-syntax` and `node-at-position` need only `read_current_buffer`, not files or
process grants. They use the existing buffer parser's current tree, not another
parser. A buffer without a parsed tree, or with parsing still behind its text,
returns an error (retry after parsing). Queries return capture names and byte ranges;
an optional byte range limits matching. Node lookup returns the smallest node at
the byte position, then its parents through the root, with kinds and byte ranges.

Ranges use UTF-8 byte boundaries and exclusive ends. Queries are limited to 16 KiB,
4,096 captures/matches and a bounded query execution time; parent chains to 256
nodes. Invalid query syntax and limit exhaustion return errors, never partial success.
