# Dust on Linux

This guide covers everyday editing. For implementation limits and qualification evidence, see [`progress/linux-phase-status.md`](progress/linux-phase-status.md).

## Start

```sh
dust                 # open the current directory
dust path/to/file    # open a file
dust --clean         # ignore user configuration
dust --safe-mode     # disable plugins, project execution, and session restore
```

A workspace Dust has not seen before opens the protected trust prompt after the first frame. Type `trust this workspace` to allow project commands, or press Escape to keep editing without execution. Dust remembers that the prompt was shown. Use `:trust` later to approve the workspace.

Press `:` from the dashboard, editor, or explorer to open the command menu. Start typing to filter commands; Enter accepts and Escape closes it.

## Essential editing

Dust uses Vim-style Normal, Insert, Visual, and command-line modes.

| Action | Key |
|---|---|
| Insert text | `i` |
| Return to Normal mode | `Escape` |
| Save | `:w` |
| Quit | `:q` |
| Save and quit | `:wq` |
| Force quit/discard | `:q!` |
| Open file | `:e path` |
| New buffer | `:new` |
| Close buffer | `:bd` |
| Force-close dirty buffer | `:bd!` |
| Undo / redo | `u` / `Ctrl-r` |
| Search forward/backward | `/` / `?` |
| Repeat search | `n` / `N` |
| Command palette | `Space f c` |
| File picker | `Space Space` |
| Workspace grep | `Space /` or `Space s g` |

`Ctrl-h/j/k/l` moves between editor panes and the explorer. `H`/`L` and `[b`/`]b` switch buffers. The cursor remains visible through wrapping, folds, Unicode text, splits, resize, and explorer/terminal layout changes. During repaint, Dust hides the hardware cursor and publishes its final position within a synchronized terminal update; unchanged frames send no cursor commands. This keeps internal painting movements from appearing as cursor jumps or trails.

## Explorer

Open the explorer with `f` on the dashboard, `Space f e` in Normal mode, or `:explore`.

| Action | Key |
|---|---|
| Move | `j` / `k` |
| Parent / open child | `h` / `l` |
| Open / split-open | `Enter` / `s` |
| Add file | `a`, enter `name.ext` |
| Add directory | `a`, enter `name/` |
| Rename (prefilled path prompt) | `e` or `r` |
| Delete | `d`, review the target, choose **Yes** or **No** |
| Copy / move selection | `c` / `m` |
| Paste into selected directory | `p` |
| Yank absolute path | `y` |
| Refresh | `R` |
| Toggle hidden / ignored | `.` / `I` |
| Open command menu | `:` |
| Close explorer | `Escape` |

Rename opens the shared **Rename path** prompt, prefilled relative to its displayed workspace root. Edit the path and press Enter; Escape cancels before application. A collision keeps the entered name available for correction. Source inspection is asynchronous; wait for it to finish before submitting. Names are literal (not shell or command-line text), limited to 4096 UTF-8 bytes without controls or `..` traversal. Open dirty buffers and their split views retain their text after rename. `:explorer.rename target=...` remains the direct selected-path command with parent-relative target semantics.

Explorer deletion opens the shared protected **Delete this path?** dialog with **No** selected. Left/Right, Tab, Y/N choose an answer; Enter submits it; Escape cancels without answering. Dust disables positive confirmation until the complete escaped target is visible, so resize a too-small terminal before choosing **Yes**. This replaces the old `d`, then type `delete` flow.

Explorer mutations are asynchronous, confined to canonical workspace roots, reject overwrite and symlink mutation, and reconcile open buffers. Deleting an open file keeps its text as a dirty recoverable scratch buffer. Recursive copy and deletion of non-empty directories are intentionally refused; empty directories are supported.

## Finders and search

Dust exposes the daily Telescope/Snacks-style workflows through its bounded picker. `Space /` and `Space s g` open the Grep panel directly—there is no intermediate command prompt—and typing restarts the cancellable live search. File and grep results use a compact result list, live result count, and adaptive file preview: readable panes sit side by side when their measured content fits, otherwise Dust stacks the preview below the results. Grep previews keep the selected match line on-screen and highlight it even when the stacked preview has fewer rows than the captured context. File previews read up to 256 KiB by default through scoped asynchronous VFS reads (`[ui.picker].maximum_preview_bytes`), so files larger than 120 KiB are not rejected:

- `Space Space` or `Space f f`: files in the active workspace roots;
- `Space /`, `Space s g`, or `Space f g`: workspace text search;
- `Space f b`: open buffers;
- `Space f c`: commands;
- `Space s r`: recent files;
- `Space s d`: diagnostics;
- `Space s s` / `Space s S`: document/workspace symbols;
- `Space s j` / `Space s m`: jumps/marks.

