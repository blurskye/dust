# Linux release

The current release version is **0.4.0**. Package versions come from `Cargo.toml`;
keep its entry in `Cargo.lock` and the AppStream release in
`packaging/dev.dustyhq.Dusty.metainfo.xml` in sync when releasing.

Pushing a tag `vX.Y.Z` that matches the version in `Cargo.toml` runs
`.github/workflows/release.yml`, which publishes a GitHub release. Running the workflow
by hand builds the same files as workflow artifacts without publishing.

The workflow builds one static x86_64 binary with musl
(`cargo build --release --locked --target x86_64-unknown-linux-musl --features static`;
the `static` feature builds OpenSSL and libgit2 in), avoiding a dependency on the
distribution's glibc. It needs x86_64 Linux. The same build makes `dusty-git`, the
standalone Git workspace; the archive and the deb, rpm and pacman packages install both,
while the AppImage and the Flatpak hold the editor only (Dusty Git is `Space g g` in it):

| File | Install |
| --- | --- |
| `dusty-X.Y.Z-x86_64-linux` | `mkdir -p ~/.local/bin && install -m 0755 dusty-X.Y.Z-x86_64-linux ~/.local/bin/dusty` |
| `dusty-git-X.Y.Z-x86_64-linux` | `install -m 0755 dusty-git-X.Y.Z-x86_64-linux ~/.local/bin/dusty-git` |
| `dusty-X.Y.Z-x86_64-linux.tar.zst` | `dusty`, `dusty-git` and licenses in a `dusty/` folder |
| `dusty_X.Y.Z-1_amd64.deb` | `sudo apt install ./dusty_X.Y.Z-1_amd64.deb` |
| `dusty-X.Y.Z-1.x86_64.rpm` | `sudo dnf install ./dusty-X.Y.Z-1.x86_64.rpm` |
| `dusty-X.Y.Z-1-x86_64.pkg.tar.zst` | `sudo pacman -U dusty-X.Y.Z-1-x86_64.pkg.tar.zst` |
| `dusty-X.Y.Z-x86_64-linux.AppImage` | `chmod +x dusty-X.Y.Z-x86_64-linux.AppImage && ./dusty-X.Y.Z-x86_64-linux.AppImage` |
| `dusty-X.Y.Z-x86_64.flatpak` | `flatpak install --user ./dusty-X.Y.Z-x86_64.flatpak`, then `flatpak run dev.dustyhq.Dusty`; `flatpak run --command=dusty-git dev.dustyhq.Dusty` opens Dusty Git |

Replace `X.Y.Z` with `0.4.0` for this release. Run Dusty from a terminal and ensure
`~/.local/bin` is on `PATH` when installing the bare binary there.

In the Flatpak, programs Dusty starts (shells, language servers, git) come from
the Freedesktop runtime rather than the host.

`SHA256SUMS` lists every file, and `SHA256SUMS.sigstore.json` is its Sigstore keyless
signature:

```sh
sha256sum -c --ignore-missing SHA256SUMS
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp '^https://github.com/dustyhq/dusty/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
```

`packaging/build-packages.sh VERSION BINARY [DUSTY_GIT_BINARY]` builds all of these except
the Flatpak from the two binaries (`dusty-git` beside `BINARY` by default), given `nfpm` and `appimagetool` on `PATH`; the Flatpak manifest is
`packaging/flatpak/dev.dustyhq.Dusty.yml`.
