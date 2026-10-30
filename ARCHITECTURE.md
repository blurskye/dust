# Architecture

One Rust package (`src/lib.rs` + `src/main.rs`). `main` parses the CLI, loads
configuration and starts the interactive session; everything else lives in the
library.

| Module | Owns |
|---|---|
| `editor/` | Text buffers (`text`), undo transactions (`transaction`), modal input and keymaps (`input`), command registry (`command`), and the live session (`session`) that wires every feature together |
| `editor/session/` | One sub-module per feature (documents, explorer, search, lsp, git, debugger, terminal, tasks, extensions, windows, …). `dispatch/` turns input into commands, commands into effects, and effects into a rendered frame |
| `ui/` | Cell grid, diffed terminal writer, window layout graph and the renderers for each surface |
| `files/` | Filesystem access, file watching, atomic saves, recovery journals, explorer tree data |
| `language/` | Tree-sitter syntax (`syntax`) and the LSP client (`lsp`) |
| `debugger/` | DAP client |
| `workspace/` | Roots, trust, diagnostics, pickers, search, Git (signs, hunks, conflicts, worktrees) |
| `forge/` | Pull requests through `gh`: list, check out into a worktree, review comments |
| `agent/` | ACP client: agent process, JSON-RPC lines, protocol messages |
| `process/` | Child processes, PTYs and JSON-RPC framing shared by LSP, DAP and terminals |
| `tools/` | Tool catalog, installer, registry client, Nix runtimes, package lock |
| `extensions/` | Wasm component plugins (ABI in `wit/dust-plugin.wit`) |
| `config/` | TOML settings, keymaps and themes; bundled defaults in `assets/defaults/` |
| `persistence/` | Session, history and recent-file state |

Defaults live in data, not code: every shipped key binding is in
`assets/defaults/keymaps.toml` and every setting in `assets/defaults/config.toml`.
Commands are registered in `src/editor/session/commands/`.

Long-running work (LSP servers, DAP adapters, PTYs, file search, plugins) runs on
worker threads and reports back over bounded channels, so the UI thread never blocks
on a child process or the disk.
