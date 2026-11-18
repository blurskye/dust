#!/usr/bin/env bash
# Packages a static release binary for Linux into dist/: a tar archive, the bare
# binary, deb, rpm and pacman packages (nfpm) and an AppImage (appimagetool).
# The Flatpak is built from the same binary by .github/workflows/release.yml.
# The archive and the packages also carry dusty-git, the standalone Git screen; the
# AppImage and the Flatpak run the editor only (Dusty Git is Space g g inside it).
#
# usage: packaging/build-packages.sh VERSION BINARY [DUSTY_GIT_BINARY]
set -euo pipefail
version=$1
binary=$2
dusty_git=${3:-$(dirname "$binary")/dusty-git}
cd "$(dirname "$0")/.."
dist=dist
stage=target/package-stage
rm -rf "$dist" "$stage"
mkdir -p "$dist" "$stage/dusty"
name=dusty-$version-x86_64-linux

# The licenses of Dusty and of the themes and icons it bundles ship with the binary.
install -m 0755 "$binary" "$stage/dusty/dusty"
install -m 0755 "$dusty_git" "$stage/dusty/dusty-git"
cp -r LICENSE NOTICE LICENSES "$stage/dusty/"
tar --zstd --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
  -cf "$dist/$name.tar.zst" -C "$stage" dusty
install -m 0755 "$binary" "$dist/$name"
install -m 0755 "$dusty_git" "$dist/dusty-git-$version-x86_64-linux"

for packager in deb rpm archlinux; do
  VERSION=$version nfpm package --config packaging/nfpm.yaml \
    --packager "$packager" --target "$dist/"
done

app=$stage/Dusty.AppDir
install -Dm 0755 "$binary" "$app/usr/bin/dusty"
ln -s usr/bin/dusty "$app/AppRun"
install -Dm 0644 packaging/dev.dustyhq.Dusty.desktop "$app/dev.dustyhq.Dusty.desktop"
install -Dm 0644 packaging/dev.dustyhq.Dusty.desktop "$app/usr/share/applications/dev.dustyhq.Dusty.desktop"
install -Dm 0644 packaging/dev.dustyhq.Dusty.svg "$app/dev.dustyhq.Dusty.svg"
ln -s dev.dustyhq.Dusty.svg "$app/.DirIcon"
install -Dm 0644 packaging/dev.dustyhq.Dusty.metainfo.xml "$app/usr/share/metainfo/dev.dustyhq.Dusty.appdata.xml"
mkdir -p "$app/usr/share/licenses/dusty"
cp -r LICENSE NOTICE LICENSES "$app/usr/share/licenses/dusty/"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 appimagetool --no-appstream "$app" "$dist/$name.AppImage"
