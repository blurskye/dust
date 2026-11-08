# Dust

Dust is a terminal IDE I use every day. It edits like Vim, with LazyVim-style keys out of the box.

One Rust binary gives you a file explorer, fuzzy finding, Tree-sitter highlighting, LSP support, DAP debugging, Git signs and hunks, terminals, an ACP coding-agent panel, and a tool installer. Settings and keymaps are TOML. No Lua needed.

Language servers, debug adapters, agents and outside tools run as separate programs. Dust can install the ones it supports. Full Git work happens in lazygit, pull requests use `gh` or `tea`. Neovim plugins do not run here.

## A quick look

Starting from the dashboard, finding a file, making Vim edits, then saving to format it.

<video src="https://raw.githubusercontent.com/blurskye/dust/master/docs/media/editing.mp4" controls muted width="100%"></video>

Browsing the explorer, searching workspace text, and jumping with the file finder.

<video src="https://raw.githubusercontent.com/blurskye/dust/master/docs/media/search.mp4" controls muted width="100%"></video>

Hover, go to definition, completion, and an inline type error with rust-analyzer.

<video src="https://raw.githubusercontent.com/blurskye/dust/master/docs/media/lsp.mp4" controls muted width="100%"></video>

Gutter change marks, the built-in terminal, and lazygit opened with Space g g.

<video src="https://raw.githubusercontent.com/blurskye/dust/master/docs/media/git.mp4" controls muted width="100%"></video>

## Install on Linux

Download a binary or package from [releases](https://github.com/blurskye/dust/releases). For the static x86_64 binary:

```sh
mkdir -p ~/.local/bin
install -m 0755 dust-0.2.1-x86_64-linux ~/.local/bin/dust
```

Ensure `~/.local/bin` is on your `PATH`. Debian, RPM, pacman, AppImage and Flatpak instructions and checksum verification are in the [Linux release guide](docs/release-linux.md). Distribution packages are named `dust-editor` to distinguish this editor from the disk-usage tool that also installs a `dust` executable.

To build from source, use the pinned Rust toolchain plus a C compiler, `pkg-config`, OpenSSL and zlib development files, or enter the Nix development shell:

```sh
nix develop          # optional; provides the Linux build environment
cargo build --release --locked
./target/release/dust
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for tests and plugin-fixture builds.

## Install on macOS

Download `dust-0.2.1-aarch64-macos.tar.gz` for Apple Silicon or `dust-0.2.1-x86_64-macos.tar.gz` for Intel from [releases](https://github.com/blurskye/dust/releases). Check the archive's SHA-256 against its entry in `SHA256SUMS`, then extract and install it:

```sh
tar -xzf dust-0.2.1-aarch64-macos.tar.gz
mkdir -p ~/.local/bin
install -m 0755 dust ~/.local/bin/dust
```

Put `~/.local/bin` on `PATH`. These binaries are not Developer ID signed or notarized. If macOS quarantines the verified executable, run:

```sh
xattr -d com.apple.quarantine ~/.local/bin/dust
```

See the [macOS and Windows guide](docs/user-guide-macos-windows.md) for configuration paths and shell selection.

## Install on Windows

Download `dust-0.2.1-x86_64-windows.zip` from [releases](https://github.com/blurskye/dust/releases) and check its SHA-256 against its entry in `SHA256SUMS`. In PowerShell:

```powershell
Get-FileHash dust-0.2.1-x86_64-windows.zip
Expand-Archive dust-0.2.1-x86_64-windows.zip -DestinationPath "$env:LOCALAPPDATA\Programs\Dust"
```

Add `%LOCALAPPDATA%\Programs\Dust` to your user `PATH` and run `dust` in Windows Terminal. The binary is not Authenticode signed, so SmartScreen may ask before the first run. The [macOS and Windows guide](docs/user-guide-macos-windows.md) covers configuration paths, shells and what differs on Windows.

## First run

```sh
dust                 # dashboard in the current directory
dust path/to/file    # edit a file
dust path/to/project # dashboard rooted at a directory
dust --clean         # use bundled configuration
```

Only trust workspaces whose code you are willing to execute. Escape dismisses the workspace trust prompt; `:trust` approves it later. ACP agents are ordinary child processes and can run even in untrusted workspaces or safe mode.

`i` inserts text. Escape goes back to Normal mode. `:w` saves and `:q` quits. Space is the leader. `Space Space` finds files, `Space e` opens the explorer, `Space /` searches the workspace.

`F1` opens the command palette. `:Tutor` opens the interactive tutor. `Space c m` opens the tool installer. `Space g g` opens lazygit when installed.

On Linux, configuration defaults to `~/.config/dust/config.toml`, with `keymaps.toml` and `dashboard.toml` beside it. See the [Linux user guide](docs/user-guide-linux.md) for XDG overrides, trust, recovery and daily workflows. Bundled [settings](assets/defaults/config.toml), [keymaps](assets/defaults/keymaps.toml) and [dashboard actions](assets/defaults/dashboard.toml) are the reference defaults.

## Plugins

Dust loads WebAssembly Component Model plugins. A plugin can add commands, modes and keys, edit buffers with normal undo, draw highlights and signs, publish diagnostics, provide completion and formatters, and add status-line parts. Its manifest decides what it may do. File, process and network access need your grant. Granting process execution gives it normal user-process access, not a sandbox.

Open **Plugins** on the dashboard or run `:plugins` to inspect installed plugins, permissions and failures, enable or disable them for the session, reload, or open their folders. `dust plugin new mywords` creates a ready-to-build Rust guest with build, pack and load steps in its README.

The [Plugin SDK](docs/plugin-sdk.md) documents the WIT contract, manifest, build commands and examples. [Registry client documentation](docs/registry-client.md) describes package installation and verification.

## License

Dust is **GPL-3.0-or-later**; see [LICENSE](LICENSE). The public plugin ABI and SDK contract are **Apache-2.0**. Bundled third-party notices and license texts are in [NOTICE](NOTICE) and [LICENSES/](LICENSES/).

[Architecture](ARCHITECTURE.md) · [Contributing](CONTRIBUTING.md) · [Documentation](docs/README.md)
