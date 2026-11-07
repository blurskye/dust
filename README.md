# Dust

Dust is a terminal IDE with Vim-style editing and LazyVim-inspired default keys.
One Rust binary includes a file explorer, fuzzy finding, Tree-sitter highlighting,
LSP support, DAP debugging, Git signs and hunks, terminals, an ACP coding-agent
panel, and a tool installer. Settings and keymaps are TOML; no Lua configuration
is required.

Language servers, debug adapters, agents and external tools run as separate
programs. Dust can install supported tools; lazygit handles full Git workflows,
and `gh` or `tea` handles pull requests. Existing Neovim plugins do not run in Dust.

## Install on Linux

Download a binary or package from [releases](https://github.com/blurskye/dust/releases).
For the static x86_64 binary:

```sh
mkdir -p ~/.local/bin
install -m 0755 dust-0.2.0-x86_64-linux ~/.local/bin/dust
```

Ensure `~/.local/bin` is on your `PATH`. Debian, RPM, pacman, AppImage and Flatpak
instructions and checksum verification are in the [Linux release guide](docs/release-linux.md).
Distribution packages are named `dust-editor` to distinguish this editor from
the disk-usage tool that also installs a `dust` executable.

To build from a source checkout, use the pinned Rust toolchain and a C compiler,
`pkg-config`, OpenSSL and zlib development files, or enter the Nix development shell:

```sh
nix develop          # optional; provides the Linux build environment
cargo build --release --locked
./target/release/dust
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for tests and plugin-fixture builds.

## First run

Run Dust in a terminal:

```sh
dust                 # dashboard in the current directory
dust path/to/file    # edit a file
dust path/to/project # dashboard rooted at a directory
dust --clean         # use bundled configuration
```

Only trust workspaces whose code you are willing to execute. Escape dismisses the
workspace trust prompt; `:trust` approves it later. ACP agents are ordinary child
processes and can run even in untrusted workspaces or safe mode.

- `i` inserts text; Escape returns to Normal mode. `:w` saves and `:q` quits.
- Space is the leader key. `Space Space` finds files, `Space e` opens the explorer,
  and `Space /` searches the workspace.
- `F1` opens the command palette; `:Tutor` opens the interactive tutor.
- `Space c m` opens the tool installer; `Space g g` opens lazygit when installed.

Configuration defaults to `~/.config/dust/config.toml`, with `keymaps.toml` and
`dashboard.toml` beside it. See the [Linux user guide](docs/user-guide-linux.md)
for XDG overrides, configuration, trust, recovery and daily workflows. Bundled
[settings](assets/defaults/config.toml), [keymaps](assets/defaults/keymaps.toml)
and [dashboard actions](assets/defaults/dashboard.toml) are the reference defaults.

## Plugins

Dust loads WebAssembly Component Model plugins. A plugin can add commands, modes
and key layers, wrap built-in commands, edit buffers with normal undo and repeat,
draw highlights, virtual text and signs, publish diagnostics, provide completion and
formatters, open windows, prompts and plugin-backed buffers, and add status-line
segments. Capabilities in its manifest control what it may do; files, processes and
network access need the user's grant. Granting process execution gives a plugin
ordinary user-process access, not a filesystem sandbox.

The [Plugin SDK](docs/plugin-sdk.md) documents the production WIT contract,
manifest, build commands and examples. [Registry client documentation](docs/registry-client.md)
describes package installation and verification.

## License

Dust is **GPL-3.0-or-later**; see [LICENSE](LICENSE). The public plugin ABI and SDK
contract are **Apache-2.0**. Bundled third-party notices and license texts are in
[NOTICE](NOTICE) and [LICENSES/](LICENSES/).

[Architecture](ARCHITECTURE.md) · [Contributing](CONTRIBUTING.md) · [Documentation](docs/README.md)
