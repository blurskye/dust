#!/usr/bin/env bash
# Packages the static Linux release binaries into dist/. The editor (dusty) and Dusty Git
# (dusty-git) are separate: each gets a bare binary, a tar archive and deb, rpm and pacman
# packages (nfpm) of its own, and the editor also an AppImage (appimagetool). The editor's
# Flatpak is built from target/package-stage/dusty by .github/workflows/release.yml.
# File names carry no version, so releases/latest/download/<file> is always the newest.
#
# usage: packaging/build-packages.sh VERSION DUSTY_BINARY [DUSTY_GIT_BINARY]
set -euo pipefail
version=$1
dusty=$2
dusty_git=${3:-$(dirname "$dusty")/dusty-git}
cd "$(dirname "$0")/.."
dist=dist
stage=target/package-stage
rm -rf "$dist" "$stage"
mkdir -p "$dist"

for program in dusty dusty-git; do
  binary=$dusty; [ "$program" = dusty-git ] && binary=$dusty_git
  # The licenses of Dusty and of the themes and icons it bundles ship with each binary.
  mkdir -p "$stage/$program"
  install -m 0755 "$binary" "$stage/$program/$program"
  cp -r LICENSE NOTICE LICENSES "$stage/$program/"
  tar --zstd --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    -cf "$dist/$program-x86_64-linux.tar.zst" -C "$stage" "$program"
  install -m 0755 "$binary" "$dist/$program-x86_64-linux"
  for package in "deb ${program}_amd64.deb" "rpm $program-x86_64.rpm" \
    "archlinux $program-x86_64.pkg.tar.zst"; do
    VERSION=$version nfpm package --config "packaging/nfpm-$program.yaml" \
      --packager "${package%% *}" --target "$dist/${package#* }"
  done
done

app=$stage/Dusty.AppDir
install -Dm 0755 "$dusty" "$app/usr/bin/dusty"
ln -s usr/bin/dusty "$app/AppRun"
install -Dm 0644 packaging/dev.dustyhq.Dusty.desktop "$app/dev.dustyhq.Dusty.desktop"
install -Dm 0644 packaging/dev.dustyhq.Dusty.desktop "$app/usr/share/applications/dev.dustyhq.Dusty.desktop"
install -Dm 0644 packaging/dev.dustyhq.Dusty.svg "$app/dev.dustyhq.Dusty.svg"
ln -s dev.dustyhq.Dusty.svg "$app/.DirIcon"
install -Dm 0644 packaging/dev.dustyhq.Dusty.metainfo.xml "$app/usr/share/metainfo/dev.dustyhq.Dusty.appdata.xml"
mkdir -p "$app/usr/share/licenses/dusty"
cp -r LICENSE NOTICE LICENSES "$app/usr/share/licenses/dusty/"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 appimagetool --no-appstream "$app" "$dist/dusty-x86_64-linux.AppImage"
