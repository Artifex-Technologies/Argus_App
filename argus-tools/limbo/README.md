# Rebuilding an app in Limbo: design and root setup

Argus can rebuild a Flipper app for the connected device's firmware API with `ufbt`. App source
from the internet is untrusted: `application.fam` is Python that ufbt runs, and the build runs
the app's own build steps. So Argus never builds on this PC. It asks Limbo for two disposable
validation runs over Limbo's agent socket, the interface Sarina already uses (Limbo
`docs/QUARANTINE.md`, "Validation runs" and "Runs that carry files").

## How a rebuild runs

| Step | Where | Network | What happens |
|---|---|---|---|
| 1. Fetch (git URL only) | guest from profile `argus-fap-fetch` | `internet-only` | `git clone --depth 1 --no-recurse-submodules` of the URL (https only, a strict character set, single-quoted). Nothing from the repository is executed. The app folder is packed as a .tgz (screenshots, docs, `.git` and binaries left out) and printed as base64 with its SHA-256. |
| 2. Hand-over | this PC | none | Argus checks the SHA-256 and size and **does not unpack** the .tgz. A local source folder is packed here instead (regular files only; symlinks left out). |
| 3. Build | guest from profile `argus-fap-build` | **none** | The .tgz goes in as the run's bundle (Limbo's 40 KiB cap). The guest runs `ufbt build` against the SDK baked into its image for the device's API (`/opt/argus-ufbt/f7-88.2`) and prints each `dist/*.fap` as base64 with its SHA-256; the log goes to stderr. |
| 4. Hand-back | this PC | none | Argus decodes each .fap, checks the SHA-256, stores it as `<sha256>.fap`, reads its manifest and checks it against the device's API with the loader's rule. Only then is it offered for install, which goes through the normal install dialog (never overwrites without asking). |

Both guests are torn down when their run ends, whatever happens. A run's output is capped by
its profile and Limbo strips control characters from it.

**Why the hand-back is the run's output and not Limbo's exchange.** Limbo's exchange
(`limbo exchange take`, ClamAV-scanned outbox) belongs to *registered instances*: the engine
only lets a guest write to `/var/lib/limbo/exchange/<guest name>`, and a validation run's guest
has a random name and no shared folder (`internal/runs`, `internal/engine` in Limbo). A run's
stdout is the channel Limbo offers for runs, so the .fap travels there, and Argus does the checks
the exchange would not do anyway (hash, ELF/manifest parse, API check). If Limbo later gives
runs an outbox, `argus-tools/src/limbo.rs` (`parse_build_output`) is the one place to change.

**Limits that come from Limbo, not Argus:**
- The bundle is at most 40 KiB (`maxBundle` in Limbo's `internal/runs/runs.go`). A compressed app
  source above that is refused with `bundle_too_big`. Most single-app sources fit; apps with
  many images may not. Raising it is a Limbo change (a per-profile `max_bundle_bytes`, say).
- If Heimdall has contained Sarina, Limbo refuses all agent-socket runs, Argus's included.
- Two agent runs at a time (Limbo's `maxConcurrent`).

## Root steps (once)

Nothing here was done by Argus's agent; these need root.

1. **Build the guest image** (needs `libguestfs` for `virt-customize`/`virt-resize`, and network
   for this step only):
   ```sh
   sudo sh argus-tools/limbo/build-image.sh
   # or with the SDK built from Argus_Firmware:
   sudo sh argus-tools/limbo/build-image.sh /var/lib/limbo/images/arch-cloudimg.qcow2 /path/to/flipper-z-f7-sdk-local.zip 88.2
   ```
   It copies the Arch cloud image (never changes it), installs git, python and ufbt, runs
   `ufbt update` into `/opt/argus-ufbt/f7-88.2`, prints `ufbt status` (check that it says API
   88.2) and the image's SHA-256. Default SDK: Flipper's official 1.5.1 SDK, API 88.2, the API
   Argus firmware keeps. For a new firmware API, run it again with the new SDK and API into a new
   image (or extend the script to add a second `/opt/argus-ufbt/f7-<api>` folder).

2. **An ssh key for the two profiles:**
   ```sh
   sudo ssh-keygen -t ed25519 -N '' -C argus-build@limbo -f /etc/limbo/keys/argus-build
   sudo chmod 600 /etc/limbo/keys/argus-build
   sudo cat /etc/limbo/keys/argus-build.pub
   ```

3. **The two profiles:**
   ```sh
   sudo install -m 0644 argus-tools/limbo/argus-fap-build.json /etc/limbo/validation/argus-fap-build.json
   sudo install -m 0644 argus-tools/limbo/argus-fap-fetch.json /etc/limbo/validation/argus-fap-fetch.json
   ```
   In both, replace `image.sha256` with the value step 1 printed and `provision.ssh_public_key`
   with the line from step 2. Do not change `network` (`none` for the build is the point), and
   leave `sudo` off: the build guest opens untrusted files.

4. **Let Argus reach Limbo's agent socket** (group `limbo-agent`, as Sarina):
   ```sh
   sudo usermod -aG limbo-agent "$USER"   # then log out and in
   ```
   This lets the account start validation runs on any profile in `/etc/limbo/validation`, the
   same right Sarina has. It cannot touch quarantines, guests it did not start, or the exchange.

5. **Check** (no restart of Limbo is needed; profiles are read on every request):
   ```sh
   argus-tools limbo profiles     # both profiles listed, nothing under "skipped"
   ```
   Then in Argus: Apps > Rebuild, pick a source folder (for example a small app from
   `flipperdevices/flipperzero-good-faps`) and build. Or headless:
   `argus-tools limbo build --git https://github.com/flipperdevices/flipperzero-good-faps --subdir snake_game --out /tmp/argus-build --api 88.2/7`.

## What was tested and what was not

Tested (`cargo test --test limbo`): the HTTP client against a fake agent socket that speaks
Limbo's API (202 + id, polling, chunked bodies, errors), the bundle layout, the fetch and build
command texts, URL/ref/subdir validation (shell metacharacters, `..`, non-https), hash checks
on the hand-back, a guest that sends a hostile file name, base64 garbage, an incompatible build
and a missing SDK. Not tested: a real Limbo with the real image (needs the root steps above),
and therefore that ufbt's `update --url/--file` flags and the image's SDK path work as written.
