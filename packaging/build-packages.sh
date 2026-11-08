#!/usr/bin/env bash
# Packages a static release binary for Linux into dist/: a tar archive, the bare
# binary, deb, rpm and pacman packages (nfpm) and an AppImage (appimagetool).
# The Flatpak is built from the same binary by .github/workflows/release.yml.
#
# usage: packaging/build-packages.sh VERSION BINARY
set -euo pipefail
version=$1
binary=$2
cd "$(dirname "$0")/.."
dist=dist
stage=target/package-stage
rm -rf "$dist" "$stage"
mkdir -p "$dist" "$stage/dust"
name=dust-$version-x86_64-linux

# The licenses of Dust and of the themes and icons it bundles ship with the binary.
install -m 0755 "$binary" "$stage/dust/dust"
cp -r LICENSE NOTICE LICENSES "$stage/dust/"
tar --zstd --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -cf "$dist/$name.tar.zst" -C "$stage" dust
install -m 0755 "$binary" "$dist/$name"

for packager in deb rpm archlinux; do
  VERSION=$version nfpm package --config packaging/nfpm.yaml \
    --packager "$packager" --target "$dist/"
done

app=$stage/Dust.AppDir
install -Dm 0755 "$binary" "$app/usr/bin/dust"
ln -s usr/bin/dust "$app/AppRun"
install -Dm 0644 packaging/dev.blurskye.Dust.desktop "$app/dev.blurskye.Dust.desktop"
install -Dm 0644 packaging/dev.blurskye.Dust.desktop "$app/usr/share/applications/dev.blurskye.Dust.desktop"
install -Dm 0644 packaging/dev.blurskye.Dust.svg "$app/dev.blurskye.Dust.svg"
ln -s dev.blurskye.Dust.svg "$app/.DirIcon"
install -Dm 0644 packaging/dev.blurskye.Dust.metainfo.xml "$app/usr/share/metainfo/dev.blurskye.Dust.appdata.xml"
mkdir -p "$app/usr/share/licenses/dust"
cp -r LICENSE NOTICE LICENSES "$app/usr/share/licenses/dust/"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 appimagetool --no-appstream "$app" "$dist/$name.AppImage"
