# Dust on Linux

## Startup and safe mode

`dust` opens the exact current directory and shows the dashboard. Paths passed on the command line are opened through bounded VFS reads. `dust --clean` ignores user configuration. `dust --safe-mode` keeps ordinary reading/editing/saving available but disables plugin loading, project execution, and session restoration. Dust also enters safe mode after two retained crash records; `crash.clear` clears that retained signal. Inspect state with `health.open`, `application.safe_mode.status`, or `dust --health`.

A bounded asynchronous worker atomically maintains user-only `live-health.json` with counts for roots, buffers, dirty buffers, jobs, PTYs, LSP sessions, DAP, pending effects, and failed plugins. The panic hook may embed it in `last-crash.json`; no buffer contents or workspace paths are recorded.

## Keymap policy

`Space` is the bundled value of `<leader>`, not a hardcoded application key. Configure the bounded literal leader sequences and bundled roster in `config.toml`:

```toml
[input.keymaps]
load_bundled_defaults = true
leader = "space"
local_leader = "\\"
```

`keymaps.toml` accepts `<leader>` and `<localleader>` as whole sequence tokens. Every declaration requires a stable bounded lowercase dot-separated `id`. Redeclaring a lower ID replaces or moves it before leader expansion, so the old sequence disappears; an identity-only declaration containing that ID plus `disable = true` disables it without repeating context/keys. Exact unmap/pass-through declarations have their own IDs and context/keys. `:keymap inspect` reports the symbolic declaration and effective sequence; `:keymap inspect id=<id>` reports the stable contribution directly. Setting `load_bundled_defaults = false` omits every bundled mapping and key group while retaining explicit user declarations. Literal `space` remains available for actions that should not follow the leader. Invalid policy/keymaps reject the complete reload and retain the previous snapshot.

When an effective sequence remains a prefix, Dust shows a delayed which-key-style surface generated from that exact resolver state. `[[key_group]]` entries in `keymaps.toml` identify symbolic prefix labels/icons; they use the same replacement and identity-only disable semantics as maps. `[ui.key_hints]` configures enablement, delay, bottom-left/right anchor, row/column/width ceilings, padding/gaps, icon/title visibility, fallback label, and border. Hints are clipped to the editor region and disappear on dispatch, timeout, invalid continuation, paste, focus/context change, prompt activation, or configuration reload. Disabled, unmapped, and pass-through operations are never advertised.

Plugin manifests require a versioned static `contributions` block for namespaced contexts, typed commands, suggested mappings, and key groups. Dust validates and indexes that block before reading Component bytes, then resolves `Bundled < Plugin < User`. User declarations may replace/move or identity-disable plugin defaults and may bind enabled plugin commands. At initial startup these references remain pending until every expected enabled manifest indexes and the complete roster compiles; an indexed command keeps its typed schema but reports unavailable until that exact runtime generation is active. Failure/disable removes exact plugin ownership and restores a safe resolved roster. Changing `[plugins.entries]` while running is rejected before mutation and currently requires restarting Dust; reloads that keep plugin topology unchanged remain transactional.

## Essential controls

- File explorer: normal-mode `<leader> f e` (bundled as `Space f e`) or `:explore`. It opens as a dedicated 40-cell left-hand tree dock and reflows the editor instead of covering it. The workspace root expands automatically; `Enter`/`s` open files while keeping the dock visible and return focus to the editor. Rows use separately themed tree, icon, filename, selection, Git, and diagnostic segments; `[ui.explorer]` selects Nerd Font, Unicode, or ASCII glyph sets and validated width/min/max. Children use directory-first natural numeric ordering. Run `:explore` again to focus it. Use `j/k`, `h/l`, `Enter`, `s`, `r`, `.`, `i`, and `Esc`.
- Surface focus: `Ctrl-h/j/k/l` moves spatially among editor splits in Normal and Visual modes. `Ctrl-h` from a leftmost editor pane enters the visible explorer; `Ctrl-l` returns to the editor. Directional focus does not wrap, and Insert-mode control characters are unchanged.
- Horizontal viewport: each split/tab view owns an independent non-wrapped display-cell offset. `[editor.viewport] horizontal_margin = 4` controls retained left/right cursor context and accepts `0..=100`. Tabs, wide characters, combining sequences, line-number gutters, pane borders, resize, and the explorer dock are included in projection. Oversized line-number separator/padding policy is clamped so every non-empty pane retains one text/cursor cell. Wrapped mode has no horizontal offset. Vertical scroll context remains zero logical lines; configurable vertical/fold-aware scrolloff is not yet delivered.
- Workspace trust: run `:trust`, type `trust this workspace` exactly in the confirmation prompt, and press Enter. Trust is identity-bound and persisted; adding or replacing roots revokes it.
- Rust LSP: after trust, run `:lsp`, select `rust-analyzer`, and press Enter, or run `:lsp.server.start`. The status line shows `LSP rust-analyzer` while selected; `:lsp.server.status` and `:lspsessions` provide details.
- Command palette: normal-mode `<leader> f c` (bundled as `Space f c`) or `:commands`.

## Sessions and navigation

`session.save` captures roots, tabs, recursive split axes/ratios, panes, cursor/selection/logical-line and display-cell scroll state, focus, and the active tab. Restore waits for every unique root-scoped read and swaps in a separately validated editor state only when all files succeed. Recent files, marks, jumps, and changes retain their horizontal display-cell location as well. `navigation.recents`, `navigation.marks`, `navigation.jumps`, and `navigation.changes` expose those lists; `navigation.mark.set name=…` records a mark. Large-file on/off overrides persist by canonical path.

