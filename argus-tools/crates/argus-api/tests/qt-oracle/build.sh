#!/bin/sh
# Builds the Qt oracles for the differential tests of argus-api, QtCore only:
#   sh build.sh OUTDIR   ->  OUTDIR/json-oracle  (Qt's own JSON parser and writer, as the C++ Sarina
#                                                 server uses them; tests/qt_json.rs)
#                            OUTDIR/pcap-oracle  (the C++ app's PcapExporter; tests/pcap.rs)
# then: ARGUS_QT_ORACLES=OUTDIR cargo test -p argus-api
# Needs g++ and Qt 6 Core (pkg-config Qt6Core).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../../../backend/chymaera/exporters" && pwd)
out=${1:?usage: sh build.sh OUTDIR}
mkdir -p "$out"
cxx() { ${CXX:-g++} -std=c++17 -O1 -fPIC -Wall $(pkg-config --cflags Qt6Core) "$@" $(pkg-config --libs Qt6Core); }
cxx "$here/json_oracle.cpp" -o "$out/json-oracle"
cxx -I"$src" "$here/pcap_oracle.cpp" "$src/pcapexporter.cpp" -o "$out/pcap-oracle"
