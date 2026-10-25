# Argus_App

**An operator console for the [Flipper Zero](https://flipperzero.one/), built for authorized penetration-testing and red-team engagements.**

Argus_App is a fork of the official [qFlipper](https://github.com/flipperdevices/qFlipper) desktop companion (Qt 6 / C++ / QML, GPLv3). It keeps everything qFlipper does — firmware update/repair, screen streaming, backups — and extends it into a full desktop console for operating the device and analyzing what it captures:

- **Richer device control & live data streams** — talk to the Flipper over its RPC/serial link, not just for updates.
- **Offload & datastore** — use the desktop as external storage/compute so the Flipper isn't bottlenecked by its own flash and MCU.
- **Analysis-tool bridges** — export captures to **Wireshark** (pcap/pcapng), feed RF/Sub-GHz captures to an **RF/SDR analyzer**, and provide an **NFC/RFID** workbench.
- **Sarina control API** — a local, scriptable interface so the assistant layer can drive the device and query captured data.

This is a work in progress. See the current design and status:

- **[CLAUDE.md](./CLAUDE.md)** — project overview + the Chymaera Ecosystem coordination board (read this first if you're contributing).
- **[docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md)** — how the app is structured and where extensions attach.
- **[docs/ROADMAP.md](./docs/ROADMAP.md)** — the phased plan and what's done vs. planned.
- **[docs/SARINA_API.md](./docs/SARINA_API.md)** — the local control API (Sarina) command reference.

**Landed so far:** an operator **console** with a live event log; a SQLite **datastore** for sessions/events/captures; a **Wireshark exporter** (`.pcap`/`.pcapng`); **Flipper capture parsing** (`.nfc`, `.sub`); the loopback **Sarina control API** (v1.1); a **live device stream** and **pull all captures** to the datastore (NFC, Sub-GHz, RFID, IR, iButton, Marauder pcaps/logs, Argus app logs) with a SHA-256 per file and no duplicates on repeat pulls, also headless through `qFlipper-cli chymaera watch|pull|pull-all`; and a **warning before official firmware replaces a fork** (see below). See the roadmap for what's next.

## Versions

Argus_App versions read `ARG-<upstream qFlipper>r<N>`, the same scheme as [Argus_Firmware](https://github.com/Artifex-Technologies/Argus_Firmware) (`ARG-1.4.3r1`). The current one is **`ARG-1.3.3r1`**, based on qFlipper 1.3.3, which is still upstream's latest release. The upstream half is `UPSTREAM_VERSION` in [`qflipper_common.pri`](./qflipper_common.pri), bumped by hand when we merge a new upstream release (N restarts at 1). Our half comes from the nearest `ARG-<upstream>r*` git tag. Builds after a tag keep its name, with no `+N` suffix; the commit hash in the log tells them apart. An untagged tree builds as `ARG-1.3.3-dev`.

## Using it with Argus firmware

- **Firmware updates.** The app's Update/Install button installs **official** firmware from Flipper's update channels. When the connected Flipper runs a fork (its `firmware.origin.fork` is not `Official`, e.g. `Argus`), the confirmation dialog says the fork will be replaced. To install Argus firmware, use *Install from file* with an Argus `.tgz` update package, or flash from the firmware repo.
- **Official Flipper apps.** qFlipper has no app catalog. Install apps from [lab.flipper.net](https://lab.flipper.net/apps) (Chrome/Edge, WebSerial) or the Flipper mobile app. They work on Argus firmware as long as it keeps upstream's app API version (88.2 today). Close Argus while installing, since it holds the serial port.
- **Self-update.** The built-in application updater checks *upstream* qFlipper releases, so it would offer to replace Argus with stock qFlipper if upstream ships a newer one. There is no switch for it in the UI: build with `DEFINES+=DISABLE_APPLICATION_UPDATES`, or set `CheckApplicatonUpdates=false` (upstream's spelling) in `~/.config/Flipper Devices Inc/qFlipper.ini` on Linux.

> Intended for authorized security engagements only — general-purpose capture, analysis, and reporting on systems you are permitted to test.

---

## Base: qFlipper

The sections below document the underlying qFlipper application that this project is built on. All of it still applies.

### Graphical desktop application for updating [Flipper Zero](https://flipperzero.one/) firmware via PC
qFlipper is completely open source and based on [Qt](https://www.qt.io/) framework. Runs on Windows, macOS, Linux.

<img alt="qFlipper" width="450" src="https://cdn.flipperzero.one/qflipper_logo_with_connected_flipper.png" />

## Download

There are no prebuilt Argus_App downloads yet; build it from source (below). The link below is for stock qFlipper, which does not include the Argus additions.

Download official qFlipper builds here: [update.flipperzero.one](https://update.flipperzero.one/)

<a href="https://update.flipperzero.one"><img width="300" alt="download qFlipper Button" src="https://cdn.flipperzero.one/download_qFlipper_button.png" /></a>


## Features
* Update Flipper's firmware and supplemental data with a press of one button
* Repair a broken firmware installation
* Stream Flipper's display and control it remotely
* Install firmware from a `.dfu` file
* Backup and restore settings, progress and pairing data
* Automatic self-update feature
* Command line interface

## Build from sources 
### Cloning
Make sure to clone the project together with submodules: 
```sh
git clone https://github.com/Artifex-Technologies/Argus_App.git --recursive
```
(If you already cloned without `--recursive`, run `git submodule update --init --recursive`.)
### Windows

Build requirements:
- MS Visual Studio 2019 or newer
- Qt5 (MSVC build) >= 5.15.0 or Qt6 >= 6.3.0
- NSIS (to generate the installer)

Edit `build_windows.bat` to adjust to your build environment and then run:
```cmd
build_windows.bat
```

Note: STM32 Bootloader driver is not provided in this repository.

### Linux
#### Docker build (AppImage, official)

Setup dev container by running:
```sh 
docker compose up -d
```
Compile qFlipper by running:
```sh
docker compose exec dev ./build_linux.sh
```

#### Arch / CachyOS / Labyrinth OS (quickest path)
```sh
sudo pacman -S --needed qt6-base qt6-declarative qt6-svg qt6-5compat qt6-serialport qt6-tools libusb sqlite
git clone --recursive https://github.com/Artifex-Technologies/Argus_App.git
cd Argus_App && mkdir build && cd build
qmake6 ../qFlipper.pro CONFIG+=release
make -j$(nproc)
./qFlipper-cli chymaera selftest   # should print SELF-TEST PASSED
```
This produces `build/qFlipper` (GUI) and `build/qFlipper-cli` without installing anything. After creating a new version tag, run `make qmake_all` before `make`, or the binaries keep the old version string. Details and test results are in [docs/LINUX_BUILD.md](./docs/LINUX_BUILD.md).

#### Standalone build
Build requirements:
- Qt5 >= 5.15.0 or Qt6 >= 6.3.0
- libusb >= 1.0.16
- zlib >= 1.2.0

Make sure to install the following Qt modules (the exact package names might differ slightly depending on your Linux distribution): 
```
base, tools, serialport, declarative,  wayland, [quickcontrols2, graphicaleffects] (Qt5 only), qt5-compat (Qt6 only)
```
Then run:
```sh
mkdir build && cd build
qmake ../qFlipper.pro PREFIX=/path/to/install/dir -spec linux-g++ CONFIG+=qtquickcompiler && 
make qmake_all && make && make install
```
**Caution:** `make install`ing to the system prefix is not recommended. Instead, use this method for building distro-specific packages. 
In this case, it is possible to disable the built-in application update feature by passing `DEFINES+=DISABLE_APPLICATION_UPDATES` to the `qmake` call.

### MacOS

Build requirements:

- Xcode or command line tools
- Qt6 6.3.1 static universal from [Flipper brew tap](https://github.com/flipperdevices/homebrew-flipper)
- libusb 1.0.24 universal from [Flipper brew tap](https://github.com/flipperdevices/homebrew-flipper)
- [dmgbuild](https://pypi.org/project/dmgbuild/) >= 1.5.2

If you want to sign binaries, set `SIGNING_KEY` environment variable:

	export SIGNING_KEY="Your Developer Key ID"

Building, signing and creating package:

	./build_mac.sh

Resulting image can be found in: `build_mac/qFlipper.dmg`

## Run

### Linux

Before the first run, let your user reach the Flipper. This installs udev rules for the Flipper's serial port, its DFU mode and the Wi-Fi devboard, and asks for your sudo password:
```sh
sh setup_rules.sh        # from the repo root; choose [I]nstall, then replug the Flipper
```

From a standalone build, run `build/qFlipper` directly. To get an **Argus** entry in the app menu that runs your build (rebuilds take effect at once, no sudo):
```sh
ln -sfn "$PWD/build/qFlipper"     ~/.local/bin/argus
ln -sfn "$PWD/build/qFlipper-cli" ~/.local/bin/argus-cli
install -Dm644 application/assets/icons/Argus-512.png ~/.local/share/icons/hicolor/512x512/apps/argus.png
cat > ~/.local/share/applications/argus.desktop <<EOF2
[Desktop Entry]
Type=Application
Name=Argus
GenericName=Flipper Zero operator console
Exec=$PWD/build/qFlipper
Icon=argus
StartupWMClass=qFlipper
Categories=Development;
Terminal=false
EOF2
```
On Labyrinth OS, the Yggdrasil panel reads the app list only when it starts, so restart it once to see the new entry: `systemctl --user restart 'app-yggdrasil\x2dpanel@autostart.service'`.

**Never run `qFlipper-cli` with no arguments while a Flipper is attached** unless you mean it: that performs a firmware update/repair (official firmware).

From the AppImage (`build_linux.sh`):
```sh
./build/qFlipper-x86_64.AppImage
```

or just launch the file above from your favourite file manager.
You will likely need to set up udev rules in order to use qFlipper as a normal user:
```sh
./qFlipper-x86_64.AppImage rules install [/optional/path/to/rules/dir]
```

#### Package managers support
See [contrib](./contrib) for available options.

## Project structure
- `application` - The main graphical application, written mostly in QML.
- `cli` - The command line interface, provides nearly all main application's functionality.
- `backend` - The backend library, written in C++. Takes care of most of the logic.
- `dfu` - Low level library for accessing USB and DFU devices.
- `plugins` - Protobuf-based communication protocol support.
- `3rdparty` - Third-party libraries.
- `contrib` - Contributed packages and scripts.
- `driver-tool` - DFU driver installation tool for Windows (based on `libwdi`).
- `docker` - Docker configuration files.
- `installer-assets` - Supplementary data for deployment.
- `docs` - Chymaera engineering docs (architecture, roadmap).

## Reporting bugs
qFlipper is a project under active development. Please report any encountered bugs to make it better!

The (mostly) complete guide is located [here](./.github/ISSUE_TEMPLATE/bug_report.md).

## Known bugs

* Sometimes Flipper's serial port doesn't get recognised by the OS, which leads to firmware update errors. This is a firmware issue.
* On some systems, there is noticeable flicker during opening, closing or resizing of the log area.
* Release source archives are automatically generated by Github and are unsuitable for building as they do not contain submodules.
* `qFlipper-cli --version` (and `--help`) sometimes crashes on exit: the startup update check is still running on a network thread when the program quits. It's harmless, and real commands are not affected.

## License and attribution

Argus_App is a fork of [qFlipper](https://github.com/flipperdevices/qFlipper) by Flipper Devices Inc. and is distributed under the **GNU General Public License v3** — see [LICENSE](./LICENSE). Full credit for the base application goes to the qFlipper authors; the Chymaera additions build on their work under the same license.