## Built-in syntax

Dust selects only pinned compiled parsers from the canonical buffer path. Rust, TypeScript, TSX, JavaScript/JSX, Go, JSON/JSONC, TOML, and Markdown are delivered. Parsing owns document/language/generation/revision and bounded source/span/fold/indent results; save-as language changes cancel stale ownership. `syntax.status` shows configured language/support/state labels and exact parser capabilities, and `syntax.fold.toggle` controls built-in folds. `syntax.indent.line` manually replaces only the current line's leading spaces/tabs with configured indentation derived from enclosing accepted syntax ranges; closing-range lines outdent, the edit is revision/writability/view checked and undoable, and stale, unsupported, or ceiling-limited data applies no edit. Dust does not claim automatic newline indentation. TOML and Markdown indentation are explicitly unsupported; Markdown block highlighting is partial. Large-file policy may disable syntax.

## Language servers and snippets

TypeScript/TSX and JavaScript/JSX use their protocol-specific `typescript`, `typescriptreact`, `javascript`, and `javascriptreact` IDs; Go uses `go`. Sessions attach only documents supported by that server definition, close and clear every owning session's URI/replay/semantic/pull-diagnostic state when save-as changes path, language, root, support, or the final buffer view closes, and select a matching root/server session before requests. Stale preflight documents are rejected on path/language/root changes while same-document revision changes refresh the launch snapshot. LSP UI projections carry active server, buffer, view, revision, URI, and language ownership; edits, view/buffer reuse, session changes, and save-as clear revisionless folds and selection ranges before another result may project. Every LSP start or restart consumes fresh trust/root identity validation and canonical execution preflight using the already discovered absolute executable. Symlinked PATH entries are resolved and validated to a regular executable; on Unix the configured executable name remains `argv[0]`, so rustup-style `rust-analyzer` proxies still dispatch correctly. Each root/server session independently owns its attached-document replay set, restart counter, exponential-backoff deadline, and circuit-open health. An inactive session restarts without stealing active selection.

Hover and signature help use a cursor-relative multiline documentation popup, not the notification row. `[ui.documentation_popup]` controls enablement, width/height, padding, preferred below/above placement, border, and bounded `scroll_step`; theme roles are `documentation_background`, `documentation_border`, `documentation_title`, and `documentation_text`. `lsp.documentation.scroll_up`, `lsp.documentation.scroll_down`, and `lsp.documentation.close` are registered and user-mappable. Dust intentionally ships no default scroll keys for this popup. Movement/edit/paste/focus/new-request/reload changes dismiss it. Structured Markdown rendering and a real-hover-server PTY fixture remain pending.

Completion snippet edits within the selected placeholder atomically update every same-index mirror and recompute subsequent tabstop ranges. Unrelated buffer revisions invalidate the snippet session safely; `snippet.next`, `snippet.previous`, and `snippet.cancel` remain typed commands.

## Tasks, test results, and diagnostics

Tool installers, tasks, formatters, linters, and tests use fresh trust plus canonical absolute-executable/cwd preflight. Tool installers inherit no environment and have a source-owned 15-minute deadline. Formatter output becomes one dirty buffer transaction and is never saved automatically. Linter locations for unopened files are read asynchronously under workspace scope; an owner is replaced only after every location snapshot succeeds. Test rows with source locations open through the same checked path.

## Integrated bottom terminal

Dust bundles a `shell` terminal profile using `/bin/sh`; `[terminals] default_profile` selects the profile opened when none is named. Additional profiles are declared under `[terminals.profiles.<name>]` with an absolute program, explicit argv/cwd/environment, and bounded launch dimensions. For example, a `zsh` profile must be configured explicitly before `:terminal.open zsh` can use it. `:terminal.open` with no argument opens the configured default. Opening requires workspace trust and canonical executable/cwd preflight. Dust owns the PTY and process group.

The selected terminal is rendered directly as a persistent full-width bottom dock beneath the explorer and editor—not as a picker. **Ctrl+/** and **Ctrl+\\** both run the bundled `terminal.toggle` mapping from editor modes, dashboard, explorer, and terminal: they open the default profile, return focus without hiding it, or focus the existing dock. Legacy control-byte terminals are normalized to the same logical bindings. `[ui.terminal_panel]` configures initial/minimum/maximum height, retained workspace height, resize step, divider, title/status formats, and labels. `terminal.resize rows=…`, `terminal.cycle direction=…`, `terminal.focus id=…`, `terminal.focus_active`, `terminal.unfocus`, and `terminal.close` are also registered and user-mappable. Escape remains the terminal-context mapping for `terminal.unfocus`. Clicking the terminal body uses the configurable `terminal_mouse` context.

The dock renders fixed cells, ANSI-16 foreground/background/bold state, wide-cell continuations, combining text, bounded scrollback metadata, PTY cursor, exit code, truncation, and unsupported-sequence counts. The `portable-pty` reader transports bounded raw PTY bytes to `PtyScreen`; incremental UTF-8 decoding preserves characters split across reads, while the screen parser consumes rather than prints terminal controls. Common shell bracketed-paste mode, SGR color, and OSC title sequences therefore never leak as literal `[?2004h`, `[1;32m`, or `]0;…` text. Resize and terminal selection update the PTY to the exact rendered body dimensions. It implements CR/LF/backspace/tab, scrolling, Unicode display width, ANSI-16 SGR, erase-line/display, OSC consumption, and a bounded cursor CSI subset. It is not a complete VT/xterm emulator. General terminal buffers in arbitrary editor splits/tabs and terminal-normal scrollback navigation remain pending.

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
