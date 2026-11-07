# Dust on Linux

This guide covers everyday editing.

## Start

```sh
dust                 # open the current directory
dust path/to/file    # open a file
dust --clean         # ignore user configuration
dust --safe-mode     # disable plugins, LSP/tasks/debuggers, and session restore
```

A workspace Dust has not seen before asks whether to trust it after the first frame. Press Enter to trust it (LSP servers, tasks and debuggers may then run project code), or Escape to keep editing without those execution paths. Dust remembers that it asked. Use `:trust` later to approve the workspace and `:untrust` to revoke it.

ACP agents currently remain executable in safe mode and untrusted workspaces;
they run as ordinary child processes with access to the workspace.

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
| Find a character on the line | `f`/`F`/`t`/`T`, then `;` / `,` to repeat |
| Change inside quotes, brackets, a word | `ci"`, `ci(`, `ciw` (also `a…` for around, `ip`/`ap` paragraph) |
| Toggle comment | `gcc` line, `gc` + motion, `gc` in Visual |
| Add / delete / replace surrounding pair | `gsa` + motion + char, `gsd` + char, `gsr` + old + new |
| Command palette | `F1` or `Space :` |
| File picker | `Space Space` |
| Workspace grep | `Space /` or `Space s g` |

Yank and paste share the system clipboard, as in LazyVim (`clipboard = "unnamedplus"` under
`[editor.registers]`; set `"off"` to keep them internal). Dust uses `wl-copy`/`wl-paste`,
`xclip` or `xsel`; without them, yanks reach the terminal clipboard through OSC 52.

`Ctrl-h/j/k/l` moves between editor panes and the explorer. `H`/`L` and `[b`/`]b` switch buffers. The cursor remains visible through wrapping, folds, Unicode text, splits, resize, and explorer/terminal layout changes. During repaint, Dust hides the hardware cursor and publishes its final position within a synchronized terminal update; unchanged frames send no cursor commands. This keeps internal painting movements from appearing as cursor jumps or trails.

## Explorer

Open the explorer with `e` on the dashboard, `Space e` in Normal mode, or `:explore`.

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

Explorer deletion opens the shared protected **Delete this path?** dialog with **No** selected. Left/Right, Tab, Y/N choose an answer; Enter submits it; Escape cancels without answering. Dust disables positive confirmation until the complete escaped target is visible, so resize a too-small terminal before choosing **Yes**. Deletion requires confirmation; pressing `d` alone never deletes the path.

Explorer mutations are asynchronous, confined to canonical workspace roots, reject overwrite and symlink mutation, and reconcile open buffers. Deleting an open file keeps its text as a dirty recoverable scratch buffer. Recursive copy and deletion of non-empty directories are intentionally refused; empty directories are supported.

## Finders and search

Dust exposes the daily Telescope/Snacks-style workflows through its bounded picker. `Space /` and `Space s g` open the Grep panel directly—there is no intermediate command prompt—and typing restarts the cancellable live search. File and grep results use a compact result list, live result count, and adaptive file preview: readable panes sit side by side when their measured content fits, otherwise Dust stacks the preview below the results. Grep previews keep the selected match line on-screen and highlight it even when the stacked preview has fewer rows than the captured context. File previews read up to 256 KiB by default through scoped asynchronous VFS reads (`[ui.picker].maximum_preview_bytes`), so files larger than 120 KiB are not rejected:

- `Space Space` or `Space f f`: files in the active workspace roots;
- `Space /` or `Space s g`: workspace text search;
- `Space f b`: open buffers;
- `F1` or `Space :`: commands;
- `Space f r`: recent files;
- `Space s d`: diagnostics;
- `Space s s` / `Space s S`: document/workspace symbols;
- `Space s j` / `Space s m`: jumps/marks.

Pickers open in **Insert** mode: every letter (including `j` and `k`) types into the query; arrow keys move through results and Enter accepts. Escape switches to **Picker Normal**, where `j`/`k` and arrows navigate, `i` returns to query editing, and Escape closes. `Ctrl-C` closes directly from either mode. The lower border shows the effective mode and configured keys. These modes do not change the underlying editor mode. Files preserves text entered before its first batch; streamed results retain the selected item while it remains present.

Controls are configurable in the `picker` (Insert) and `picker_normal` contexts. Insert-context Escape invokes `picker.mode.normal`; `picker.cancel` closes directly. Workspace-edit review uses separate toggle/apply/reject controls.

`Space s r` opens checked workspace replacement.

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

The status line runs along the bottom of the screen, under the terminal panel. It shows the mode, Git branch, file, diagnostics and running tasks on the left, and the LSP server, workspace root, progress through the file and cursor position on the right.

## Terminal

`Ctrl-/` and `Ctrl-\\` are aliases for the same `terminal.toggle` command in the editor, explorer, dashboard and terminal contexts. From the editor, explorer or dashboard they open the default terminal or focus the existing dock; from the terminal they return focus to the editor without hiding or stopping the process. Double Escape enters terminal Normal mode; Visual selection and yanks use the ordinary register system. `:terminal.list` opens a selectable terminal-session menu rather than a transient message.

