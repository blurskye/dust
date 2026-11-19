# Linux release

The current release version is **0.3.3**. Package versions come from `Cargo.toml`;
keep its entry in `Cargo.lock` and the AppStream release in
`packaging/dev.dustyhq.Dusty.metainfo.xml` in sync when releasing.

Pushing a tag `vX.Y.Z` that matches the version in `Cargo.toml` runs
`.github/workflows/release.yml`, which publishes a GitHub release. Running the workflow
by hand builds the same files as workflow artifacts without publishing.

The workflow builds static x86_64 binaries with musl
(`cargo build --release --locked --target x86_64-unknown-linux-musl --features static`;
the `static` feature builds OpenSSL and libgit2 in), avoiding a dependency on the
distribution's glibc. They need x86_64 Linux. The editor (`dusty`) and Dusty Git
(`dusty-git`, the standalone Git workspace) are separate downloads and packages; the
editor has Dusty Git built in as `Space g g`, so install `dusty-git` only to run it on its
own. File names carry no version, so
`https://github.com/dustyhq/dusty/releases/latest/download/<file>` always fetches the
newest release.

| Editor | Dusty Git | Install |
| --- | --- | --- |
| `dusty-x86_64-linux` | `dusty-git-x86_64-linux` | `mkdir -p ~/.local/bin && install -m 0755 dusty-x86_64-linux ~/.local/bin/dusty` |
| `dusty-x86_64-linux.tar.zst` | `dusty-git-x86_64-linux.tar.zst` | the binary and licenses in a `dusty/` or `dusty-git/` folder |
| `dusty_amd64.deb` | `dusty-git_amd64.deb` | `sudo apt install ./dusty_amd64.deb` |
| `dusty-x86_64.rpm` | `dusty-git-x86_64.rpm` | `sudo dnf install ./dusty-x86_64.rpm` |
| `dusty-x86_64.pkg.tar.zst` | `dusty-git-x86_64.pkg.tar.zst` | `sudo pacman -U dusty-x86_64.pkg.tar.zst` |
| `dusty-x86_64-linux.AppImage` | | `chmod +x dusty-x86_64-linux.AppImage && ./dusty-x86_64-linux.AppImage` |
| `dusty-x86_64.flatpak` | | `flatpak install --user ./dusty-x86_64.flatpak`, then `flatpak run dev.dustyhq.Dusty` |

For Dusty Git, use the same commands with its file names and `~/.local/bin/dusty-git`.
Run Dusty from a terminal and ensure `~/.local/bin` is on `PATH` when installing a bare
binary there.

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

`packaging/build-packages.sh VERSION DUSTY_BINARY [DUSTY_GIT_BINARY]` builds all of these
except the Flatpak from the two binaries (`dusty-git` beside `DUSTY_BINARY` by default),
given `nfpm` and `appimagetool` on `PATH`. The package definitions are
`packaging/nfpm-dusty.yaml` and `packaging/nfpm-dusty-git.yaml`; the Flatpak manifest is
`packaging/flatpak/dev.dustyhq.Dusty.yml`.
