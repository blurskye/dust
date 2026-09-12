# Dust

A keyboard-first terminal IDE with modal editing, native workspace/LSP/DAP workflows and sandboxed Component extensions. Unreleased; implemented slices are not full product conformance.

- [Authoritative specification](docs/specs/README.md)
- [Delivery status and qualification](docs/specs/delivery.md)
- [Architecture and ownership](ARCHITECTURE.md)
- [Current Linux user guide](docs/user-guide-linux.md)
- [Current plugin SDK (ABI 0.2)](docs/plugin-sdk.md)
- [Documentation map](docs/README.md)

Build with the existing locked Rust toolchain policy: `cargo build --locked`. The declared Rust 1.85 floor currently has a locked-dependency incompatibility; see delivery status before claiming minimum-version or release support.

Documentation validation: `python3 scripts/check-specs.py`. Proposed ABI validation: `cargo test --locked --test spec_contract`. Neither enables the target 0.3 API.