Terminal processes are never restored as fake live sessions. Zero-exit terminals close automatically; failed terminals retain bounded output.

## Git

Dust does what needs the buffer natively and runs lazygit for the rest:

- file and explorer status decorations, and gutter signs for the active buffer;
- `]h` / `[h` move between hunks; `Space g h p` previews, `Space g h s` stages, `Space g h u`
  unstages and `Space g h r` resets the hunk at the cursor (reset asks for the phrase
  `reset this hunk`);
- `Space g s` lists changed files with the diff beside them (`s` / `u` stage and unstage);
- `Space g x` shows conflicts and lets you pick a side; `Space g w` lists worktrees;
- `Space g g` opens lazygit at the workspace root, `Space g G` in the current file's
  directory, `Space g f` on the current file's history, and `Space g l` / `Space g c` on the
  log. Commit, branch, stash, rebase, push and pull all happen there.

The lazygit commands are ordinary terminal profiles in `[terminals.profiles]`. A profile's
arguments and `cwd` may use `{root}`, `{file}`, `{dir}` and `{line}`; for example
`arguments = ["-c", "exec lazygit -f \"$1\"", "lazygit", "{file}"]` passes the path as a
separate argument instead of splicing it into the shell string.

### Custom terminal profiles

Example profile in `config.toml`:

```toml
[terminals.profiles.lazygit]
program = "/bin/sh"
arguments = ["-c", "exec lazygit"]
cwd = "{root}"
float = true
close_on_exit = true
```

Mapping in `keymaps.toml`:

```toml
[[keymap]]
id = "user.lazygit"
context = "editor"
mode = "normal"
keys = "<leader> g g"
command = "terminal.open"
arguments = { profile = "lazygit" }
```

#### The Git and debugger prefixes

`Space g` and `Space d` show available Git and debugger commands. The table records
which shortcuts are bound; unassigned keys do not invoke another action. Lazygit,
`gh` and `tea` must be installed separately. The `gh pr` profiles require GitHub;
the pull-request picker and review-comment commands also support Gitea/Forgejo via `tea`.

| Keys | Action | Status |
|---|---|---|
| `Space g g` | Lazygit at the workspace root | bound |
| `Space g G` | Lazygit in the current file's directory | bound |
| `Space g c` | Lazygit log | bound |
| `Space g l` | Lazygit log | bound |
| `Space g f` | Lazygit file history | bound |
| `Space g s` | Changed files and diff | bound |
| `Space g x` | Conflicts | bound |
| `Space g w` | Worktrees | bound |
| `Space g d` | Changed hunks in the project | bound |
| `Space g h p` | Preview the hunk at the cursor | bound |
| `Space g b` | Blame the cursor line | bound |
| `Space g B` | Open the file on the forge | bound |
| `Space g Y` | Copy the file's forge URL | bound |
| `Space g p p` | Pick a pull request and check it out in a worktree | bound |
| `Space g p v` | Pull request description (`gh pr view`) | bound |
| `Space g p d` | Pull request diff (`gh pr diff`) | bound |
| `Space g p r` | Submit a review (`gh pr review`) | bound |
| `Space g p c` | Review comments | bound |
| `Space g p a` | Comment on the cursor line | bound |
| `Space g i` | Unassigned | no command |
| `Space d s` | Start a debug session | bound |
| `Space d c` | Continue | bound |
| `Space d O` | Step over | bound |
| `Space d i` | Step in | bound |
| `Space d o` | Step out | bound |
| `Space d p` | Pause | bound |
| `Space d t` | Stop the debuggee | bound |
| `Space d R` | Restart the session | bound |
| `Space d b` | Toggle a breakpoint | bound |
| `Space d B` | Set a condition or hit count | bound |
| `Space d l` | List breakpoints and verification state | bound |
| `Space d w` | Unassigned; use `:dap.watch.add expression=...` | no prompt command |
| `Space d D` | Detach, leaving the process alive | bound |
| `Space d r` | Debug console for the selected frame | bound |

The debug console is opened with `Space d r`; `Space d R` restarts the session.

## Recovery and health

Dirty buffers are journaled asynchronously. Use `:recovery`, `:recover`, or the dashboard recovery action after an unclean exit. Dust never overwrites the original file automatically during recovery.

Use `:health`, `dust --health`, or `application.safe_mode.status` to inspect runtime state. `crash.clear` clears retained crash records.

## Configuration

User configuration is loaded transactionally. Invalid reloads retain the previous working configuration. WhichKey-style help appears after `[ui.key_hints].delay_ms`; a pure prefix such as `g`, `Space`, or `Space s` remains open until the next key or cancellation. Only a prefix that is also a complete command expires through `[input].mapping_timeout_ms`.

Current Linux path resolution is:

