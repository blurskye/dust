# Linux release

```sh
cargo build --release --locked
tar --zstd -cf target/dust-linux-x86_64.tar.zst -C target/release dust
```

`.github/workflows/release.yml` builds the archive, writes its SHA-256 and signs it
with Sigstore keyless `cosign sign-blob`. Publishing to `https://dust.blurskye.dev/`
is a separate manual step.

Install by copying the binary onto your `PATH`:

```sh
install -m 0755 target/release/dust ~/.local/bin/dust
```
