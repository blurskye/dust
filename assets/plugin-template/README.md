# {{name}} — Dust component plugin

This working starter offers words from the current buffer in Dust's completion
menu and inline preview. It is copied from `examples/plugins/completion` and uses
Dust's production WIT 0.4.0. The WIT and generated guest source are Apache-2.0.

## Build

Install Rust and its `wasm32-unknown-unknown` target (or use `nix develop .#guest`
in a Dust source checkout), then run inside this directory:

```sh
cargo build --locked --release --target wasm32-unknown-unknown
```

## Pack

Use the packer in a Dust 0.2.1 source checkout. Set `DUST_SOURCE` to its absolute
path, then run from this plugin directory in the host development shell:

```sh
cargo run --locked --manifest-path "$DUST_SOURCE/Cargo.toml" --example plugin_pack -- \
  "$PWD/packaged/{{name}}" \
  "$PWD/target/wasm32-unknown-unknown/release/dust_plugin_{{name}}.wasm" \
  "$PWD/manifest.toml"
```

This encodes the core Wasm module as a component and writes `guest.wasm` and a
manifest with its digest. The source manifest's zero digest is only a placeholder.
The destination must not already exist; use a new destination when repacking.

## Load and try it

Add to your Dust `config.toml` (replace the store path with the absolute path):

```toml
[plugins]
store = "/absolute/path/to/{{name}}/packaged"

[plugins.entries.{{name}}]
manifest = "{{name}}/manifest.toml"
component = "{{name}}/guest.wasm"
enabled = true
```

Restart Dust, open a buffer containing a few words, and run
`:plugin.{{name}}.enable`. Type a word prefix and press Ctrl-Space to request
completion. `:plugins` shows the component and its permissions. Choose its row
for enable/disable, reload, and folder actions. Browser toggles last for this
editor session; set `enabled` in config for a permanent choice.

Edit `src/lib.rs` to change the behavior. Commands exported by `activate` must
match the manifest's local command names. Keep capabilities minimal. See
`docs/plugin-sdk.md` and the other five guests under `examples/plugins` in the
Dust checkout for editing, decorations, modes, wrappers, and file access.
