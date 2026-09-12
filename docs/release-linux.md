> **Current-interface reference, not the target specification.** This document describes release operations implemented in the checkout being used; source and registered CLI behavior prevail over a prose mismatch. Future requirements and delivery classification live in [specs/README.md](specs/README.md).

# Linux release, installation, and rollback

These commands are local release mechanics, not a stable-release claim. The Rust 1.85 floor, independent reproducibility, dependency/advisory review, executed signing, publication, installer qualification and native platform matrix remain Unproven in [delivery status](specs/delivery.md). The checked workflow covers Linux x86-64 only; no macOS or Windows artifact is built or qualified here.

Dust's release owner produces a deterministic USTAR stream compressed as `.tar.zst` with the locked zstd crate, level 19, one stream, and checksum enabled. Entries are sorted, timestamps/UIDs/GIDs are zero, permissions are fixed, and symlinks and traversal names are rejected. The archive contains `bin/dust`, bundled assets, `Cargo.lock`, the canonical release manifest, provenance metadata, and a CycloneDX dependency inventory derived from the exact lockfile.

```sh
cargo build --release --locked
target/release/dust --release-archive target/release/dust \
  --output "$PWD/target/dust-linux.tar.zst"
```

The command prints the archive SHA-256. This is build metadata, not reproducibility or signature evidence. Dust does not generate signing keys and does not claim unsigned artifacts are signed.

The checked release workflow at `.github/workflows/release.yml` requests Sigstore keyless `cosign sign-blob` with GitHub Actions OIDC, writes one `dust-linux-x86_64.tar.zst.sigstore.json` verification bundle, and uploads that bundle with the archive, SHA-256 file and archive metadata as a workflow artifact. The repository contains policy and automation only: no successful workflow run, verified signer identity, independent reproduction, public release or publication to `https://dust.blurskye.dev/` is established by this guide. Domain promotion is an explicitly separate credentialed operator action and is not implemented by the workflow.

Check a frozen API requirement before packaging:

```sh
target/release/dust --check-api-compatibility api/compat-v1.json
```

## User-local install/update

Installation accepts a regular binary, optionally verifies an expected SHA-256, writes a process-named create-new mode-0755 temporary file, syncs it, and atomically renames it to `<prefix>/bin/dust`. It does not write through a `dust` symlink: a resolving symlink is refused and a dangling symlink is replaced by the final rename. An existing binary becomes `dust.previous`.

```sh
target/release/dust --install-binary target/release/dust \
  --install-prefix "$HOME/.local" --install-sha256 HEX
```

The same operation updates an installation. Roll back or uninstall with:

```sh
"$HOME/.local/bin/dust" --install-prefix "$HOME/.local" --rollback-install
"$HOME/.local/bin/dust" --install-prefix "$HOME/.local" --uninstall
```

Current rollback checks only that `dust.previous` resolves to a file before renaming it; that check follows symlinks and does not satisfy the target C-06 no-follow/ownership validation. Use rollback only with a private, inspected user-owned prefix until that Partial implementation gap is closed. Uninstall rejects a resolved managed symlink or non-file, but its current existence check skips a dangling symlink; inspect the private prefix after failure or removal. These commands do not modify shell profiles, desktop integration, system directories, or package-manager databases.