Pickers open in **Insert** mode: every letter (including `j` and `k`) types into the query; arrow keys move through results and Enter accepts. Escape switches to **Picker Normal**, where `j`/`k` and arrows navigate, `i` returns to query editing, and Escape closes. `Ctrl-C` closes directly from either mode. The lower border shows the effective mode and configured keys. These modes do not change the underlying editor mode. Files preserves text entered before its first batch; streamed results retain the selected item while it remains present.

Controls are configurable in the `picker` (Insert) and `picker_normal` contexts. The existing `j`/`k` mapping IDs now belong to `picker_normal`; the existing Insert-context Escape mapping invokes `picker.mode.normal`. `picker.cancel` remains the direct-close command. Workspace-edit review is deliberately separate: its existing toggle/apply/reject controls are not query editing.

`Space f r` opens checked workspace replacement. The full parity inventory and missing Telescope sources are tracked in [`plans/neovim-feature-matrix.md`](plans/neovim-feature-matrix.md).

## Diagnostics and LSP

Diagnostics render directly beside the affected source line. Navigating to a diagnostic expands every matching message for that line below it.

| Action | Next | Previous |
|---|---|---|
| Error | `]e` | `[e` |
| Warning | `]w` | `[w` |
| Error or warning | `]d` | `[d` |
| Git hunk | `]h` | `[h` |

`Space x x` opens all workspace problems. The status line shows error/warning counts for the active buffer.

For an existing named file, LSP synchronization uses every in-memory buffer revision. Saving is **not** required for diagnostics, completion, hover, semantic data, or inlay hints to update. Server startup still requires a named file inside a recognized project and a trusted workspace.

`Ctrl-Space` requests completion. The menu is clipped to the focused split, follows selection, identifies item kind and server, and automatically opens available documentation beside the menu. Enter accepts; Escape cancels.

Code navigation uses `g d` for definition, `g D` for declaration, `g I` for implementation, `g y` for type definition, `g r` for references, `g O` for document symbols, and `g K` for signature help.

`K` opens hover documentation. Scroll hover/signature/Git documentation with `j`/`k`, arrow keys, `Ctrl-d`/`Ctrl-u`, or the mouse wheel; close it with `q` or Escape. Completion documentation uses `Ctrl-d`/`Ctrl-u` or the wheel while completion remains active. Scrollable popups show their visible line range in the lower border. Documentation and completion previews are revision-owned and close when their source becomes stale.

Use `:lsp` for installed server/runtime management, `:lsp.server.status` for status, and `:lsp.server.logs` for the live bounded log surface. A `rust-analyzer` PATH entry that is only a rustup proxy is probed before launch. If its active toolchain lacks the component, Dust shows `unavailable` instead of looping in `starting`; press `i` on that PATH row to explicitly run bounded, shell-free `rustup component add rust-analyzer` for the active workspace toolchain. Dust never changes the user toolchain automatically. Use `p` to choose a Dust-managed build instead.

## Notifications, tasks, and logs

Short notifications expire automatically. `:messages` opens the bounded notification history, including errors that have already expired.

Task output and LSP logs update while their surfaces are open and follow the tail until you move away from it. Use:

- `:task.list`
- `:task.output name=<task>`
- `:test.list name=<task>`
- `:lsp.server.logs`

The status line shows active diagnostics, changed Git files, running tasks, LSP identity, workspace root, and cursor position. `Git N files` is the number of changed files in the active workspace Git snapshot, not a changed-line count.

## Terminal

`Ctrl-/` toggles the default terminal. `Ctrl-\\` focuses the active terminal. Double Escape enters terminal Normal mode; Visual selection and yanks use the ordinary register system. `:terminal.list` opens a selectable terminal-session menu rather than a transient message.

Terminal processes are never restored as fake live sessions. Zero-exit terminals close automatically; failed terminals retain bounded output.

## Git

Dust provides a bounded local Git subset without invoking the Git CLI:

- file and explorer status decorations;
- active-buffer hunk signs;
- `]h` / `[h` hunk navigation;
- `Space g h p` hunk preview;
- `:workspace.git.hunks.open` hunk picker;
- exact stage and protected reset of the selected unstaged hunk;
- an explicit `Git N files` changed-file count in the status line.

More advanced branch, history, blame, conflict, remote, and staged-hunk workflows remain delegated to LazyGit until their native Dust workflows are complete.

## Recovery and health

Dirty buffers are journaled asynchronously. Use `:recovery`, `:recover`, or the dashboard recovery action after an unclean exit. Dust never overwrites the original file automatically during recovery.

