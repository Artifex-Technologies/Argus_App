#!/bin/sh
# Builds the oracle for tests/qt_oracle.rs from the C++ app's own update directory, region and fetcher code:
#   sh build.sh OUT  (then: ARGUS_UPDATES_ORACLE=OUT cargo test -p flipper-updates --test qt_oracle)
# Needs g++, moc, Qt 6 Core and Network, and the 3rdparty/nanopb submodule (RegionData encodes with nanopb).
# The oracle uses the network nowhere: RemoteFileFetcher runs with its test hook (ARGUS_TEST_FIRMWARE_DIR).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../../.." && pwd)
be=$repo/backend
pl=$repo/plugins/flipperproto0
msgs=$pl/messages
npb=$repo/3rdparty/nanopb
[ -f "$npb/pb_encode.c" ] || { echo "build.sh: 3rdparty/nanopb is not checked out" >&2; exit 1; }
out=${1:?usage: sh build.sh OUT}
moc=${MOC:-$(pkg-config --variable=libexecdir Qt6Core)/moc}
gen=$(mktemp -d)
trap 'rm -rf "$gen"' EXIT
for h in updateregistry remotefilefetcher flipperupdates; do
    "$moc" -I"$be" "$be/$h.h" -o "$gen/moc_$h.cpp"
done
for c in "$npb/pb_encode.c" "$npb/pb_decode.c" "$npb/pb_common.c" "$msgs"/*.pb.c; do
    ${CC:-cc} -std=c99 -O1 -c -DPB_ENABLE_MALLOC -I"$npb" -I"$msgs" "$c" -o "$gen/$(basename "$c").o"
done
${CXX:-g++} -std=c++17 -O1 -fPIC -Wall -DPB_ENABLE_MALLOC -I"$be" -I"$repo/dfu" -I"$pl" -I"$repo/plugins/protobufinterface" -I"$npb" -I"$msgs" \
    $(pkg-config --cflags Qt6Core Qt6Network) \
    "$here/updates_oracle.cpp" "$be/updateregistry.cpp" "$be/flipperupdates.cpp" "$be/remotefilefetcher.cpp" "$be/failable.cpp" \
    "$be/regioninfo.cpp" "$pl/regiondata.cpp" "$gen"/moc_*.cpp "$gen"/*.o \
    -o "$out" $(pkg-config --libs Qt6Core Qt6Network)
