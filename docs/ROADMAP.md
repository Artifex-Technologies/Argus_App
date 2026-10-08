# Roadmap — Chymaera operator console

Phased plan to grow the qFlipper fork into the operator console described in
`../CLAUDE.md`. Phases are ordered so each is useful on its own and builds on the last.
Check off items as they land; keep the **Coordination Log** in `../CLAUDE.md` updated.

Guiding constraints:
- Add modules; don't rewrite upstream files (keep `upstream` mergeable).
- Everything on-desktop first (parse/store/export/analyze). Device-side features that
  need firmware support are called out explicitly.
- General-purpose capture/analyze/report tooling for authorized engagements.

---

## Phase 0 — Foundation & coordination  ✅ (this commit)
- [x] `CLAUDE.md` coordination board + agent guide.
- [x] `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`.
- [x] README reflects the fork; keeps qFlipper/GPLv3 attribution.
- [x] Gitignore local editor state.

## Phase 1 — Richer control + live data streams  🚧 (foundation landed)
Goal: go beyond update/repair to real-time operation.
- [x] **Live event/console stream**: `ChymaeraEventLog` (a capped, persisted
      `QAbstractListModel`) + the `ChymaeraConsole.qml` panel, opened from the
      developer-mode actions. Central sink every subsystem writes to.
- [x] **Device lifecycle stream**: `Bridge::attachDevice()` mirrors a connected Flipper's
      status / online-offline / error / RPC-session changes into the event log (GUI wires
      it on `currentDeviceChanged`; console shows a device indicator). Headless equivalent:
      `qFlipper-cli chymaera watch`.
- [ ] Enumerate the RPC commands the current firmware exposes; document in `docs/RPC.md`.
- [ ] **Raw firmware log tap**: feed the Flipper's own log/CLI output into the stream (needs
      an RPC log channel / firmware support; the lifecycle stream above is the interim win).
- [ ] Robust reconnect/keepalive on the serial session for long-running sessions.

## Phase 2 — Offload & datastore ("extra power / database")  🚧
Goal: use the desktop as external storage/compute so the Flipper isn't bottlenecked.
- [x] Embedded store chosen: **SQLite via Qt Sql** (`ChymaeraDatastore`).
- [x] `backend/chymaera/datastore/`: schema (sessions / events / captures) + write path,
      with read-back helpers for the API/CLI.
- [x] Data-exchange format defined and recorded — see `docs/SARINA_API.md` and the
      Cross-repo contracts in `CLAUDE.md`.
- [x] **Pull device files into the datastore** on demand: `Bridge::pullPath("/ext/nfc")`
      downloads a device directory (via `utility()->downloadDirectory`) and ingests/parses
      captures. GUI buttons (Pull NFC / Pull Sub-GHz) + `qFlipper-cli chymaera pull <path>`
      + API `capture.pull`.
- [ ] Auto-pull on connect / background sync + a mirror view in the GUI (currently manual).

## Phase 3 — Analysis-tool bridges  🚧
Goal: get captured data into the tools operators already use.
- [x] **Wireshark**: `PcapExporter` writes `.pcap` and `.pcapng` (byte-exact, DLT
      selectable; defaults to LINKTYPE_USER0 for opaque payloads).
- [x] **Flipper capture parsing**: `FlipperFileFormat` + typed parsers for `.nfc`
      (device type / UID / ATQA / SAK) and `.sub` (frequency / preset / protocol / RAW
      timing), recorded as captures on import.
- [ ] Live-to-Wireshark via a named pipe / `wireshark -k -i`.
- [ ] **RF / SDR**: export Sub-GHz RAW timing to a documented IQ/raw format; hooks for an
      external analyzer (URH / GNU Radio / SDR#). Document in `docs/FORMATS.md`.
- [ ] **NFC / RFID workbench UI**: display/diff parsed dumps; export standard formats.

## Phase 4 — Sarina control API  ✅ (v1)
Goal: let the assistant drive the app and query captured data, scriptably.
- [x] `backend/chymaera/sarina/`: `SarinaServer` — loopback-only, newline-delimited JSON
      over TCP, with a pluggable request handler.
- [x] Commands: `ping`, `help`, `status`, `log.tail`, `log.add`, `session.start/end`,
      `capture.list`, `capture.import`.
- [x] Auth: loopback binding + non-loopback peer rejection + optional shared token.
- [x] Documented in `docs/SARINA_API.md` and recorded in `CLAUDE.md` Cross-repo contracts.
- [x] Headless CLI surface: `qFlipper-cli chymaera <serve|selftest|parse|import>` — runs
      without a GUI/display (WSL/SSH/CI friendly). `selftest` verifies the whole subsystem.
- [ ] Extend with device-control commands once Phase 1 device taps land.

## Phase 5 — Reporting
Goal: turn a session into a deliverable for the engagement.
- [ ] Session timeline (what was captured/done, when).
- [ ] Export findings + evidence to a report artifact (Markdown/PDF/HTML).

---

## Notes for engineering agents
- Start with Phase 1 log/console stream — it is self-contained, uses existing patterns
  (`screenstreamer`, `rpc/` ops), and immediately proves the "richer control" value.
- Any new C++ must register in the relevant `*.pro` file and follow the operation
  lifecycle in `backend/abstractoperation*`.
- Because this environment can't reliably compile Qt, land new C++ in small, pattern-
  faithful commits and flag them in the Coordination Log for build verification.
