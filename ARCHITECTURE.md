# Architecture

Dust is one Rust package with a terminal executable and a library. The library
contains the editing model, application, protocol clients, workers and renderer.
`src/main.rs` parses command-line options, handles noninteractive commands, loads
configuration and starts the application. `src/lib.rs` lists the modules.

## Code owners

Paths in this table are under `src/`.

| Module | Owns |
|---|---|
| `app/` | The running editor: feature state, command/effect coordination, startup, ordered polling and shutdown. |
| `editor/` | Text, buffers, views, registers, modal input grammar, command registry, transactions, undo and anchored decorations. |
| `windows/` | Window IDs, tabs, split trees, focus, content ownership, layout and compositor geometry through `WindowGraph`. |
| `files/` | Filesystem reads and writes, directory operations, file watching, project-root discovery, workspace enumeration/search and recovery journals. |
| `process/` | Child-process jobs, cancellation, output limits, PTYs, executable checks and protocol framing. |
| `config/` | Typed settings, TOML loading and validation, keymap declarations, theme palettes and highlight groups, configuration/state paths. |
| `workspace/` | Workspace roots and trust records, diagnostic storage, picker data and fuzzy matching, workspace-edit proposals. |
| `git/` | Repository status, hunks, signs, mutations, branches, conflicts and worktrees. |
| `forge/` | Pull requests and review comments through the `gh` and `tea` CLIs. |
| `language/` | Filetype identification, Tree-sitter parsing and queries, LSP transport, sessions, protocol messages, snippets and paths. |
| `debugger/` | DAP transport and sessions, requests, events and reverse requests. |
| `agent/` | ACP clients, conversation history and the MCP subagent endpoint. |
| `tools/` | Tool catalog, installed executables, runtime discovery, Nix workspace environments, downloads, installers, plugin packages and registry client. |
| `plugins/` | Wasmtime component loading/execution, manifests, capabilities, bounded host requests, HTTP, storage, picker providers and virtual-document protocols. |
| `persistence/` | Saved session/history records and asynchronous state I/O. |
| `telemetry/` | Bounded logs, health snapshots and crash-report data. |
| `ui/` | Terminal lifecycle and events, Unicode cell grids, frame differences, Markdown layout, PTY screen state and rendering. |
| `remote.rs` | IPC for `dust --remote` and requests from programs Dust launches. |
| `clock.rs` | Clock and ID sources, including deterministic test implementations. |
| `api/` | Public API manifests and compatibility checks for commands, configuration, persistence, tools and WIT. |
| `testing/` | Test-only headless drivers that exercise production editor and application paths. |

### Application features

`app/mod.rs` holds `App`, composes the feature owners and runs the event loop.
`app/startup.rs` resolves file/directory arguments and constructs state and workers.
`app/shutdown.rs` finishes owned work on exit.

Most feature directories contain state, an `impl App` coordinating it, commands and
owner-local tests. `app/commands/` registers those commands and routes `AppEffect`;
it does not implement a second editor. Important groups are:

- `input/`, `editing/` and `prompt/`: focus routing, configured keys, modal edits,
  repeat/macros, clipboard, search/substitute input and ordinary prompts.
- `documents/`: open, reload, save, recovery, editable plugin documents and the
  formatter/before-save chain. `workspace/edits/` coordinates checked multi-file
  changes and review.
- `windows/`: application content attached to the window graph, focus changes,
  navigation checks and cached statusline/buffer-line segments.
- `explorer/`, `picker/` and `search/`: file-tree interactions, result lists,
  previews, live grep and workspace replacement.
- `language/`, `syntax.rs` and `diagnostics.rs`: document synchronization,
  language-server requests/results, completion, parsing and diagnostic display.
- `git/`, `terminal/`, `debugger/`, `agent/`, `tasks/` and `tools/`: editor-facing
  workflows around their respective clients and workers.
- `configuration/` and `settings/`: compile and apply configuration contributions,
  keymaps, themes, setup and settings interactions.
- `plugins/`: connect component requests and replies to these native owners;
  `plugins/abi/` translates typed WIT requests, command values and callbacks.
- `persistence/`, `events.rs`, `notifications.rs`, `documentation.rs` and
  `health.rs`: session restoration, editor occurrences, notices, documentation
  popup lifetime and health reporting.
- `dashboard/`, `help.rs` and `tutor.rs`: startup actions, help and practice lessons.

`ui/render/` draws from editor state, `UiState`, configuration and window size.
Feature owners prepare that data; rendering does not invoke plugins or start
filesystem/process/network work. Window topology belongs to `windows/`, not to a
renderer or a plugin panel.

## Key to screen

1. `ui/terminal.rs` reads a terminal event. `app/input/` routes it to the focused
   surface and active input context. Configured keymaps, temporary layers and the
   modal grammar resolve a `CommandInvocation` with target, arguments, count and
   register.
2. `app/commands/` enters the single `editor/command/CommandRegistry`. It checks
   command access, executes the handler or ordered wrapper chain, and receives a
   transaction, application effects, follow-up commands, a value or a pending
   continuation.
3. `EditorState::apply_transaction` in `editor/transaction/` validates preconditions
   and operations on a draft before publishing it. Text edits update undo history,
   cursor positions, anchored decorations and buffer change records together.
   Undo and redo use the same owner. An invalid edit does not publish a partial
   editor transaction.
