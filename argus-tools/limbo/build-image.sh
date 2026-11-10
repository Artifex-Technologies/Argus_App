#!/bin/sh
# Builds Limbo's guest image for Argus app rebuilds: /var/lib/limbo/images/argus-ufbt.qcow2.
# Run as root on the host, once (and again for a new firmware API). Needs libguestfs
# (virt-customize) and network for this step only; the build guest itself has none.
#
#   sudo sh build-image.sh [BASE_IMAGE] [SDK_URL_OR_ZIP] [API]
#
# BASE_IMAGE  a cloud-init Arch image (default /var/lib/limbo/images/arch-cloudimg.qcow2,
#             the one Limbo's own integration test uses). It is copied, never changed.
# SDK         the ufbt SDK for the device's firmware. Default: Flipper's official 1.5.1 SDK,
#             API 88.2, the API Argus firmware ARG-1.5.1rN keeps. A local .zip (for example the
#             SDK built from Argus_Firmware with ./fbt) is used with --file instead of --url.
# API         the SDK's API version (default 88.2): the SDK lands in /opt/argus-ufbt/f7-<API>,
#             which is where Argus's build command looks for it.
set -eu
base=${1:-/var/lib/limbo/images/arch-cloudimg.qcow2}
sdk=${2:-https://update.flipperzero.one/builds/firmware/1.5.1/flipper-z-f7-sdk-1.5.1.zip}
api=${3:-88.2}
out=/var/lib/limbo/images/argus-ufbt.qcow2

case "$api" in *[!0-9.]*|'') echo "API must look like 88.2" >&2; exit 2;; esac
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
command -v virt-customize >/dev/null || { echo "install libguestfs (virt-customize) first" >&2; exit 2; }
[ -f "$base" ] || { echo "no base image at $base" >&2; exit 2; }
[ ! -e "$out" ] || { echo "$out exists; move it away first (a profile pins its sha256)" >&2; exit 2; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# The SDK and its toolchain need about 1.5 GB: grow the largest partition (the root) by 6 GB.
root_part=$(virt-filesystems -a "$base" --partitions --long --blkdevs=false | awk 'NR>1 {print $NF, $1}' | sort -n | tail -n 1 | cut -d' ' -f2)
size=$(qemu-img info --output=json "$base" | python -c 'import json,sys; print(json.load(sys.stdin)["virtual-size"])')
qemu-img create -f qcow2 "$tmp/img.qcow2" $((size + 6 * 1024 * 1024 * 1024))
virt-resize --expand "$root_part" "$base" "$tmp/img.qcow2"

if [ -f "$sdk" ]; then
    cp "$sdk" "$tmp/sdk.zip"
    sdk_arg="--file /root/sdk.zip"
    upload="--upload $tmp/sdk.zip:/root/sdk.zip"
else
    sdk_arg="--url $sdk"
    upload=""
fi

# git and tar for the fetch run; python + ufbt (and its toolchain, fetched by ufbt update) for the build.
# The SDK folder is left writable for everyone: the guest is disposable and ufbt writes state there.
# shellcheck disable=SC2086
virt-customize -a "$tmp/img.qcow2" \
    --run-command 'pacman -Syu --noconfirm --needed git tar gzip coreutils python python-pip' \
    $upload \
    --run-command 'python -m venv /opt/ufbt-venv && /opt/ufbt-venv/bin/pip install --no-cache-dir ufbt && ln -sf /opt/ufbt-venv/bin/ufbt /usr/local/bin/ufbt' \
    --run-command "mkdir -p /opt/argus-ufbt && UFBT_HOME=/opt/argus-ufbt/f7-$api /usr/local/bin/ufbt update --hw-target f7 $sdk_arg" \
    --run-command "UFBT_HOME=/opt/argus-ufbt/f7-$api /usr/local/bin/ufbt status > /opt/argus-ufbt/f7-$api.status 2>&1 || true" \
    --run-command 'chmod -R a+rwX /opt/argus-ufbt; rm -f /root/sdk.zip; pacman -Scc --noconfirm'

virt-cat -a "$tmp/img.qcow2" "/opt/argus-ufbt/f7-$api.status" || true
install -m 0644 "$tmp/img.qcow2" "$out"
echo "image: $out"
echo "sha256: $(sha256sum "$out" | cut -d' ' -f1)   <- put this in both profiles' image.sha256"
