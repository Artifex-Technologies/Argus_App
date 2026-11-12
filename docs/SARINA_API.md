# Sarina control API

The local, scriptable interface for driving Argus_App and querying its
data (Roadmap Phase 4). **This is the cross-repo contract** — sibling Artifex
Ecosystem projects that need to talk to the Flipper operator console do it through here.
Changes to this document must be mirrored in `../CLAUDE.md` → Cross-repo
contracts.

## Transport

- **TCP, loopback only.** The server binds `127.0.0.1` and drops any non-loopback
  peer. Default port **44700**.
- **Newline-delimited JSON.** One request object per line (`\n`), one response
  object per line.
- **Off by default.** It must be started explicitly (from the Chymaera Console
  UI, or `Chymaera.startServer(port, token)`), or headless with
  `qFlipper-cli chymaera serve [port] [token]`. The Rust port, `argus-cli chymaera serve [port] [token]`,
  is the same server: it answers byte for byte as the C++ one (`argus-tools/crates/argus-api`, proven
  against it in CI; docs/RUST_PORT.md).
- **Token always required** (since 2026-10-27, `f7d5e44`). Every request must include a
  matching `"token"` field; otherwise the response is `{"ok":false,"error":"unauthorized"}`.
  Started with a token, the server uses it. Started without one (as the console button
  does), it makes a random 256-bit token and writes it to `$XDG_RUNTIME_DIR/argus/token`
  (folder 0700, file 0600; removed on stop). If it cannot write that file, it does not
  start. Tokens are compared in constant time. Sarina's console clients read the token
  file by default. Since 2026-11-12 the port is taken before the token file is written, so a second
  server that cannot listen (the port is in use) no longer replaces the running server's token, and
  `serve` stopped with Ctrl+C or SIGTERM removes its token file.

### Request / response shape

```jsonc
// request
{ "cmd": "status", "id": 7, "token": "<the token>" }
// response
{ "ok": true, "id": 7, /* command-specific fields */ }
```

`id`, if provided, is echoed back so asynchronous clients can correlate
responses. Every response has a boolean `ok`; failures carry `"error"`.

## Commands

| cmd | params | returns |
|-----|--------|---------|
| `ping` | — | `pong`, `version` |
| `help` | — | `commands` (array) |
| `status` | — | `serverRunning`, `serverPort`, `sessionActive`, `sessionName`, `datastorePath`, `eventCount`, `version` |
| `log.tail` | `limit` (default 50) | `entries` (array, chronological) |
| `log.add` | `message` (req), `severity` | — |
| `session.start` | `name` (req), `notes` | `sessionId` |
| `session.end` | — | — |
| `capture.list` | `limit` (default 50) | `captures` (array) |
| `capture.import` | `path` (req: a local `.nfc`, `.sub`, `.rfid`, `.ir`, `.ibtn`, `.pcap` or `.pcapng` file) | `summary` |
| `device.status` | — | `connected`, `name` |
| `capture.pull` | `path` (req: a capture folder or below one, e.g. `/ext/nfc`) | `started`, `path` |
| `capture.pullAll` | — | `started`, `paths` (the folders it will read) |
| `capture.library` | `query` (optional), `limit` (default 50, max 500) | `total`, `captures` (array, see below) |
| `capture.explain` | `id` (req: 8+ hex digits of the capture's SHA-256) | `capture`, `explanation` |

### `capture.import` takes only capture files (2026-11-12)

`capture.import` records a file on this PC. Over the API it takes only the Flipper's capture formats and
packet captures (`.nfc`, `.sub`, `.rfid`, `.ir`, `.ibtn`, `.pcap`, `.pcapng`); any other name fails with
`only capture files may be imported: .nfc, .sub, …`. Before, any file this account could read was parsed,
and the generic parser stores every `Key: Value` line of a file in `meta`, so a caller holding only the
token could read, say, a config file with a password in it through `capture.list`. Only a regular file is
read (a FIFO used to hang the app, `/dev/zero` never ended, a folder was recorded as a capture); those fail
with `could not parse capture`. A file over 64 MiB is recorded unparsed. Importing in the Argus window or
with `chymaera import` has no suffix rule (I choose those files myself), but the other rules hold there too.

### `device.status` and `capture.pull` (added 2026-09-12)

`device.status` reports whether a Flipper is attached to the app and its name.

`capture.pull` downloads a device directory (e.g. `/ext/nfc`, `/ext/subghz`) into the
datastore. Since 2026-10-27 (`5655783`) the path must be one of the capture folders listed
under `capture.pullAll` below, or inside one (normalised, no `..`); any other path fails
with `only the capture folders may be pulled: …`. The same rule holds for the GUI and the
CLI. File names from the device cannot lead outside the staging folder (`e16698d`): a name
with a `.` or `..` part, a backslash or NUL is refused and reported as failed. It parses any `.nfc`/`.sub` captures it finds. It is **read-only** —
it only reads files off the device. It is **asynchronous**: the response confirms the pull
*started*; watch `log.tail` (source `device`) for progress and completion. Requires a
device attached to the app (the GUI, or `qFlipper-cli chymaera pull`/`watch`).

### `capture.pullAll` and capture integrity (v1.2, added 2026-10-24)

`capture.pullAll` pulls every known capture folder in one go: `/ext/nfc`, `/ext/subghz`,
`/ext/lfrfid`, `/ext/infrared`, `/ext/ibutton`, `/ext/apps_data/marauder/pcaps`,
`/ext/apps_data/marauder/logs`, and the Argus apps' `/ext/rf_monitor`, `/ext/wifi_scan`,
`/ext/ld2450`. The response lists them in `paths`. Like `capture.pull` it is
**read-only** and **asynchronous**: watch `log.tail` for a final `Pulled …: N new, M
already stored, K failed` entry. Only one pull runs at a time; a second request fails until it ends.

- **Allowlist, not the whole card.** Secrets such as `/ext/u2f` and `apps_data/totp` are
  never read. Sub-trees named `assets` (key dictionaries, Keeloq manufacturer keys) and
  `.cache` are skipped inside every folder, by `capture.pull` too.
- **No duplicates.** Every file gets a SHA-256 on arrival. A file whose digest was already
  recorded from the same device path is not stored again, so pulling repeatedly is safe.
- **Nothing is dropped.** Files the Flipper-format parsers don't understand (Marauder
  `.pcap`, CSV logs, `.log`) are recorded unparsed with type `pcap`, `csv`, `log` or `file`.
