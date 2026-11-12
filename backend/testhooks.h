#pragma once

#include <QString>
#include <QtGlobal>

// Test-only hook for running qFlipper-cli against the fake Flipper of the Rust port
// (argus-tools/crates/flipper-fake, docs/RUST_PORT.md). When ARGUS_TEST_SERIAL_PORT names a
// serial port (a pseudo-terminal), the app:
//   - does not watch USB at all (so a real Flipper plugged in is never seen or opened),
//   - treats that port as the one connected Flipper,
//   - does not raise DTR on it (a pseudo-terminal has no modem lines),
//   - does not fetch the firmware update directory (no network),
//   - takes a restart, factory reset or the start of the updater as done once its request is
//     sent (a pseudo-terminal has no USB to come back on; the fake ends the session as the
//     device would), and never fetches region data for a firmware update.
// Unset (always, outside tests), nothing changes.
namespace Argus {
inline QString testSerialPort()
{
    return qEnvironmentVariable("ARGUS_TEST_SERIAL_PORT");
}

inline bool isTestSerialPort()
{
    return !testSerialPort().isEmpty();
}
}
