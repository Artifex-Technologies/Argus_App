#!/bin/sh
# Rebuilds the nanopb driver from the C++ app's generated messages and nanopb (the
# 3rdparty/nanopb submodule) and rewrites ../fixtures/nanopb_frames.txt.
# CI runs this and fails if the fixture changes. Needs a C compiler; no Qt.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../../.." && pwd)
msgs="$repo/plugins/flipperproto0/messages"
npb="$repo/3rdparty/nanopb"
[ -f "$npb/pb_encode.c" ] || { echo "gen.sh: 3rdparty/nanopb is not checked out (git submodule update --init 3rdparty/nanopb)" >&2; exit 1; }
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
${CC:-cc} -std=c99 -O1 -Wall -Werror -DPB_ENABLE_MALLOC -I"$npb" -I"$msgs" -o "$out/driver" \
    "$here/driver.c" "$npb/pb_encode.c" "$npb/pb_decode.c" "$npb/pb_common.c" "$msgs"/*.pb.c
"$out/driver" > "$here/../fixtures/nanopb_frames.txt"
echo "wrote $(wc -l < "$here/../fixtures/nanopb_frames.txt") frames"