- Main configuration: `$DUST_CONFIG_HOME/config.toml` when set, otherwise `$XDG_CONFIG_HOME/dust/config.toml`, otherwise `$HOME/.config/dust/config.toml`.
- Keymaps and dashboard: `keymaps.toml` and `dashboard.toml` beside the resolved `config.toml`.
- Installed themes: `themes/*.toml` beside the resolved `config.toml`.
- State directory: `$DUST_STATE_HOME` when set, otherwise `$XDG_STATE_HOME/dust`, otherwise `$HOME/.local/state/dust`.

Dust accepts those environment paths as supplied, including relative overrides. If no applicable environment/home variable exists, it falls back to `./dust`. Prefer absolute override directories so configuration and state do not depend on the current directory.

Available fields, defaults and bounds are defined by the configuration serializers and validators in `src/config/` and [`assets/defaults/config.toml`](../assets/defaults/config.toml). Bundled mappings and dashboard actions are in [`keymaps.toml`](../assets/defaults/keymaps.toml) and [`dashboard.toml`](../assets/defaults/dashboard.toml). Run `dust --check-config` before restarting after large edits. `:config explain key` shows where a resolved value came from; `:keymap inspect key context` explains a binding.


### Select a theme

Use `:theme.select` (or `Space u C` from editor Normal mode). Type to filter, then use Up/Down to preview the selected theme on the actual screen. **Enter applies and remembers it in editor state. Escape or Ctrl-C cancels immediately**, restoring the previously chosen theme without saving the preview. This selection dialog uses the configurable `theme_picker` context, rather than the two-Escape result-picker grammar. A newer accepted configuration reload wins over an older preview or pending discovery.

The compact chooser leaves the editor visible for live preview (up to 64 columns and 12 rows by default). It lists **Dust**, **Catppuccin Latte, Frappé, Macchiato and Mocha**, **Gruvbox Dark**, **Nord**, **Tokyo Night / Storm / Moon / Day**, **Rosé Pine / Moon / Dawn**, and validated `themes/<name>.toml` files beside the accepted user `config.toml`. Identical current/catalog entries appear only once. Theme names are left-aligned; `Builtin`, `Installed` or `Saved` provenance is right-aligned. `Current` is separate from the keyboard selection. This uses the common picker row renderer, including plugin-provider pickers; long source labels are clipped to at most a third of the row. The optional `ui.picker.source_separator` is a prefix for that trailing source column (empty by default). Longer lists scroll; other pickers keep their existing size. `builtin.theme` has its own ordinary-window placement rule. The file's declared `name` must match its filename; names use letters, digits, `-` or `_` (up to 96 UTF-8 bytes). Files are regular, non-symlink TOML files of at most 128 KiB, with the existing semantic roles and validated color fallbacks. Discovery examines at most 256 directory entries and loads at most 31 installed files; it reports invalid files and limits instead of silently applying a fallback. `--clean` skips installed-theme discovery, but does not clear editor state; safe mode skips state restoration. Palette sources and MIT/Apache-2.0 license notices are in `assets/themes/README.md` and `LICENSES/`.

Theme selection does not reload the rest of your configuration or restart tools. The atomic state writer saves the chosen palette snapshot under `metadata["ui.theme.snapshot"]` in `session.json` below the resolved state directory above. It restores on the next launch, including when opening a file directly. Preview alone is never saved. State-save errors are reported; the initial notice says **save requested**, not that disk I/O has already completed. Normal persistence settings apply: disabling persistence or `save_on_exit` disables automatic theme saving; disabling `restore_on_startup` disables restoration. `:config.explain key=ui.theme` and `:config.explain key=theme.styles.accent.foreground` identify the effective editor-state selection. An accepted configuration reload replaces the selection and cancels any older pending theme restore. Child terminal programs still control their own output colors.

**No configuration-file write is needed to remember a choice.** To explicitly override the saved selection, set `theme = "<name>"` in your existing `[ui]` section and use `:config.reload`. Selecting a theme never rewrites configuration files. Bundled names are `dust`, `catppuccin-latte`, `catppuccin-frappe`, `catppuccin-macchiato`, `catppuccin-mocha`, `gruvbox-dark`, `nord`, `tokyo-night`, `tokyo-night-storm`, `tokyo-night-moon`, `tokyo-night-day`, `rose-pine`, `rose-pine-moon` and `rose-pine-dawn`. The 14 bundled palettes work offline; chooser discovery does not download anything. Plugins cannot yet contribute themes through their manifests or WIT—use custom Dust TOML files for now, not executable Neovim theme plugins. For names other than the reserved original `dust`, an installed same-name file takes precedence when loading configuration; malformed installed files are reported, not silently replaced by the bundled palette.

### Toggle the file explorer

Press `<leader> f e` (Space, then `f`, then `e` by default) to open or close the explorer. The same shortcut works while the explorer is focused and from the normal editor when the panel remains visible. A customized leader is respected. Escape still closes the explorer; `l` still expands/collapses the selected directory. The command is `:explorer.panel.toggle`; `:explorer.open` and `:explorer.close` retain their explicit behavior.