- Folders missing on the device are skipped silently. Folders that exist but are empty are fine.

### `capture.library` and `capture.explain` (v1.3, added 2026-11-09)

Read-only access to Argus's **capture library**: the Sub-GHz, infrared, NFC, 125 kHz RFID and
iButton files Argus has seen on the card (its "Read from Flipper"), in the console's pulls and in
every SD card snapshot, indexed by content (one entry per SHA-256). The library is built in the
Argus tools window (Captures tab) or with `argus-tools capture index`; until then both commands
fail with `the capture library has not been built yet (capture index)`.

- **Captures are private and stay on this PC.** Both commands answer over the loopback API only,
  and carry **header facts, never payloads**: no Sub-GHz key codes, RAW timings, IR signal data,
  card blocks/pages or RFID/iButton data. Locations name the card path and the snapshot id (or
  `card`), never a local file path.
- `query` is every whitespace-separated term matching: a plain word matches names, paths,
  summaries, protocols, signal names and tags; `kind:nfc`, `proto:princeton`, `freq:433.92`
  (MHz, prefix), `uid:04a1b2` (spaces ignored) and `tag:garage` narrow by field. Empty lists all.
- `capture.explain` adds a few plain sentences built from the same facts (for example whether a
  Sub-GHz protocol is fixed- or rolling-code). Tags are set by the person in the Argus window,
  not through the API.

```jsonc
// capture.library → captures[i], and capture.explain → capture
{
  "id": "4e58f7dfa03d0d94",          // first 16 hex digits of sha256; capture.explain takes 8+
  "sha256": "4e58f7df…",
  "kind": "subghz",                  // subghz | subghz_raw | infrared | nfc | rfid | ibutton
  "name": "garage",
  "summary": "Sub-GHz Princeton 24-bit key at 433.92 MHz",
  "fields": {                        // by kind; all optional
    "frequency_hz": 433920000, "frequency_mhz": "433.92", "preset": "AM650", "protocol": "Princeton",
    "bits": 24, "te_us": 400, "code_type": "fixed",          // subghz (code_type: fixed | rolling)
    // subghz_raw: raw_samples, raw_duration_ms
    // infrared: signal_count, protocols {"NEC": 2, "raw": 1}, signals [{name, type, protocol?, frequency?}]
    // nfc: device_type, uid, uid_bytes, atqa, sak, mifare_classic_type, blocks, blocks_unread, pages
    // rfid: key_type, data_bytes   ibutton: protocol
  },
  "tags": ["garage"],
  "locations": [ { "origin": "snapshot", "source": "snap-20261109-171500", "path": "/ext/subghz/garage.sub" },
                 { "origin": "pull", "source": "card", "path": "/ext/subghz/garage.sub" } ]
}
```

### Event entry shape (`log.tail` / persisted events)

```jsonc
{
  "timestamp": "2026-09-11T14:03:22.145",  // ISO-8601 local time
  "epochMs": 1789045402145,
  "severity": "info",   // debug|info|notice|warning|error|critical
  "source":   "device", // system|device|sarina|capture|export|user
  "message":  "…",
  "detail":   "…"        // optional
}
```

### Capture record shape (`capture.list`)

```jsonc
{
  "id": 12, "session_id": 3, "ts": 1789045402145,
  "type": "nfc",              // nfc|subghz|generic|pcap|csv|log|file
  "name": "office_badge.nfc",
  "source_path": "/ext/nfc/office_badge.nfc",  // device path (or local path for capture.import)
  "size": 512,
  "meta": "{…}",              // JSON string: the parsed file summary ({"kind":…} only when unparsed)
  "sha256": "9ff752d4…",      // v1.2: hex digest of the file as received; null on older rows
  "local_path": "/home/…/Argus/chymaera/pulls/20261024-210030/ext/nfc/office_badge.nfc"
                              // v1.2: the kept copy on this machine; null on older rows
}
```

`session_id` is null for a capture recorded outside a session. Until 2026-11-12 the C++ app sent an empty
string (`""`) wherever this document says null (`session_id`, `sha256`, `local_path`); it now sends null,
as documented.

The datastore (and the `pulls/` folder of received files) lives at
`~/.local/share/Argus/chymaera/` on Linux, shared by the GUI and `qFlipper-cli` since
v1.2. Before that each executable had its own copy under `Flipper Devices Inc/`.

## Quick test

```sh
# with the server started on 44700:
printf '{"cmd":"ping"}\n{"cmd":"status"}\n' | nc 127.0.0.1 44700
```

## Notes for sibling projects

- Treat this as **read/observe + light control**. Destructive device operations
  are intentionally not exposed here yet. Argus's app install, snapshot restore and rebuild
  are deliberately **not** in the API: they write to the card or start Limbo guests, and stay
  behind the person's confirmation in the Argus window.
- The datastore behind it is SQLite at `status.datastorePath`; prefer going
  through this API rather than opening the DB file directly, so schema changes
  stay behind the contract.
- New commands are additive. If you need one, propose it here and in
  `../CLAUDE.md` before building against it.
