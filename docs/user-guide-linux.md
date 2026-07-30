# Dust on Linux

## Startup and safe mode

`dust` opens the exact current directory and shows the dashboard. Paths passed on the command line are opened through bounded VFS reads. `dust --clean` ignores user configuration. `dust --safe-mode` keeps ordinary reading/editing/saving available but disables plugin loading, project execution, and session restoration. Dust also enters safe mode after two retained crash records; `crash.clear` clears that retained signal. Inspect state with `health.open`, `application.safe_mode.status`, or `dust --health`.

A bounded asynchronous worker atomically maintains user-only `live-health.json` with counts for roots, buffers, dirty buffers, jobs, PTYs, LSP sessions, DAP, pending effects, and failed plugins. The panic hook may embed it in `last-crash.json`; no buffer contents or workspace paths are recorded.

## Essential controls

- File explorer: normal-mode `Space f e` or `:explore`. It opens as a persistent left-hand tree dock and reflows the editor instead of covering it. The workspace root expands automatically; `Enter`/`s` open files while keeping the dock visible and return focus to the editor. Run `:explore` again to focus it. Use `j/k`, `h/l`, `Enter`, `s`, `r`, `.`, `i`, and `Esc`.
- Workspace trust: run `:trust`, type `trust this workspace` exactly in the confirmation prompt, and press Enter. Trust is identity-bound and persisted; adding or replacing roots revokes it.
- Rust LSP: after trust, run `:lsp`, select `rust-analyzer`, and press Enter, or run `:lsp.server.start`. The status line shows `LSP rust-analyzer` while selected; `:lsp.server.status` and `:lspsessions` provide details.
- Command palette: normal-mode `Space f c` or `:commands`.

## Sessions and navigation

`session.save` captures roots, tabs, recursive split axes/ratios, panes, cursor/selection/scroll state, focus, and the active tab. Restore waits for every unique root-scoped read and swaps in a separately validated editor state only when all files succeed. Recent files, marks, jumps, and changes use `navigation.recents`, `navigation.marks`, `navigation.jumps`, and `navigation.changes`; `navigation.mark.set name=…` records a mark. Large-file on/off overrides persist by canonical path.

## Built-in syntax

Dust selects only pinned compiled parsers from the canonical buffer path. Rust, TypeScript, TSX, JavaScript/JSX, Go, JSON/JSONC, TOML, and Markdown are delivered. Parsing owns document/language/generation/revision and bounded source/span/fold/indent results; save-as language changes cancel stale ownership. `syntax.status` shows configured language/support/state labels and exact parser capabilities, and `syntax.fold.toggle` controls built-in folds. `syntax.indent.line` manually replaces only the current line's leading spaces/tabs with configured indentation derived from enclosing accepted syntax ranges; closing-range lines outdent, the edit is revision/writability/view checked and undoable, and stale, unsupported, or ceiling-limited data applies no edit. Dust does not claim automatic newline indentation. TOML and Markdown indentation are explicitly unsupported; Markdown block highlighting is partial. Large-file policy may disable syntax.

## Language servers and snippets

TypeScript/TSX and JavaScript/JSX use their protocol-specific `typescript`, `typescriptreact`, `javascript`, and `javascriptreact` IDs; Go uses `go`. Sessions attach only documents supported by that server definition, close and clear every owning session's URI/replay/semantic/pull-diagnostic state when save-as changes path, language, root, support, or the final buffer view closes, and select a matching root/server session before requests. Stale preflight documents are rejected on path/language/root changes while same-document revision changes refresh the launch snapshot. LSP UI projections carry active server, buffer, view, revision, URI, and language ownership; edits, view/buffer reuse, session changes, and save-as clear revisionless folds and selection ranges before another result may project. Every LSP start or restart consumes fresh trust/root identity validation and canonical execution preflight using the already discovered absolute executable. Symlinked PATH entries are resolved and validated to a regular executable; on Unix the configured executable name remains `argv[0]`, so rustup-style `rust-analyzer` proxies still dispatch correctly. Each root/server session independently owns its attached-document replay set, restart counter, exponential-backoff deadline, and circuit-open health. An inactive session restarts without stealing active selection.

Completion snippet edits within the selected placeholder atomically update every same-index mirror and recompute subsequent tabstop ranges. Unrelated buffer revisions invalidate the snippet session safely; `snippet.next`, `snippet.previous`, and `snippet.cancel` remain typed commands.

