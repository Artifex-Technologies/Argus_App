#!/bin/sh
# Builds the oracle for tests/qt_oracle.rs and tests/qt_device.rs from the C++ app's own dfu/ sources:
#   sh build.sh OUT  (then: ARGUS_DFU_ORACLE=OUT cargo test -p flipper-dfu --test qt_oracle --test qt_device)
# The parsers and DfuseDevice are the app's; USBDevice is fakeusbdevice.cpp, which opens nothing (the
# requests go to the test over stdin/stdout). Needs g++, moc and Qt 6 Core (pkg-config Qt6Core).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../../../dfu" && pwd)
out=${1:?usage: sh build.sh OUT}
moc=${MOC:-$(pkg-config --variable=libexecdir Qt6Core)/moc}
gen=$(mktemp -d)
trap 'rm -rf "$gen"' EXIT
"$moc" "$src/libusb/usbdevice.h" -o "$gen/moc_usbdevice.cpp"
"$moc" "$src/dfusedevice.h" -o "$gen/moc_dfusedevice.cpp"
${CXX:-g++} -std=c++17 -O1 -fPIC -Wall -DUSB_BACKEND_LIBUSB -I"$src" $(pkg-config --cflags Qt6Core) \
    "$here/dfu_oracle.cpp" "$here/fakeusbdevice.cpp" "$src/dfusefile.cpp" "$src/dfumemorylayout.cpp" "$src/dfusedevice.cpp" \
    "$src/usbdeviceinfo.cpp" "$src/debug.cpp" "$gen/moc_usbdevice.cpp" "$gen/moc_dfusedevice.cpp" \
    -o "$out" $(pkg-config --libs Qt6Core)
