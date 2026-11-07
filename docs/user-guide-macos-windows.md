# Dust on macOS

The editing commands and daily workflows are the same as in the
[Linux user guide](user-guide-linux.md). Use a Unicode-capable terminal and a Nerd
Font for file icons. Windows support is a separate, unfinished port.

## Release archives

Use `dust-<version>-aarch64-macos.tar.gz` for Apple Silicon or
`dust-<version>-x86_64-macos.tar.gz` for Intel. Check the archive with
`shasum -a 256 <archive>` against its entry in `SHA256SUMS` before running it.
The checksum file also has a Sigstore signature, as described in the
[release guide](release-linux.md). This does not replace OS code signing.

The binaries have no Apple Developer ID signature or notarization. If macOS
quarantines a verified download, remove quarantine from that executable with
`xattr -d com.apple.quarantine ~/.local/bin/dust`. Do not disable system-wide
security settings.

## Configuration and state

`DUST_CONFIG_HOME` and `DUST_STATE_HOME` override all platform defaults. Prefer
absolute paths. Configuration otherwise uses `$XDG_CONFIG_HOME/dust`, falling
back to `~/Library/Application Support/Dust`. State uses `$XDG_STATE_HOME/dust`,
with the same fallback.

`config.toml`, `keymaps.toml`, `dashboard.toml` and installed themes live in the
configuration directory. Recovery, sessions, trust records and managed tools live
under the state directory. `dust --check-config` validates configuration without
opening the editor.

## Shells and external tools

The bundled shell profile uses `program = "$SHELL"`. Dust uses the executable
named by `$SHELL`, falling back to `/bin/sh`. Explicit profile programs are not
replaced by the user's default shell.

Some pinned managed runtimes are Linux-only. Install language servers or runtimes
externally when the catalogue says to use PATH; incompatible downloads are not
offered. LSP, debugger and agent programs run with your ordinary user privileges,
subject to Dust's workspace trust and plugin permissions.

macOS uses atomic exchange for checked saves and no-overwrite renames. Local IPC
uses private Unix-domain sockets. Terminal programs control their own colors.
