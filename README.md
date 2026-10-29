# Dust

A terminal IDE in one Rust binary that feels like LazyVim out of the box: Vim
editing, file tree, LSP, DAP, Git signs, lazygit, an ACP agent panel and a tool
installer built in, all rebindable in TOML. Unreleased.

- [Product plan](docs/product-plan.md) — what Dust is for and what gets built next
- [Architecture](ARCHITECTURE.md)
- [Linux user guide](docs/user-guide-linux.md)
- [Plugin SDK (ABI 0.2)](docs/plugin-sdk.md)
- Bundled defaults: [config](assets/defaults/config.toml), [keymaps](assets/defaults/keymaps.toml), [dashboard](assets/defaults/dashboard.toml)

Build with `cargo build --locked`, test with `cargo test --locked`.
