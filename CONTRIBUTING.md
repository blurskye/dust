# Contributing to Dust

Dust is one Rust package. Keep changes focused, reuse the existing feature owner,
and add regression tests beside it. Cross-feature tests belong in `src/app/tests/`;
CLI and real-terminal tests belong in `tests/`. See [ARCHITECTURE.md](ARCHITECTURE.md).
Settings, keymaps and labels belong in `assets/defaults/`.

## Build and test

Use the toolchain in `rust-toolchain.toml`. Linux builds need a C compiler,
`pkg-config`, OpenSSL and zlib development files, Git and Python 3. The optional
Nix shell provides the pinned Rust toolchain and development libraries:

```sh
nix develop
cargo build --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

The tests use disposable directories and real child processes. Do not skip a
failing test because a required tool is missing. For CLI and terminal checks:

```sh
cargo run --locked -- --check-config
cargo run --locked -- --check-api-compatibility api/compat-v1.json
cargo build --release --locked
python3 tests/pty_smoke.py --binary target/release/dust
```

## Rebuild the test plugin

Tests load a checked-in Wasm fixture; they do not compile it. When its source or
WIT changes, start from the repository root in a new shell:

```sh
nix develop .#guest
cp wit/dust-plugin.wit tests/fixtures/retained-guest/wit/dust-plugin.wit
cd tests/fixtures/retained-guest
cargo build --locked --release --target wasm32-unknown-unknown --target-dir target
cp target/wasm32-unknown-unknown/release/dust_retained_guest.wasm retained-guest.core.wasm
```

Commit the fixture source, matching WIT and rebuilt binary together. Update the
fixture-digest assertions to match the rebuilt binary, then run the host tests.
See the [fixture README](tests/fixtures/retained-guest/README.md) and
[Plugin SDK](docs/plugin-sdk.md) for the contract and examples.

## UI changes

Check changes in a real terminal, not only grid unit tests. Install Xvfb, kitty,
ImageMagick (`import`) and FiraCode Nerd Font for the screenshot script:

```sh
cargo build --release --locked
scripts/ui-shot scripts/ui-scenarios/tour.keys --binary target/release/dust \
  --size 80x24 120x40 200x60 --out target/ui-shots
```

Use or add a scenario for the changed surface. Inspect its PNGs for clipping,
alignment, contrast and cursor placement; the accompanying text dumps help check
content. Include relevant screenshots with UI pull requests. Also check another
terminal emulator when changing terminal input or output behavior.

## License

Contributions to the editor are licensed under **GPL-3.0-or-later**, like the
project. Contributions to the public WIT ABI, SDK contract and Apache-licensed
plugin fixtures retain **Apache-2.0**. Preserve third-party notices; see
[NOTICE](NOTICE) and [LICENSES/](LICENSES/).
