#!/usr/bin/env bash
# Verify the exact Bun bootstrap artifact against the ordinary glibc Linux
# distributions Dust supports. This is an opt-in release qualification: it needs
# Podman plus network access to fetch the pinned ZIP and the three base images.
set -euo pipefail

version=1.3.14
sha256=a063908ae08b7852ca10939bbdc6ceed3ddabce8fb9402dce83d65d73b36e6c7
url="https://github.com/oven-sh/bun/releases/download/bun-v${version}/bun-linux-x64-baseline.zip"

for command in podman curl unzip sha256sum; do
    command -v "$command" >/dev/null || {
        printf 'missing required command: %s\n' "$command" >&2
        exit 1
    }
done

case "$(uname -m)" in
    x86_64|amd64) ;;
    *)
        printf 'Bun x64 qualification requires an x86_64 host (got %s)\n' "$(uname -m)" >&2
        exit 1
        ;;
esac

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
curl --fail --location --proto '=https' --tlsv1.2 --max-time 180 --output "$workdir/bun.zip" "$url"
printf '%s  %s\n' "$sha256" "$workdir/bun.zip" | sha256sum --check --status
unzip -q "$workdir/bun.zip" -d "$workdir/unpacked"
bun=$(find "$workdir/unpacked" -type f -path '*/bun-linux-x64-baseline/bun' -print -quit)
test -n "$bun"
chmod 755 "$bun"

for image in docker.io/library/debian:12 docker.io/library/ubuntu:24.04 quay.io/fedora/fedora:42; do
    printf 'qualifying %s: ' "$image"
    podman run --rm --network=none -v "$bun:/opt/dust/bun:ro" "$image" /opt/dust/bun --version |
        grep -Fx "$version"
done

printf 'Bun %s glibc runtime qualified.\n' "$version"
