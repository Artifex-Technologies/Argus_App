# CLAUDE.md — Argus_App

## What this is

`Argus_App` is a fork of [qFlipper](https://github.com/flipperdevices/qFlipper) (the official Flipper Zero desktop companion,
Qt 6 / C++ / QML, GPLv3), grown into an operator console for the Flipper Zero in authorized penetration-testing and red-team
engagements: richer control over the serial RPC, captures pulled into a desktop datastore, Wireshark export, and a local
control API for the Sarina assistant. The Qt app is `application/` (QML), `backend/` (C++, our additions under
`backend/chymaera/`) and `cli/`; the desktop side is being ported to Rust in `argus-tools/` (crates `flipper-rpc`,
`flipper-dfu`, `flipper-updates`, `flipper-fake`, `argus-capture`, `argus-api`, `argus-cli`, `argus-app`), described in
[docs/RUST_PORT.md](docs/RUST_PORT.md). The Qt app stays until I switch.

> Scope: legitimate authorized-engagement tooling. Keep functionality general-purpose (capture, analyze, report). Nothing
> whose only purpose is to hide activity from a system's legitimate owner.

## Build and test

Submodules first: `git submodule update --init --recursive`.

| What | Command |
| --- | --- |
| Qt app, Linux | `qmake6 ../qFlipper.pro CONFIG+=release && make` in `build/` (Arch: [docs/LINUX_BUILD.md](docs/LINUX_BUILD.md)); then `qFlipper-cli chymaera selftest` |
| Release tarball | `.github/scripts/package_linux.sh build-release` (also in the `docker/` Arch container) |
| Rust (argus-tools) | `cd argus-tools && systemd-run --user --scope -q -p MemoryMax=8G -p MemorySwapMax=0 env CARGO_BUILD_JOBS=4 nice -n 19 cargo test --locked --workspace` |
| Windows / macOS | `build_windows.bat` (MSVC 2019+, NSIS) / `./build_mac.sh` |

Cap every build: MemoryMax=8G and 4 jobs (`make -j4`, `CARGO_BUILD_JOBS=4`). Linux device access needs udev rules
(`sh setup_rules.sh`, root): never run it from a session.

CI, every push: `linux.yml` (an `archlinux:base-devel` container) builds and self-tests the Qt app, packages the tarball, and
compares the C++ and Rust versions on the fake Flipper (the CLIs, the Sarina API servers byte for byte, the DfuSe parsers and
protocol, the update directory and region parsers, the full repair and the legacy update); `argus-tools.yml` runs `cargo fmt
--check`, clippy with `-D warnings` (also with the `usb` feature), `cargo test --workspace`, and checks the nanopb fixture is
current. Upstream's `ci.yml` needs Flipper's self-hosted runners and stays manual. Actions are pinned by commit hash. After a
push, watch the runs for the full commit: `gh run list --commit <sha>`.

## Rules

- **The real Flipper.** No test opens `/dev/ttyACM*` or any USB device, nothing flashes, and nothing writes to my real
  Flipper. Nothing changes the device (writes, restore, erase, updates, recovery, reboots into the bootloader) without my
  approval in the app; nothing reachable from the Sarina API transmits, flashes or changes the device. A first run against
  my Flipper happens only with me watching. Tests use `flipper-fake` (over a Unix socket or a pseudo-terminal).
- **Privacy.** No personal details in commits, code or docs: no names of people or devices (my Flipper's name), serial
  numbers, host names, network addresses, SSIDs or MAC addresses. Scan the staged diff before every push. Write docs in the
  first person as the owner, and "Sarina", not "her".
- **No secrets or capture data in git**: no tokens, keys, captures or engagement client data. Real capture output belongs in
  git-ignored paths.
- **Releases** (tags `vX.Y.Z`, the Linux tarball and its `.sha256`): build in a folder whose path holds no user name and scan
  the assets for user names, `/home/`, host names and e-mail addresses before uploading. Release notes say Argus is
  unofficial and not affiliated with Flipper Devices.
- Push every commit to `origin` as you make it; stage files by explicit path; `git pull --rebase` first. Never push to
  `upstream` (flipperdevices/qFlipper: pull fixes from it only).
- Preserve GPLv3 and qFlipper's attribution; every new Rust crate says `license = "GPL-3.0-only"`.

## Contracts

- What Argus_App provides (the Sarina control API, the datastore, argus-tools' JSON) and what it uses from Yggdrasil and
  Limbo: [docs/CONTRACTS.md](docs/CONTRACTS.md); the API in full: [docs/SARINA_API.md](docs/SARINA_API.md). A change to
  either is made in both files in the same commit.
- Yggdrasil's and Limbo's own contracts: their repositories' docs/CONTRACTS.md.

## Open work

docs/plans/BACKLOG.md and docs/plans/LOG.md (private; ask the owner).

## Know-how

```text
application/            Qt/QML GUI (main.qml, components/, styles/, imports/)
backend/flipperzero/    protobufsession (RPC over serial), rpc/ (one class per op), utility/, toplevel/ (update, repair, ...)
backend/chymaera/       our additions: event log, datastore, sync, exporters, the Sarina server (chymaera.pri)
cli/                    qFlipper-cli, including `chymaera serve|selftest|parse|import|watch|pull|pull-all`
plugins/flipperproto0/  protobuf plumbing (nanopb)
argus-tools/            Rust: argus-tools itself (apps, snapshots, capture library, Limbo) and the port in crates/
docs/                   ARCHITECTURE.md, SARINA_API.md, RUST_PORT.md, LINUX_BUILD.md, CONTRACTS.md
```

- **C++ conventions:** match upstream (`.cpp`/`.h` pairs, `camelCase`, `m_` members, `Q_OBJECT`, signals and slots); a new
  device operation is one class under `backend/flipperzero/rpc/` exposed through `utilityinterface` / `protobufsession`;
  register new files in the `*.pro`/`.pri`. Prefer adding files to rewriting upstream ones, so `upstream` stays mergeable.
  Build and run `qFlipper-cli chymaera selftest` after C++ changes, or say in the commit that it was not built.
- **The port is proven without a device** (docs/RUST_PORT.md section 4): the schema and bytes against the C++ app's own
  nanopb encoder (`argus-tools/crates/flipper-rpc/tests/nanopb/gen.sh` regenerates the fixture), the fake Flipper, Qt
  oracles for the capture metadata and DfuSe parsers, property tests and fuzz targets (`argus-tools/fuzz`, nightly).
- **A change found by the port in the C++ is fixed in both** ("both apps fixed"), and the comparison scripts in CI are the
  proof.
- **argus-app** (egui/eframe) draws qFlipper's own design in the Yggdrasil accent (dark only); tested headless
  (accessibility-tree clicks, software pixel snapshots in `crates/argus-app`), never a window or a real device;
  `crates/argus-app/smoke-yggdrasil.sh` runs the real binary in Yggdrasil's headless compositor.
- **Versions:** `VERSION` (MAJOR.MINOR.PATCH) is the app's own; releases are tagged `vX.Y.Z`.
- **Firmware compatibility:** official Flipper apps work on Argus firmware while it keeps upstream's API version (88.2).
