#!/bin/bash
# Builds Argus_App the docs/LINUX_BUILD.md way (qmake6 + make, Qt 6 from the distro) and packs a
# relocatable tarball: bin/, lib/qFlipper/plugins/, share/ and the udev rules. The app finds its
# plugin through bin/../lib/qFlipper/plugins, so the folder can be unpacked anywhere.
# Used by .github/workflows/linux.yml and for release builds. Run from the repo root:
#   .github/scripts/package_linux.sh [build-dir]
set -euo pipefail

SRC="$(pwd)"
BUILD="${1:-build-release}"
JOBS="${JOBS:-$(nproc)}"

mkdir -p "$BUILD"
cd "$BUILD"
qmake6 "$SRC/qFlipper.pro" CONFIG+=release PREFIX=/usr
make qmake_all
make -j"$JOBS"

# The subsystem self-test needs no Flipper and no display.
QT_QPA_PLATFORM=offscreen ./qFlipper-cli chymaera selftest

VERSION="$(git -C "$SRC" describe --tags --abbrev=0 --match='ARG-*r*' 2>/dev/null || echo ARG-dev)"
NAME="argus-app-${VERSION}-linux-x86_64"
STAGE="$(pwd)/stage"
rm -rf "$STAGE" "$NAME" "$NAME.tar.gz" "$NAME.tar.gz.sha256"
make install INSTALL_ROOT="$STAGE"

mkdir "$NAME"
cp -a "$STAGE/usr/." "$NAME/"
strip --strip-unneeded "$NAME/bin/qFlipper" "$NAME/bin/qFlipper-cli" "$NAME"/lib/qFlipper/plugins/*.so
install -m644 "$SRC/LICENSE" "$NAME/LICENSE"
install -m644 "$SRC/setup_rules.sh" "$NAME/setup_rules.sh"
cat > "$NAME/INSTALL.txt" <<TXT
Argus App ${VERSION} for Linux (x86_64)

Unofficial fork of qFlipper. Not affiliated with or endorsed by Flipper Devices Inc.
GPLv3, see LICENSE. Source: https://github.com/Artifex-Technologies/Argus_App

Needs Qt 6 from your distribution (built against the Qt 6 in Arch Linux at release time).
On Arch, CachyOS and other Arch-based systems:
  sudo pacman -S --needed qt6-base qt6-declarative qt6-svg qt6-5compat qt6-serialport libusb

Let your user reach the Flipper (installs udev rules, asks for sudo), then replug it:
  sh setup_rules.sh

Run from this folder:
  ./bin/qFlipper          the desktop app
  ./bin/qFlipper-cli      the command line. Never run it with no arguments while a
                          Flipper is attached: that installs official firmware.
TXT

# No builder user, group or file times in the archive.
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='@0' -czf "$NAME.tar.gz" "$NAME"
sha256sum "$NAME.tar.gz" | tee "$NAME.tar.gz.sha256"
echo "PACKAGE=$(pwd)/$NAME.tar.gz"
