#pragma once

#include <QString>
#include <QtGlobal>

// Test-only hooks for running qFlipper-cli against the fakes of the Rust port
// (argus-tools/crates/flipper-fake, docs/RUST_PORT.md). Unset (always, outside tests), nothing changes.
//
// ARGUS_TEST_SERIAL_PORT names a serial port (a pseudo-terminal). The app then:
//   - does not watch USB at all (so a real Flipper plugged in is never seen or opened),
//   - treats that port as the one connected Flipper,
//   - does not raise DTR on it (a pseudo-terminal has no modem lines),
//   - does not fetch the firmware update directory (no network),
//   - takes a restart, factory reset or the start of the updater as done once its request is
//     sent (a pseudo-terminal has no USB to come back on; the fake ends the session as the
//     device would), and never fetches region data for a firmware update.
//
// ARGUS_TEST_DEVICE_STATE names a file the test harness writes. The registry does not watch USB;
// it reads that file every 50 ms instead: "dfu N" is the bootloader on ARGUS_TEST_DFU_SOCKET,
// "serial N" the Flipper on ARGUS_TEST_SERIAL_PORT, anything else (or no file) no device; N counts
// the chip's lives. A change is a device leaving and another arriving, as USB hot-plug reports a
// reset (a new N with the same kind too: a reset can come and go between two reads). A serial device that
// leaves also ends its RPC session, as a vanished USB port does (a pseudo-terminal does not vanish).
//
// ARGUS_TEST_DFU_SOCKET names a Unix socket: dfu/libusb/usbdevice.cpp sends every USB request
// there as one line (the line protocol of flipper_dfu::line) and never calls libusb for a device.
// It reads the variable itself (the dfu library does not include this file).
//
// ARGUS_TEST_FIRMWARE_DIR names a folder: RemoteFileFetcher reads every file it is asked for
// (the update directory, firmware files) from there, by the file name of its URL, with the same
// SHA-256 check. While any of these hooks is set, RemoteFileFetcher never uses the network.
namespace Argus {
inline QString testSerialPort()
{
    return qEnvironmentVariable("ARGUS_TEST_SERIAL_PORT");
}

inline bool isTestSerialPort()
{
    return !testSerialPort().isEmpty();
}

inline QString testDeviceStateFile()
{
    return qEnvironmentVariable("ARGUS_TEST_DEVICE_STATE");
}

inline QString testDfuSocket()
{
    return qEnvironmentVariable("ARGUS_TEST_DFU_SOCKET");
}

inline QString testFirmwareDir()
{
    return qEnvironmentVariable("ARGUS_TEST_FIRMWARE_DIR");
}

// Any device hook set: USB is not watched and the network is not used.
inline bool isTestDevice()
{
    return isTestSerialPort() || !testDeviceStateFile().isEmpty() || !testDfuSocket().isEmpty() || !testFirmwareDir().isEmpty();
}
}
