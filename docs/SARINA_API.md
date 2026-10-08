# Sarina control API

The local, scriptable interface for driving Argus_App and querying its
data (Roadmap Phase 4). **This is the cross-repo contract** — sibling Chymaera
projects that need to talk to the Flipper operator console do it through here.
Changes to this document must be mirrored in `../CLAUDE.md` → Cross-repo
contracts.

## Transport

- **TCP, loopback only.** The server binds `127.0.0.1` and drops any non-loopback
  peer. Default port **44700**.
- **Newline-delimited JSON.** One request object per line (`\n`), one response
  object per line.
- **Off by default.** It must be started explicitly (from the Chymaera Console
  UI, or `Chymaera.startServer(port, token)`).
- **Optional token.** If a token is set, every request must include a matching
  `"token"` field; otherwise the response is `{"ok":false,"error":"unauthorized"}`.

### Request / response shape

```jsonc
// request
{ "cmd": "status", "id": 7, "token": "optional" }
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
| `capture.import` | `path` (req) | `summary` |
| `device.status` | — | `connected`, `name` |
| `capture.pull` | `path` (req, e.g. `/ext/nfc`) | `started`, `path` |

### `device.status` and `capture.pull` (added 2026-09-12)

`device.status` reports whether a Flipper is attached to the app and its name.

`capture.pull` downloads a device directory (e.g. `/ext/nfc`, `/ext/subghz`) into the
datastore, parsing any `.nfc`/`.sub` captures it finds. It is **read-only** — it only
reads files off the device. It is **asynchronous**: the response confirms the pull
*started*; watch `log.tail` (source `device`) for progress and completion. Requires a
device attached to the app (the GUI, or `qFlipper-cli chymaera pull`/`watch`).

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
  "type": "nfc",              // nfc|subghz|generic
  "name": "office_badge.nfc",
  "source_path": "/…/office_badge.nfc",
  "size": 512,
  "meta": "{…}"               // JSON string: the parsed file summary
}
```

## Quick test

```sh
# with the server started on 44700:
printf '{"cmd":"ping"}\n{"cmd":"status"}\n' | nc 127.0.0.1 44700
```

## Notes for sibling projects

- Treat this as **read/observe + light control**. Destructive device operations
  are intentionally not exposed here yet.
- The datastore behind it is SQLite at `status.datastorePath`; prefer going
  through this API rather than opening the DB file directly, so schema changes
  stay behind the contract.
- New commands are additive. If you need one, propose it here and in
  `../CLAUDE.md` before building against it.