## Tasks, test results, and diagnostics

Tool installers, tasks, formatters, linters, and tests use fresh trust plus canonical absolute-executable/cwd preflight. Tool installers inherit no environment and have a source-owned 15-minute deadline. Formatter output becomes one dirty buffer transaction and is never saved automatically. Linter locations for unopened files are read asynchronously under workspace scope; an owner is replaced only after every location snapshot succeeds. Test rows with source locations open through the same checked path.

## Terminal panel

Terminal profiles are declared under `[terminals.profiles.<name>]` with an absolute program, explicit argv/cwd/environment, and dimensions. Dust owns the PTY and process group. The panel implements a bounded terminal subset: fixed cells and scrollback, Unicode display width, cursor movement, CR/LF/backspace/tab, scrolling, 16-color SGR, erase-line/display, resize, exit code, truncation, and unsupported-sequence counts. It is not a complete VT/xterm emulator.

## Debugging

Declare absolute adapters under `[debug.adapters.<id>]` and launch/attach definitions under `[debug.sessions.<name>]`. `dap.configuration.start name=…` performs fresh trust validation and canonical preflight for both adapter and launch program. Path maps use longest-prefix local↔adapter conversion. Breakpoint toggle/configure, watches, evaluate, threads, frames, scopes, variables, source references, restart, and controls are typed commands.

A debug session may set `run_in_terminal_profile` to an existing terminal profile. Dust accepts a DAP `runInTerminal` request only when executable, argv, cwd, and environment exactly equal that profile, then repeats trust/preflight and starts it through the PTY owner. Other requests receive an explicit failure.

## Workspace resource operations

LSP/plugin text edits always preview and become dirty buffers. Create first checks canonical parent scope and target absence, then creates only a dirty empty editor buffer. Rename/Delete capture canonical source bytes and Linux device/inode plus destination-parent identity. Preview acceptance only stores intents—disk remains unchanged. `workspace.resource.apply` revalidates editor state, bytes, identities, target absence, and root revision before filesystem mutation. Rename updates buffer paths after success; Delete leaves recoverable dirty scratch content. Multiple operations are sequential, not transactionally atomic. Cross-filesystem rename is rejected and never falls back to copying.

## Components and local packages

Components implement `wit/dust-plugin.wit`; see `docs/plugin-sdk.md`. There is no WASI or ambient process/filesystem/network authority. Locally supplied package/plugin manifests and component bytes can be installed with `package.install` or updated with `package.update` after workspace trust. Dust performs scoped bounded reads, checksum/store verification, production-host candidate validation, current-runtime activation, acknowledged lock commit, startup activation reconciliation, and bounded garbage collection. A prepared object never changes the active lock. The package worker publishes the lock only after the plugin owner reports `Loaded`; a failure aborts the token and leaves the old generation active. Committed plugin manifests are stored beside immutable objects, checksummed and Component-validated at startup, and a failed startup activation attempts the retained previous lock once. Rollback validates the complete prior activation set before atomically swapping current/previous locks; remove/rollback unload managed runtimes and reconcile the resulting set. `package.list`, `package.verify`, `package.remove`, `package.rollback`, and `package.gc` are available. `package.registry.search`, `package.registry.show`, and `package.registry.install` use only `https://dust.blurskye.dev/registry/v1`. HTTPS uses rustls, zero redirects, independent deadlines, strict content types, streamed byte ceilings, exact immutable identity and SHA-256, atomic quarantine/cache writes, and an integrity-checked bounded offline cache. Direct Git fetching remains explicitly unsupported; Dust never shells out to Git.

## Release and installation

See `docs/release-linux.md` for deterministic archive, API compatibility, install/update, rollback, and uninstall commands. Generated hashes and provenance are metadata, not signature claims. Publication and external signing remain release-operator responsibilities.

## Qualification scripts

`scripts/qualify-linux.sh` contains real staged commands for implementation checks, tests, strict Clippy, release, PTY, advisory, fuzz, soak, and limited-terminal checks. Advisory/fuzz stages exit 77 and report “not executed” when optional tools/targets are absent; this is not a pass claim. The workflow in `.github/workflows/linux.yml` invokes these stages sequentially where ordering matters.
