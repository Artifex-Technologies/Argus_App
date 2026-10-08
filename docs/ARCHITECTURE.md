# Architecture

How the app is put together today (inherited from qFlipper) and where the Chymaera
extensions attach. Pair this with `../CLAUDE.md` (project overview + coordination) and
`ROADMAP.md` (what we are building).

## Layers

```
┌─────────────────────────────────────────────────────────────┐
│  application/   QML GUI  +  cli/  headless interface          │
├─────────────────────────────────────────────────────────────┤
│  backend/       Qt C++ logic, exposed to QML as models        │
│    flipperzero/ device model, operations, streaming, state    │
├─────────────────────────────────────────────────────────────┤
│  protobufsession  ← serial/RPC transport to the device        │
│  plugins/flipperproto0 + 3rdparty/nanopb  ← protobuf codec     │
├─────────────────────────────────────────────────────────────┤
│  Flipper Zero  (VID 0x0483 / PID 0x5740 serial; 0xdf11 DFU)   │
└─────────────────────────────────────────────────────────────┘
```

## The comms path (most important thing to understand)

1. `backend/serialfinder.*` + `helper/serialinithelper.*` locate and open the Flipper's
   virtual serial port.
2. `flipperzero/protobufsession.*` runs an RPC session: it frames protobuf messages
   (encoded/decoded via `plugins/flipperproto0`, backed by `3rdparty/nanopb`) over the
   serial link, matching responses to requests.
3. Each **RPC operation** is one class in `flipperzero/rpc/`. Examples:
   - `storage*` — list/stat/read/write/mkdir/remove/rename/md5 on the device filesystem.
   - `gui*` — screen streaming, sending input events, virtual display.
   - `system*` — device info, datetime, reboot, factory reset, update.
4. **Utility operations** (`flipperzero/utility/`) compose RPC ops into higher-level
   flows (e.g. `directorydownloadoperation`, `filesuploadoperation`, `userbackupoperation`).
5. **Top-level operations** (`flipperzero/toplevel/`) orchestrate full user-facing flows
   (full update, full repair, factory reset, settings backup/restore).
6. `flipperzero/devicestate.*` and `flipperzero/flipperzero.*` expose a per-device model
   that the QML layer binds to. `deviceregistry.*` tracks connected devices.

Operations share a common lifecycle via `backend/abstractoperation*.{h,cpp}` and are run
through `abstractoperationrunner`. **Follow this pattern for any new device interaction.**

## Adding a new RPC operation (the standard recipe)

1. Copy the closest existing op in `flipperzero/rpc/` (e.g. a `storage*` op) into a new
   `.cpp`/`.h` pair; rename the class.
2. Fill in the protobuf request/response handling for the new command.
3. Register it in `backend/backend.pro` (SOURCES/HEADERS).
4. Expose a convenience method on `utilityinterface`/`protobufsession` if the GUI/CLI
   should call it.
5. Wire a QML control (`application/`) and/or a CLI verb (`cli/cli.cpp`).

> The set of RPC commands the device understands is defined by the Flipper firmware's
> protobuf schema. New commands that the firmware does not implement require firmware
> support first; on-desktop features (parsing, storage, export, analysis) do not.

## Streaming

`flipperzero/screenstreamer.*` already streams the device framebuffer. It is the
reference implementation for a continuous data channel and the model to imitate for the
Chymaera live-data streams (logs, capture events) described in `ROADMAP.md` Phase 1.

## Where Chymaera code lives

The Chymaera subsystem is a self-contained subtree under `backend/`, kept separate from
upstream files so `upstream` merges stay clean. It is pure QtCore/Sql/Network (no GUI
deps), built via `backend/chymaera/chymaera.pri` (included from `backend.pro`), and
exposed to QML as the `Chymaera` singleton (registered in `application/application.cpp`).

```
backend/chymaera/
  chymaeratypes.{h,cpp}        Severity/Source enums + name helpers
  chymaerabridge.{h,cpp}       Bridge — the façade owned by the app / QML singleton
  datastore/                   ChymaeraDatastore — SQLite: sessions, events, captures
  eventlog/                    ChymaeraEventLog — live console model + central log sink
  exporters/
    pcapexporter.{h,cpp}       .pcap / .pcapng writer (Wireshark bridge)
    flipperfileformat.{h,cpp}  Flipper File Format parser (.nfc, .sub, generic)
  sarina/                      SarinaServer — loopback JSON control API
```

UI: `application/components/ChymaeraConsole.qml` (the console panel), opened from
`DeveloperActions.qml` (developer mode). The control API surface is documented in
`SARINA_API.md`.

Integration points touched (all additive):
- `backend/backend.pro` — one `include(chymaera/chymaera.pri)` line.
- `application/application.{h,cpp}` — a `Chymaera::Bridge` member + singleton registration.
- `application/application.pro`, `cli/cli.pro` — `QT += sql` (backend now uses QtSql).
- `application/qml.qrc` — the console component.

Build note: this environment cannot compile Qt, so the subsystem has been written to
match existing patterns but **needs a build pass on a Qt machine** to confirm. QtSql needs
the SQLite driver plugin (ships with standard Qt; static builds must link it).