Use `:health`, `dust --health`, or `application.safe_mode.status` to inspect runtime state. `crash.clear` clears retained crash records.

## Configuration

User configuration is loaded transactionally. Invalid reloads retain the previous working configuration. WhichKey-style help appears after `[ui.key_hints].delay_ms`; a pure prefix such as `g`, `Space`, or `Space s` remains open until the next key or cancellation. Only a prefix that is also a complete command expires through `[input].mapping_timeout_ms`.

- Main configuration: `config.toml`
- Keymaps: `keymaps.toml`
- Dashboard: `dashboard.toml`
- Theme: `themes/*.toml`

Run `dust --check-config` before restarting after large edits. `:config explain key` shows where a resolved value came from; `:keymap inspect key context` explains a binding.


### Select a theme

Use `:theme.select` (or `Space u t` from editor Normal mode). Type to filter, then use Up/Down to preview the selected theme on the actual screen. **Enter applies and remembers it in editor state. Escape or Ctrl-C cancels immediately**, restoring the previously chosen theme without saving the preview. This selection dialog uses the configurable `theme_picker` context, rather than the two-Escape result-picker grammar. A newer accepted configuration reload wins over an older preview or pending discovery.

The compact chooser leaves the editor visible for live preview (up to 64 columns and 12 rows by default). It lists **Dust** (the unchanged original theme), **Catppuccin Latte, Frappé, Macchiato and Mocha**, **Gruvbox Dark**, **Nord**, **Tokyo Night / Storm / Moon / Day**, **Rosé Pine / Moon / Dawn**, and validated `themes/<name>.toml` files beside the accepted user `config.toml`. Identical current/catalog entries appear only once. Theme names are left-aligned; `Builtin`, `Installed` or `Saved` provenance is right-aligned. `Current` is separate from the keyboard selection. This uses the common picker row renderer, including plugin-provider pickers; long source labels are clipped to at most a third of the row. The optional `ui.picker.source_separator` is a prefix for that trailing source column (empty by default). Longer lists scroll; other pickers keep their existing size. `builtin.theme` has its own ordinary-window placement rule. The file's declared `name` must match its filename; names use letters, digits, `-` or `_` (up to 96 UTF-8 bytes). Files are regular, non-symlink TOML files of at most 128 KiB, with the existing semantic roles and validated color fallbacks. Discovery examines at most 256 directory entries and loads at most 31 installed files; it reports invalid files and limits instead of silently applying a fallback. `--clean` skips installed-theme discovery, but does not clear editor state; safe mode skips state restoration. Palette sources and MIT/Apache-2.0 license notices are in `assets/themes/README.md` and `LICENSES/`.

Theme selection does not reload the rest of your configuration or restart tools. The existing atomic state writer saves the chosen palette snapshot under `metadata["ui.theme.snapshot"]` in the configured state file (normally `$DUST_STATE_HOME/session.json`). It restores on the next launch, including when opening a file directly. Preview alone is never saved. State-save errors are reported; the initial notice says **save requested**, not that disk I/O has already completed. Normal persistence settings apply: disabling persistence or `save_on_exit` disables automatic theme saving; disabling `restore_on_startup` disables restoration. `:config.explain key=ui.theme` and `:config.explain key=theme.styles.accent.foreground` identify the effective editor-state selection. An accepted configuration reload replaces the selection and cancels any older pending theme restore. Child terminal programs still control their own output colors.

**No configuration-file write is needed to remember a choice.** To explicitly override the saved selection, set `theme = "<name>"` in your existing `[ui]` section and use `:config.reload`. Selecting a theme never rewrites configuration files. Bundled names are `dust`, `catppuccin-latte`, `catppuccin-frappe`, `catppuccin-macchiato`, `catppuccin-mocha`, `gruvbox-dark`, `nord`, `tokyo-night`, `tokyo-night-storm`, `tokyo-night-moon`, `tokyo-night-day`, `rose-pine`, `rose-pine-moon` and `rose-pine-dawn`. The 14 bundled palettes work offline; chooser discovery does not download anything. Plugins cannot yet contribute themes through their manifests or WIT—use custom Dust TOML files for now, not executable Neovim theme plugins. For names other than the reserved original `dust`, an installed same-name file takes precedence when loading configuration; malformed installed files are reported, not silently replaced by the bundled palette.

### Toggle the file explorer

Press `<leader> f e` (Space, then `f`, then `e` by default) to open or close the explorer. The same shortcut works while the explorer is focused and from the normal editor when the panel remains visible. A customized leader is respected. Escape still closes the explorer; `l` still expands/collapses the selected directory. The command is `:explorer.panel.toggle`; `:explorer.open` and `:explorer.close` retain their explicit behavior.
