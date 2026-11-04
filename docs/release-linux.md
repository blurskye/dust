# Linux release

Pushing a tag `vX.Y.Z` that matches the version in `Cargo.toml` runs
`.github/workflows/release.yml`, which publishes a GitHub release. Running the workflow
by hand builds the same files as workflow artifacts without publishing.

The workflow builds one static x86_64 binary with musl
(`cargo build --release --locked --target x86_64-unknown-linux-musl --features static`;
the `static` feature builds OpenSSL and libgit2 in), so it runs on any Linux
distribution. Every package holds that same binary:

| File | Install |
| --- | --- |
| `dust-X.Y.Z-x86_64-linux` | `install -m 0755 dust-X.Y.Z-x86_64-linux ~/.local/bin/dust` |
| `dust-X.Y.Z-x86_64-linux.tar.zst` | binary and licenses in a `dust/` folder |
| `dust-editor_X.Y.Z-1_amd64.deb` | `sudo apt install ./dust-editor_X.Y.Z-1_amd64.deb` |
| `dust-editor-X.Y.Z-1.x86_64.rpm` | `sudo dnf install ./dust-editor-X.Y.Z-1.x86_64.rpm` |
| `dust-editor-X.Y.Z-1-x86_64.pkg.tar.zst` | `sudo pacman -U dust-editor-X.Y.Z-1-x86_64.pkg.tar.zst` |
| `dust-X.Y.Z-x86_64-linux.AppImage` | `chmod +x` it and run it |
| `dust-X.Y.Z-x86_64.flatpak` | `flatpak install --user dust-X.Y.Z-x86_64.flatpak`, then `flatpak run dev.blurskye.Dust` |

The deb, rpm and pacman packages are named `dust-editor` because distributions already
ship a disk usage tool called dust that also installs `/usr/bin/dust`; they conflict
with it. In the Flatpak, programs Dust starts (shells, language servers, git) come from
the Freedesktop runtime rather than the host.

`SHA256SUMS` lists every file, and `SHA256SUMS.sigstore.json` is its Sigstore keyless
signature:

```sh
sha256sum -c --ignore-missing SHA256SUMS
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp 'https://github.com/blurskye/dust/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
```

`packaging/build-packages.sh VERSION BINARY` builds all of these except the Flatpak
from a binary, given `nfpm` and `appimagetool` on `PATH`; the Flatpak manifest is
`packaging/flatpak/dev.blurskye.Dust.yml`.
