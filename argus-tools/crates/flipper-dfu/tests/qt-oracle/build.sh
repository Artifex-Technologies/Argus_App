#!/bin/sh
# Builds the oracle for tests/qt_oracle.rs from the C++ app's own dfu/ sources:
#   sh build.sh OUT  (then: ARGUS_DFU_ORACLE=OUT cargo test -p flipper-dfu --test qt_oracle)
# Needs g++ and Qt 6 Core (pkg-config Qt6Core).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../../../dfu" && pwd)
out=${1:?usage: sh build.sh OUT}
${CXX:-g++} -std=c++17 -O1 -fPIC -Wall -I"$src" $(pkg-config --cflags Qt6Core) \
    "$here/dfu_oracle.cpp" "$src/dfusefile.cpp" "$src/dfumemorylayout.cpp" "$src/debug.cpp" -o "$out" $(pkg-config --libs Qt6Core)
