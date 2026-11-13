#!/bin/sh
# A smoke test of the real argus-app binary inside Yggdrasil's headless compositor: the window opens at qFlipper's size,
# connects to flipper-fake on a Unix socket (the only device it is given), draws, asks the fake only for reads, and
# ends when the session closes it (Super+Q). The test itself is Yggdrasil's (packages/compositor/tests/apps.rs,
# the_argus_app_opens_reaches_the_fake_flipper_and_closes); this script builds both sides and runs it the way
# Yggdrasil's apps-podman.sh runs its own: in a rootless podman container with no devices, no network, no system bus
# and nothing of the running desktop, the programs mounted read-only.
#
#   sh argus-tools/crates/argus-app/smoke-yggdrasil.sh YGGDRASIL_CHECKOUT
#
# Needs podman and Yggdrasil's test image (made once by its apps-podman.sh). That image has no SQLite, which the app
# links; the host's libsqlite3 is mounted read-only beside the programs. Nothing is downloaded here.
set -eu

[ $# -eq 1 ] || { echo "usage: sh smoke-yggdrasil.sh YGGDRASIL_CHECKOUT" >&2; exit 2; }
ygg=$(cd "$1" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
argus=$(cd "$here/../.." && pwd)
image=localhost/yggdrasil-headless-test

command -v podman >/dev/null || { echo "smoke-yggdrasil: podman is not installed" >&2; exit 2; }
podman image exists "$image" || { echo "smoke-yggdrasil: no $image yet (Yggdrasil's packages/compositor/tests/apps-podman.sh makes it)" >&2; exit 2; }
sqlite=$(for d in /usr/lib /usr/lib64 /usr/lib/x86_64-linux-gnu; do [ -e "$d/libsqlite3.so.0" ] && { echo "$d/libsqlite3.so.0"; break; }; done)
[ -n "$sqlite" ] || { echo "smoke-yggdrasil: no libsqlite3.so.0 on this machine" >&2; exit 2; }

(cd "$argus" && cargo build --locked -p argus-app -p flipper-fake)
(cd "$ygg/packages/compositor" && cargo build --locked)
test_bin=$(cd "$ygg/packages/compositor" && cargo test --locked --no-run --test apps --message-format=json 2>/dev/null |
    grep -o '"executable":"[^"]*/deps/[^"]*"' | sed 's/"executable":"//; s/"$//')
[ -n "$test_bin" ] || { echo "smoke-yggdrasil: Yggdrasil's apps test was not built" >&2; exit 2; }
ygg_target=$ygg/target
rel=${test_bin#"$ygg_target"/}

mkdir -p "$argus/target/smoke-lib"
cp -f "$(readlink -f "$sqlite")" "$argus/target/smoke-lib/libsqlite3.so.0"

podman run --rm --network=none --user ci \
    -v "$ygg_target:/target:ro" -v "$argus/target:/argus:ro" \
    -e YGGDRASIL_COMPOSITOR=/target/debug/yggdrasil-compositor \
    -e YGGDRASIL_TEST_ARGUS_APP=/argus/debug/argus-app \
    -e YGGDRASIL_TEST_FLIPPER_FAKE=/argus/debug/flipper-fake \
    -e YGGDRASIL_TEST_ARGUS_LIB=/argus/smoke-lib \
    "$image" "/target/$rel" --test-threads=1 the_argus_app
