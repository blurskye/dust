# Linux release

```sh
cargo build --release --locked
mkdir -p target/dist/dust
cp -r target/release/dust LICENSE NOTICE LICENSES target/dist/dust/
tar --zstd --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -cf target/dust-linux-x86_64.tar.zst -C target/dist dust
```

The archive holds a `dust/` folder with the binary and the licenses of Dust and of the
themes and icons it bundles.

`.github/workflows/release.yml` builds the archive, writes its SHA-256 and signs it
with Sigstore keyless `cosign sign-blob`. Publishing to `https://dust.blurskye.dev/`
is a separate manual step.

Install by unpacking the archive and copying the binary onto your `PATH`:

```sh
tar --zstd -xf dust-linux-x86_64.tar.zst
install -m 0755 dust/dust ~/.local/bin/dust
```
