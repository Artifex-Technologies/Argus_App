# Contracts

What other projects may rely on from Argus_App, and what it uses from them. One section per contract. Commands and fields
are additive: propose a change here and in the detailed spec before building on it.

## Provided by Argus_App

### The Sarina control API (v1.3)

Loopback only (`127.0.0.1`, default port **44700**), newline-delimited JSON over TCP, a token on every request (made and
written to `$XDG_RUNTIME_DIR/argus/token` when none is given). Off by default; the console UI or `chymaera serve` starts it.
`argus-cli chymaera serve` (Rust) is the same server as `qFlipper-cli chymaera serve` (C++), byte for byte (CI compares them).
Everything in it reads or observes, with light control: nothing transmits, flashes or changes the device's state.
History: v1.1 (2026-09-12) `device.status`, `capture.pull`; v1.2 (2026-10-24) `capture.pullAll` (allowlisted folders, secrets
never read) and the capture fields `sha256`, `local_path`; since 2026-10-27 `capture.pull` takes only capture folders and the
token is always required; v1.3 (2026-11-09) `capture.library`, `capture.explain` (header facts, tags, where seen; never payloads
or local paths); since 2026-11-12 `capture.import` takes only capture files (`.nfc .sub .rfid .ir .ibtn .pcap .pcapng`, regular
files) and `capture.list` sends null for NULL fields.
Spec: [SARINA_API.md](SARINA_API.md). Used by: Sarina (its Argus client and its headless server).

### The datastore (captures, events, sessions)

A SQLite database at `status.datastorePath` (`~/.local/share/Argus/chymaera/` on Linux), shared by the GUI and the CLIs. Other
projects read it **through the API** (`log.tail`, `capture.list`), never by opening the file, so its schema can change. The
event and capture record shapes are in [SARINA_API.md](SARINA_API.md). Used by: Sarina (through the API).

### argus-tools (JSON on stdout)

`argus-tools/` (Rust) behind Argus's apps, snapshots, capture library and Limbo rebuilds: one JSON object per command; data in
`~/.local/share/argus/` (`snapshots/`, `captures/`, `apps/`). Not a contract for other repositories yet: use the Sarina API.

## Used by Argus_App (provided elsewhere)

| Contract | Provider | Used here by |
| --- | --- | --- |
| `~/.config/yggdrasil/appearance.conf` (the accent colour) | Yggdrasil (docs/CONTRACTS.md there) | `argus-app`'s theme (`crates/argus-app/src/theme.rs`) |
| `yggdrasil-compositor --headless` and its control socket | Yggdrasil | `crates/argus-app/smoke-yggdrasil.sh` (the app on the fake, in CI) |
| Limbo's agent and run bundles (40 KiB cap) | Limbo | the Limbo rebuild of apps (`argus-tools/limbo/README.md`) |
