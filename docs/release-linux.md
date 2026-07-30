# Linux release, installation, and rollback

Dust's release owner produces a deterministic USTAR stream compressed as `.tar.zst` with a pinned zstd crate/version, level 19, one stream, and checksum enabled. Entries are sorted, timestamps/UIDs/GIDs are zero, permissions are fixed, and symlinks and traversal names are rejected. The archive contains `bin/dust`, bundled assets, `Cargo.lock`, the canonical release manifest, provenance metadata, and a CycloneDX dependency inventory derived from the exact lockfile.

```sh
cargo build --release --locked
target/release/dust --release-archive target/release/dust \
  --output "$PWD/target/dust-linux.tar.zst"
```

The command prints the archive SHA-256. This is build metadata, not a reproducibility or signature claim. Dust does not generate signing keys and does not claim unsigned artifacts are signed. Publication policy uses Sigstore keyless `cosign sign-blob` under the release workflow's OIDC identity, publishing the detached signature, certificate, and transparency-log bundle beside the archive at `https://dust.blurskye.dev/`. A local archive is unsigned until those external outputs exist and verify against the documented workflow identity.

Check a frozen API requirement before packaging:

```sh
target/release/dust --check-api-compatibility api/compat-v1.json
```

## User-local install/update

Installation accepts a regular binary, optionally verifies an expected SHA-256, rejects symlink destinations, writes a unique mode-0755 temporary file, syncs it, and atomically renames it to `<prefix>/bin/dust`. An existing binary becomes `dust.previous`.

```sh
target/release/dust --install-binary target/release/dust \
  --install-prefix "$HOME/.local" --install-sha256 HEX
```

The same operation updates an installation. Roll back or uninstall with:

```sh
"$HOME/.local/bin/dust" --install-prefix "$HOME/.local" --rollback-install
"$HOME/.local/bin/dust" --install-prefix "$HOME/.local" --uninstall
```

Rollback requires the regular `dust.previous` file. Uninstall removes only the managed regular files and refuses symlinks or non-files. These commands do not modify shell profiles, desktop integration, system directories, or package-manager databases.
