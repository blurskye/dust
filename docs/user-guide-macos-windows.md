# Dusty on macOS and Windows

The editing commands and daily workflows are the same as in the
[Linux user guide](user-guide-linux.md). Use a Unicode-capable terminal and a Nerd
Font for file icons. On Windows, use Windows Terminal or another terminal with
virtual terminal support.

## Release archives

Use `dusty-<version>-aarch64-macos.tar.gz` for Apple Silicon,
`dusty-<version>-x86_64-macos.tar.gz` for Intel Macs and
`dusty-<version>-x86_64-windows.zip` for Windows. Check the archive's SHA-256
(`shasum -a 256 <archive>` on macOS, `Get-FileHash <archive>` in PowerShell)
against its entry in `SHA256SUMS` before running it. The checksum file also has a
Sigstore signature, as described in the [release guide](release-linux.md). This
does not replace OS code signing.

The binaries have no Apple Developer ID or Windows Authenticode signature. If
macOS quarantines a verified download, remove quarantine from that executable
with `xattr -d com.apple.quarantine ~/.local/bin/dusty`. Windows SmartScreen may
ask before the first run; choose "More info", then "Run anyway". Do not disable
system-wide security settings.

## Configuration and state

`DUSTY_CONFIG_HOME` and `DUSTY_STATE_HOME` override all platform defaults. Prefer
absolute paths.

| | Configuration | State |
| --- | --- | --- |
| macOS | `$XDG_CONFIG_HOME/dusty`, else `~/Library/Application Support/Dusty` | `$XDG_STATE_HOME/dusty`, else `~/Library/Application Support/Dusty` |
| Windows | `%APPDATA%\Dusty` | `%LOCALAPPDATA%\Dusty` |

`config.toml`, `keymaps.toml`, `dashboard.toml` and installed themes live in the
configuration directory. Recovery, sessions, trust records and managed tools live
under the state directory. `dusty --check-config` validates configuration without
opening the editor.

## Shells and external tools

On macOS the bundled shell profile uses `$SHELL`, falling back to `/bin/sh`. On
Windows it uses the first of `pwsh`, `powershell` and `cmd.exe` found on `PATH`.
Explicit profile programs are not replaced by the default shell.

Dusty finds tools on `PATH` the way the shell does; on Windows that includes the
extensions in `PATHEXT`, so `rg` finds `rg.exe`. Some pinned managed runtimes are
Linux-only, and adapters whose launcher is a POSIX shell script (the JavaScript
debugger among them) must be installed on `PATH` on Windows. The catalogue says
when to install a tool yourself; incompatible downloads are not offered. Nix
integration is not available on Windows. LSP, debugger and agent programs run with
your ordinary user privileges, subject to Dusty's workspace trust and plugin
permissions.

Programs in Dusty's terminal run in a Unix pseudo-terminal on macOS and in ConPTY
on Windows. ConPTY draws the program's screen itself, so on Windows a full-screen
program such as LazyGit may not get mouse reporting or bracketed paste.

macOS uses atomic exchange for checked saves and no-overwrite renames. Local IPC
uses private Unix-domain sockets on both systems.

## Dusty Git keys

Dusty Git's modes, the leader, the staging keys (`s` stage, `u` unstage, `-` toggle)
and the `ui.git.keys = "lazygit"` preset are the same on every system; see
[Modes and the leader](user-guide-linux.md#modes-and-the-leader) in the Linux guide.

## Dusty Git forge tabs

Signing in, anonymous read-only browsing, self-hosted forges, the cache and the keys
work as described under [Forge tabs](user-guide-linux.md#forge-tabs) in the Linux
guide, with these differences in where Dusty looks for files:

- On macOS, fj's `keys.json` is also read from
  `~/Library/Application Support/forgejo-cli.forgejo-cli/` and
  `~/Library/Application Support/Cyborus.forgejo-cli/`, and tea's `config.yml` from
  `~/Library/Application Support/tea/` when there is none under `~/.config/tea/`.
  The cache is `~/.cache/dusty/forge` and host records go to
  `~/.local/state/dusty/forge/hosts.json` unless `XDG_CACHE_HOME`,
  `XDG_STATE_HOME`, `DUSTY_FORGE_CACHE_DIR` or `DUSTY_FORGE_STATE_DIR` say
  otherwise.
- On Windows, Dusty finds the CLIs' sign-in files only through `HOME`,
  `XDG_CONFIG_HOME`, `XDG_DATA_HOME` or `GLAB_CONFIG_DIR`, which Windows does not
  set by default. Use a token variable, or let Dusty ask `gh auth token` or
  `glab auth status`. Without `HOME` or `XDG_CACHE_HOME` the cache stays in memory
  unless `DUSTY_FORGE_CACHE_DIR` names a folder; host records are kept only with
  `XDG_STATE_HOME`, `HOME` or `DUSTY_FORGE_STATE_DIR`.

The forge tutor (`:tutor forge`) needs `python3` or `python` on `PATH` for its
practice forge.
