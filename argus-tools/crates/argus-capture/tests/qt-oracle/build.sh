#!/bin/sh
# Builds the Qt oracle for tests/qt_oracle.rs from the C++ app's own parser:
#   sh build.sh OUT  (then: ARGUS_QT_ORACLE=OUT cargo test -p argus-capture --test qt_oracle)
# Needs g++ and Qt 6 Core (pkg-config Qt6Core).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../../../backend/chymaera/exporters" && pwd)
out=${1:?usage: sh build.sh OUT}
${CXX:-g++} -std=c++17 -O1 -fPIC -Wall -I"$src" $(pkg-config --cflags Qt6Core) \
    "$here/oracle.cpp" "$src/flipperfileformat.cpp" -o "$out" $(pkg-config --libs Qt6Core)