4. Application effects go to their feature owners. A save, tool launch or server
   request may finish asynchronously. Foreground plugin commands hold subsequent
   input while pending; repeat and macro recording wait for completion rather
   than treating a queued call as a successful edit.
5. The main loop polls worker results, reconciles windows and feature projections,
   and updates `UiState`. It rebuilds a frame when tracked editor/UI state,
   configuration bindings, window size or terminal invalidation requires it.
   `ui/render/` produces the cell grid; the terminal writes the changed cells and
   final cursor state. Idle polls do not rebuild unchanged frames.

The polling order is explicit in `app/mod.rs`. Filesystem, Git, language, debugger,
terminal, agent, plugin, task and persistence results return through their owners.
Requests retain the source information needed to reject stale results rather than
applying them to a different document, view or workspace.

## Documents, persistence and defaults

`app/documents/` coordinates buffer lifecycle with `files/` workers. A normal save
passes through formatting/before-save steps and checked filesystem I/O. Saving an
editable plugin document invokes its provider instead; a rejected provider write
leaves it dirty and never falls through to a local file save. Dirty buffers also
feed recovery journals without automatically overwriting their original files.

`app/persistence/` interprets saved editor records and restores the current native
owners. `persistence/` owns serialization and bounded background I/O. Recovery
journals in `files/` are separate from the session layout/history file.

Shipped configuration lives in `assets/defaults/`: settings, keys, command labels,
dashboard declarations, tool definitions and tutor data. `assets/themes/` and
`assets/syntax/` contain palettes and syntax queries with upstream notices.
`config/` parses and validates configuration; `app/configuration/` combines it with
command and plugin contributions. Invalid reloads leave the previous working
configuration active. The [user guide](docs/user-guide-linux.md#configuration)
describes configuration and state paths.

## Language services

`app/language/launch.rs` resolves project roots, configured/installed servers and
tool environments. Launch is gated by workspace execution policy. The LSP client
in `language/lsp/` owns process transport, protocol framing, request IDs, server
sessions and capabilities.

`app/language/` synchronizes named buffers from their in-memory revisions, not
only saved contents. Requests retain their document/session context. Incoming
messages return to the main loop as owned events; `app/language/incoming.rs`
routes supported methods through `lsp.incoming.*` commands before native handling.
Plugin wrappers can continue or suppress those handlers using the same command
chain as other commands. Unhandled server edit requests receive a failure reply.

Diagnostics go to the shared workspace diagnostic store. Completion sources from
LSP and plugins share native matching, selection, snippet insertion, inline
preview and acceptance. Document edits use checked native transactions or the
workspace-edit workflow. Stale replies cannot replace current document results.

Tree-sitter is independent of LSP. `language/syntax/` parses buffer snapshots and
provides highlights, folds, text objects and bounded queries. `app/syntax.rs`
coordinates parsing and projects accepted results for display.

## Plugins and permissions

The production interface is [`wit/dust-plugin.wit`](wit/dust-plugin.wit), package
`dust:plugin@0.4.0`, world `editor-plugin`. This ABI version is separate from the
editor's release version. The [Plugin SDK](docs/plugin-sdk.md) specifies manifests,
limits and request semantics.

The flow is:

```text
WIT imports/exports
  -> plugins/component.rs and plugins/host.rs (component execution and requests)
  -> app/plugins/abi/ (translation, callback ownership and authorization)
  -> native editor, document, language, prompt, picker and window owners
  -> cached UI state -> ui/render/
```

Manifest and component validation happen before loading. Components run on a
worker with Wasmtime resource limits and no WASI imports. Host imports stage
bounded requests; the application applies accepted work through native owners.
Plugin panels use the native window graph, edits use editor transactions,
diagnostics use the shared store, and system requests use `files/` and `process/`.
The renderer never calls a guest to draw a frame.

Permissions are checked both when a command/import is requested and at the final
effect. `CommandAuthority` carries the initiating caller through wrappers,
delegation and pending operations; intersecting grants cannot broaden authority.
`app/commands/` checks access labels, feature owners check the actual operation,
and `run_trusted` checks execution policy before launching trusted work. Plugin
filesystem operations check workspace scope and dirty buffers; the HTTP worker
accepts declared HTTPS hosts and does not follow redirects. Effects without a
supported restricted path remain host-only. Trust/grant prompts and forced quit
cannot be replaced by plugins.

Process grants are not an OS sandbox: children run with the user's ordinary
access. ACP adapters likewise run as ordinary child processes; adapter-owned tools
do not acquire filesystem confinement from the editor's ACP file-request checks.

`app/events.rs` and editor-owned change records supply occurrence batches to
plugins. Delivery can resnapshot after a gap. Callback deadlines discard late
results; traps or runaway execution unload the component. Unload removes owned
commands, layers, marks, presenters and pending work. Unsaved editable documents
remain available to the user.

## Tests and release files

Owner tests sit beside the implementation; `app/tests/` covers cross-feature
flows. `tests/` contains CLI, PTY and fixture tests. The retained guest fixture is
built from its source and matching production WIT, then checked in so host tests
need no guest compiler. `scripts/ui-shot` captures the real terminal for visual
checks. Build and test commands are in [CONTRIBUTING.md](CONTRIBUTING.md).

`packaging/` and `.github/workflows/release.yml` build Linux distribution artifacts.
The [release guide](docs/release-linux.md) describes versions, packages and signatures.
