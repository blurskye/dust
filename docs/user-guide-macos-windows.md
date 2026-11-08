# Dust on macOS and Windows

The editing commands and daily workflows are the same as in the
[Linux user guide](user-guide-linux.md). Use a Unicode-capable terminal and a Nerd
Font for file icons. On Windows, use Windows Terminal or another terminal with
virtual terminal support.

## Release archives

Use `dust-<version>-aarch64-macos.tar.gz` for Apple Silicon,
`dust-<version>-x86_64-macos.tar.gz` for Intel Macs and
`dust-<version>-x86_64-windows.zip` for Windows. Check the archive's SHA-256
(`shasum -a 256 <archive>` on macOS, `Get-FileHash <archive>` in PowerShell)
against its entry in `SHA256SUMS` before running it. The checksum file also has a
Sigstore signature, as described in the [release guide](release-linux.md). This
does not replace OS code signing.

The binaries have no Apple Developer ID or Windows Authenticode signature. If
macOS quarantines a verified download, remove quarantine from that executable
with `xattr -d com.apple.quarantine ~/.local/bin/dust`. Windows SmartScreen may
ask before the first run; choose "More info", then "Run anyway". Do not disable
system-wide security settings.

## Configuration and state

`DUST_CONFIG_HOME` and `DUST_STATE_HOME` override all platform defaults. Prefer
absolute paths.

| | Configuration | State |
| --- | --- | --- |
| macOS | `$XDG_CONFIG_HOME/dust`, else `~/Library/Application Support/Dust` | `$XDG_STATE_HOME/dust`, else `~/Library/Application Support/Dust` |
| Windows | `%APPDATA%\Dust` | `%LOCALAPPDATA%\Dust` |

`config.toml`, `keymaps.toml`, `dashboard.toml` and installed themes live in the
configuration directory. Recovery, sessions, trust records and managed tools live
under the state directory. `dust --check-config` validates configuration without
opening the editor.

## Shells and external tools

On macOS the bundled shell profile uses `$SHELL`, falling back to `/bin/sh`. On
Windows it uses the first of `pwsh`, `powershell` and `cmd.exe` found on `PATH`.
Explicit profile programs are not replaced by the default shell.

Dust finds tools on `PATH` the way the shell does; on Windows that includes the
extensions in `PATHEXT`, so `rg` finds `rg.exe`. Some pinned managed runtimes are
Linux-only, and adapters whose launcher is a POSIX shell script (the JavaScript
debugger among them) must be installed on `PATH` on Windows. The catalogue says
when to install a tool yourself; incompatible downloads are not offered. Nix
integration is not available on Windows. LSP, debugger and agent programs run with
your ordinary user privileges, subject to Dust's workspace trust and plugin
permissions.

Programs in Dust's terminal run in a Unix pseudo-terminal on macOS and in ConPTY
on Windows. ConPTY draws the program's screen itself, so on Windows a full-screen
program such as LazyGit may not get mouse reporting or bracketed paste.

macOS uses atomic exchange for checked saves and no-overwrite renames. Local IPC
uses private Unix-domain sockets on both systems.
