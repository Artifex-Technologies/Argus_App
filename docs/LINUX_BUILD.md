# Building and trying Argus_App on Arch / CachyOS / Labyrinth OS (checked 2026-10-16)

Built from a fresh clone, outside the source tree, on Labyrinth OS (CachyOS, Qt 6.12). No Flipper was attached, so everything that needs a device is untested.

## Packages
`qt6-base qt6-declarative qt6-svg qt6-5compat qt6-serialport qt6-tools libusb sqlite` (all in the official repos; `qt6-serialport` and `qt6-5compat` are the two people forget).

## Build (shadow build, nothing is installed)
```sh
git clone --recursive https://github.com/Artifex-Technologies/Argus_App.git
cd Argus_App && mkdir build && cd build
qmake6 ../qFlipper.pro CONFIG+=release
make -j$(nproc)          # about 3 minutes on 16 threads
```
Produces `build/qFlipper` (GUI) and `build/qFlipper-cli`. The `--recursive` matters (nanopb is a submodule). `build_linux.sh` is the AppImage path and needs linuxdeploy.

## What was run, and what it showed
- `qFlipper-cli chymaera selftest`: **11 of 11 pass** (datastore open/session/event/capture, pcap and pcapng writers, `.sub` and `.nfc` parsing, the control API listening and answering `ping`).
- `qFlipper-cli chymaera parse x.sub` and `x.nfc`: correct JSON (frequency, raw sample count and duration; UID).
- `qFlipper-cli chymaera serve 44700`, then JSON lines over TCP: `ping`, `status` and `capture.import` answered as documented; an unknown command gives `{"ok":false,"error":"unknown command: ..."}`.
- `qFlipper` (GUI): starts, shows "CONNECT YOUR FLIPPER / WAITING FOR DEVICES". At start it fetches `update.flipperzero.one/qFlipper/directory.json` and `/firmware/directory.json` (upstream's update check; block the app's network in Labyrinth's Settings > Apps if you do not want that).
- **Never run `qFlipper-cli` without arguments with a Flipper attached unless you mean it:** that performs a firmware update/repair.

## Known rough edges (QML/Qt, not blocking)
- `[qt.qml.usedbeforedeclared] :22:41 Variable "onDialogAccepted" is used before its declaration`
- `QObject::startTimer: Timers cannot be started from another thread` (at start)
- `CustomDialog.qml:59:13: Binding loop detected for property "width"`
- Not tested: a real device (screen streaming, RPC, backups), the console view and datastore in the GUI, the Wireshark export from a live session, the NFC/RF bridges.

## Next
A `PKGBUILD` (so it can be installed and updated like any package; the Labyrinth app store candidate list), and a udev rules check with `setup_rules.sh` on a machine with a Flipper.
