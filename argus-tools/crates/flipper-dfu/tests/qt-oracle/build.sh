#!/bin/sh
# Builds the oracle for tests/qt_oracle.rs and tests/qt_device.rs from the C++ app's own dfu/ sources:
#   sh build.sh OUT  (then: ARGUS_DFU_ORACLE=OUT cargo test -p flipper-dfu --test qt_oracle --test qt_device)
# The parsers and DfuseDevice are the app's; USBDevice is fakeusbdevice.cpp, which opens nothing (the
# requests go to the test over stdin/stdout). Needs g++, moc, Qt 6 Core and Core5Compat (pkg-config Qt6Core
# Qt6Core5Compat; the option bytes reader uses QRegExp).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../../../../../dfu" && pwd)
out=${1:?usage: sh build.sh OUT}
moc=${MOC:-$(pkg-config --variable=libexecdir Qt6Core)/moc}
gen=$(mktemp -d)
trap 'rm -rf "$gen"' EXIT
"$moc" "$src/libusb/usbdevice.h" -o "$gen/moc_usbdevice.cpp"
"$moc" "$src/dfusedevice.h" -o "$gen/moc_dfusedevice.cpp"
"$moc" -I"$src" "$src/device/stm32wb55/stm32wb55.h" -o "$gen/moc_stm32wb55.cpp"
wb=$src/device/stm32wb55
${CXX:-g++} -std=c++17 -O1 -fPIC -Wall -DUSB_BACKEND_LIBUSB -I"$src" $(pkg-config --cflags Qt6Core Qt6Core5Compat) \
    "$here/dfu_oracle.cpp" "$here/fakeusbdevice.cpp" "$src/dfusefile.cpp" "$src/dfumemorylayout.cpp" "$src/dfusedevice.cpp" \
    "$wb/stm32wb55.cpp" "$wb/optionbytes.cpp" "$wb/fusstate.cpp" \
    "$src/usbdeviceinfo.cpp" "$src/debug.cpp" "$gen/moc_usbdevice.cpp" "$gen/moc_dfusedevice.cpp" "$gen/moc_stm32wb55.cpp" \
    -o "$out" $(pkg-config --libs Qt6Core Qt6Core5Compat)
