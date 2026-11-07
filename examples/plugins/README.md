# Rust component examples

These six small guests use production `wit/dust-plugin.wit` directly. The only
SDK dependency is the same pinned `wit-bindgen` used by the retained test guest.
Sources are Apache-2.0. `guest.core.wasm` files are checked in so editor tests run
real guests without invoking a compiler. They are **core modules with component
metadata**, not directly installable components.

## What you can use

| Example | Practical behavior | Start command |
|---|---|---|
| [autopairs](autopairs/README.md) | Pair insertion, closing-character skipping, paired backspace, native undo/repeat | `:plugin.autopairs.enable` |
| [surround](surround/README.md) | Tree-sitter function text objects and surrounding edits | `:plugin.surround.enable` |
| [oil](oil/README.md) | Edit a directory listing to create, rename and delete files, with confirmation | `:plugin.oil.open` |
| [completion](completion/README.md) | Words from the current buffer in native menu and inline completion | `:plugin.completion.enable` |
| [wrappers](wrappers/README.md) | Ordered before/after command wrappers (demonstration) | `:plugin.wrappersa.enable`, then `:plugin.wrappersb.enable` |
| [mode](mode/README.md) | A temporary review mode, key layer and status segment (demonstration) | `:plugin.mode.enter` |

`dust plugin new mywords` copies the completion guest's shape into a standalone
crate with WIT, lockfile, manifest and its own build/load README. No SDK dependency
besides `wit-bindgen` is needed. `:plugins` opens the browser; Enter on a plugin
shows session enable/disable, reload, folder and permission actions.

## Build and package

From the repository root, in the guest shell (it provides the Wasm target):

```sh
nix develop .#guest
for name in autopairs surround oil completion wrappers mode; do
  (cd examples/plugins/$name &&
    cargo build --locked --release --target wasm32-unknown-unknown &&
    cp target/wasm32-unknown-unknown/release/dust_example_$name.wasm guest.core.wasm)
done
```

For a build check that leaves the checked-in fixtures unchanged, build the host
editor first, then run (replace the binary path if using `CARGO_TARGET_DIR`):

```sh
nix develop .#guest --command scripts/check-plugin-guests "$PWD/target/release/dust"
```

This builds all six examples and a fresh `dust plugin new words` scaffold, with
locked dependencies. Guest build outputs go under the Cargo target directory.

Then package them with the repository's existing component encoder:

```sh
cargo run --locked --example plugin_pack -- packaged-plugins
```

To package a scaffold or another guest instead, pass its core module and manifest:

```sh
cargo run --locked --example plugin_pack -- /absolute/path/to/output \
  /absolute/path/to/guest.core.wasm /absolute/path/to/manifest.toml
```

The destination must not exist. The source manifest is left unchanged.

`packaged-plugins/` then holds one installable folder per example. The packer creates
fresh manifests with the component digests; the all-zero digests beside the
sources are templates, not valid installed manifests.

## Install

In your Dust `config.toml`, select the absolute directory containing the packaged
plugin folders, and enable only the examples you want:

```toml
[plugins]
store = "/absolute/path/to/packaged-plugins"

[plugins.entries.autopairs]
manifest = "autopairs/manifest.toml"
component = "autopairs/guest.wasm"
enabled = true
```

Replace `autopairs` in all three places for another example. Wrappers uses **two**
entries, `wrappersa` and `wrappersb`, backed by the same guest source. Restart Dust,
open a buffer, and run the command in each example's README. Contributions are
removed when the plugin is disabled. No example changes the default keymap.

For autopairs, turn the built-in pairs off so you are testing the guest:

```toml
[editor]
autopairs = false
```

Boundary tests live in `src/app/plugins/abi/example_tests.rs`. Terminal scenarios
live in `scripts/ui-scenarios/plugin-*.keys`. The launcher supplies an isolated
configuration and writes demonstration files, so use a **fresh disposable directory**:

```sh
mkdir -p target/plugin-demo
DUST_EXAMPLE=completion \
DUST_BINARY="$PWD/target/debug/dust" \
DUST_EXAMPLE_PACK="/absolute/path/to/packaged-plugins" \
scripts/ui-shot scripts/ui-scenarios/plugin-completion.keys \
  --binary scripts/ui-scenarios/plugin-launch --cwd target/plugin-demo
```

Build the editor first with `cargo build`. Change `completion` to another example
and choose a new empty working directory for each run.
