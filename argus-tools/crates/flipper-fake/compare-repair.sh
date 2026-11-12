#!/bin/sh
# Runs the C++ full repair (qFlipper-cli with no command, on a Flipper in recovery mode: FullRepairOperation)
# and the Rust one (argus-cli recovery repair --resources) against the same fake Flipper across its lives (flipper-fake
# --recovery: the bootloader on a Unix socket, then the firmware on a pseudo-terminal), and compares them: exit
# status, the bootloader requests in order, the bytes on the fake chip (all of its flash), the option bytes
# (RDP untouched), the FUS and resets, then the requests on the firmware (resources onto the card, restart) and
# the files on the card afterwards, and what each says. docs/RUST_PORT.md, section 4.
#
#   sh compare-repair.sh QFLIPPER_CLI ARGUS_CLI FLIPPER_FAKE OUTDIR
#
# QFLIPPER_CLI must be built with the test hooks (backend/testhooks.h): the script refuses a binary without
# them, because without them the C++ CLI would watch USB and could find a real Flipper. With them it sees only
# what the fake's state file says, reaches the bootloader only over the fake's socket (never libusb), the
# firmware only on the fake's /dev/pts/N, and reads the update files from a made-up local folder (never the
# network). It runs with its own HOME and XDG folders under OUTDIR, which must not exist. Nothing is flashed.
# argus-cli fetches region bundles only from flipper-fake's HTTPS stand-in on 127.0.0.1, with a CA made for the run.
set -u
[ $# -eq 4 ] || { echo "usage: sh compare-repair.sh QFLIPPER_CLI ARGUS_CLI FLIPPER_FAKE OUTDIR" >&2; exit 2; }
cpp=$1 rs=$2 fake=$3 out=$4
for b in "$cpp" "$rs" "$fake"; do [ -x "$b" ] || { echo "not executable: $b" >&2; exit 2; }; done
cpp=$(realpath "$cpp") rs=$(realpath "$rs") fake=$(realpath "$fake")
for hook in ARGUS_TEST_DEVICE_STATE ARGUS_TEST_DFU_SOCKET ARGUS_TEST_FIRMWARE_DIR ARGUS_TEST_SERIAL_PORT; do
    grep -aq "$hook" "$cpp" || { echo "refusing: $cpp has no $hook hook (it could open a real Flipper or the network)" >&2; exit 2; }
done
[ -e "$out" ] && { echo "refusing: $out exists" >&2; exit 2; }
mkdir -p "$out" && chmod 700 "$out"
out=$(cd "$out" && pwd)
fw=$out/firmware
"$fake" --write-firmware "$fw" || { echo "could not write the firmware folder" >&2; exit 1; }
"$fake" --write-firmware "$out/firmware-mismatch" --resources-mismatch || { echo "could not write the firmware folder" >&2; exit 1; }
# the same folder with a region bundle in which the server names no country, and one with a band the firmware
# cannot hold (a power limit of 200 dBm)
cp -r "$fw" "$out/firmware-nocountry" && sed -i 's/"country":"DE"/"country":null/' "$out/firmware-nocountry/bundle"
cp -r "$fw" "$out/firmware-badband" && sed -i 's/"max_power":14/"max_power":200/' "$out/firmware-badband/bundle"

# The update server for argus-cli: flipper-fake's HTTPS stand-in on 127.0.0.1 (a test CA made at its start; argus-cli
# trusts it through SSL_CERT_FILE), one per folder. The C++ app reads the same folders through its test hook.
# start_server NAME DIR: sets $upd to its address
start_server() {
    mkfifo "$out/$1.stdin"
    "$fake" --update-server "$2" --ca-out "$out/$1.pem" < "$out/$1.stdin" > "$out/$1.out" 2> "$out/$1.err" &
    eval "$1_pid=\$!"
    eval "exec $3> \"\$out/\$1.stdin\""
    i=0
    while [ $i -lt 100 ] && ! grep -q '^listening ' "$out/$1.out" 2>/dev/null; do i=$((i + 1)); timeout 0.1 tail -f /dev/null; done
    upd=$(sed -n 's/^listening //p' "$out/$1.out")
    case "$upd" in https://127.0.0.1:*) ;; *) echo "update server did not start: $(cat "$out/$1.err")" >&2; exit 1 ;; esac
}
start_server srv_main "$fw" 7
upd_main=$upd
start_server srv_nocountry "$out/firmware-nocountry" 8
upd_nocountry=$upd
start_server srv_badband "$out/firmware-badband" 9
upd_badband=$upd
upd=$upd_main
ca=$out/srv_main.pem

LC_ALL_OUTER=${LC_ALL:-}
failures=0
check() { # check NAME RESULT (0 = ok)
    if [ "$2" -eq 0 ]; then echo "  ok    $1"; else echo "  FAIL  $1"; failures=$((failures + 1)); fi
}

# run SCENARIO SIDE [fake args...]: one repair by one side on a fresh fake; leaves $out/SCENARIO-SIDE/
# ($fw names the update folder both use)
run() {
    name=$1 side=$2; shift 2
    dir=$out/$name-$side
    mkdir -p "$dir/home" "$dir/tmp"
    mkfifo "$dir/stdin"
    # socket paths are short (108 bytes): everything runs inside the scenario's folder, the socket is dfu.sock
    (cd "$dir" && exec timeout 400 "$fake" --recovery . "$@" < stdin > fake.out 2> fake.err) &
    fake_pid=$!
    exec 3> "$dir/stdin"
    i=0
    while [ $i -lt 100 ]; do
        grep -q '^listening ' "$dir/fake.out" 2>/dev/null && [ -S "$dir/dfu.sock" ] && break
        i=$((i + 1)); timeout 0.1 tail -f /dev/null
    done
    port=$(sed -n 's/^pty //p' "$dir/fake.out")
    case "$port" in /dev/pts/*) ;; *) echo "fake did not start: $(cat "$dir/fake.err")" >&2; exit 1 ;; esac
    if [ "$side" = cpp ]; then
        (cd "$dir" && exec env HOME="$dir/home" XDG_CONFIG_HOME="$dir/home/.config" XDG_DATA_HOME="$dir/home/.local/share" \
            XDG_CACHE_HOME="$dir/home/.cache" XDG_RUNTIME_DIR="$dir/tmp" TMPDIR="$dir/tmp" QT_QPA_PLATFORM=offscreen \
            ARGUS_TEST_DEVICE_STATE="$dir/state" ARGUS_TEST_DFU_SOCKET=dfu.sock ARGUS_TEST_SERIAL_PORT="$port" \
            ARGUS_TEST_FIRMWARE_DIR="$fw" LC_ALL="${cpp_locale:-$LC_ALL_OUTER}" \
            timeout 300 "$cpp" -d 2 > stdout.txt 2> stderr.txt)
    elif [ "${legacy:-}" = radio ] || [ "${legacy:-}" = plain ]; then
        radio_arg=
        [ "$legacy" = radio ] && radio_arg=$fw/radio.bin
        (cd "$dir" && exec env SSL_CERT_FILE="$ca" timeout 300 "$rs" --update-server "$upd" --dfu-socket dfu.sock --port "$port" --poll-ms 200 recovery legacy-update \
            "$fw/firmware.dfu" "$fw/ob.data" $radio_arg --resources "$fw/flipper-z-any-resources-test.tgz" \
            --backup "$dir/backup.tgz" --yes > stdout.txt 2> stderr.txt)
    else
        (cd "$dir" && exec env SSL_CERT_FILE="$ca" timeout 300 "$rs" --update-server "$upd" --dfu-socket dfu.sock --port "$port" --poll-ms 200 recovery repair \
            "$fw/firmware.dfu" "$fw/ob.data" "$fw/radio.bin" --resources "$fw/flipper-z-any-resources-test.tgz" --yes \
            > stdout.txt 2> stderr.txt)
    fi
    echo $? > "$dir/exit.txt"
    exec 3>&-
    wait "$fake_pid"
}

# The bootloader requests of the work: without the waits the Rust code reports as lines (the C++ app sleeps;
# the waits are compared in tests/qt_device.rs), without the connection markers, and without the C++ app's own
# identification of the device each time it appears: it reads the factory information in the OTP to name it
# (argus-cli does not name the device), which leaves the chip in dfuUPLOAD-IDLE, so the next connection's first
# status is followed by an abort (DFU_ABORT and a status) that is the identification's too.
requests() {
    awk '
        /^SESSION / { if (n) flush(); n = 0; otp = 0; other = 0; next }
        /^SLEEP / { next }
        { lines[n++] = $0; if ($0 == "ALT 2") otp = 1; else if ($0 ~ /^ALT /) other = 1 }
        function flush(   i, skip) {
            if (otp && !other) { after_id = 1; return }
            # the first status (after a first ALT, if any), then the abort and its status
            first = (lines[0] ~ /^ALT /) ? 1 : 0
            skip = after_id && n >= first + 3 && lines[first] == "IN 3 0 6" && lines[first + 1] == "OUT 6 0 -" && lines[first + 2] == "IN 3 0 6"
            after_id = 0
            for (i = 0; i < n; i++) if (!skip || (i != first + 1 && i != first + 2)) print lines[i]
        }
        END { if (n) flush() }
    ' "$1/dfu.log"
}
# The RPC requests on the firmware, without session and command ids: without the C++ app's device probes (a
# session that starts with the protobuf version: versions, devinfo, card, manifest, and it sets the clock) and its
# screen (Gui*), and without the devinfo reads argus-cli makes for the region.
rpc_requests() {
    awk '
        $1 != s { s = $1; probe = ($3 == "SystemProtobufVersionRequest") }
        !probe { $1 = ""; $2 = ""; sub(/^  /, ""); print }
    ' "$1/requests.txt" | grep -v '^Gui\|^PropertyGetRequest'
}
field() { sed -n "s/^$2 //p" "$1/chip.txt"; }

# compare SCENARIO EXPECTED_EXIT(0|3|3u|fail): both sides already run. 3: repaired (3u: updated), but the radio
# stack was not installed (partial success; both CLIs exit 3 and say why)
compare() {
    name=$1 want=$2 c=$out/$1-cpp r=$out/$1-rs
    ce=$(cat "$c/exit.txt") re=$(cat "$r/exit.txt")
    verb=repaired
    [ "$want" = 3u ] && want=3 verb=updated
    if [ "$want" = 0 ] || [ "$want" = 3 ]; then
        check "$name: C++ exit $want ($ce)" "$(test "$ce" = "$want"; echo $?)"
        check "$name: Rust exit $want ($re)" "$(test "$re" = "$want"; echo $?)"
    else
        check "$name: C++ fails ($ce)" "$(test "$ce" != 0 && test "$ce" != 124; echo $?)"
        check "$name: Rust fails ($re)" "$(test "$re" != 0 && test "$re" != 124; echo $?)"
    fi
    requests "$c" > "$c/requests.dfu"
    requests "$r" > "$r/requests.dfu"
    diff "$c/requests.dfu" "$r/requests.dfu" > "$out/$name-requests.diff"
    check "$name: same bootloader requests in the same order ($(wc -l < "$c/requests.dfu") each)" $?
    cmp -s "$c/flash.bin" "$r/flash.bin"
    check "$name: same flash, byte for byte" $?
    for f in option_bytes otp fus fus_commands wireless_stack erased_pages resets booted_firmware; do
        check "$name: same $f ($(field "$r" "$f" | cut -c1-24))" "$(test "$(field "$c" "$f")" = "$(field "$r" "$f")"; echo $?)"
    done
    rpc_requests "$c" cpp > "$c/requests.rpc"
    rpc_requests "$r" rs > "$r/requests.rpc"
    diff "$c/requests.rpc" "$r/requests.rpc" > "$out/$name-rpc.diff"
    check "$name: same requests on the firmware afterwards ($(wc -l < "$c/requests.rpc") each)" $?
    cmp -s "$c/dump.txt" "$r/dump.txt"
    check "$name: same files on the card afterwards" $?
    check "$name: RDP untouched (0xAA, level 0) in both" \
        "$(field "$c" option_bytes | grep -q '^aa' && field "$r" option_bytes | grep -q '^aa'; echo $?)"
    if [ "$want" = 3 ]; then
        check "$name: both say $verb, but the radio stack was not installed" \
            "$(grep -q "PARTIAL SUCCESS: $verb, but the radio stack was not installed: " "$c/stderr.txt" "$c/stdout.txt" &&
               grep -q "\"code\":\"radio_not_installed\",\"message\":\"$verb, but the radio stack was not installed: " "$r/stdout.txt" &&
               grep -q '"partial":true' "$r/stdout.txt"; echo $?)"
    fi
}

echo "compare-repair: $out"

# A Flipper in normal use (the wireless stack running), as the C++ app finds one in recovery mode.
run running cpp --fus running
run running rs --fus running
compare running 0
check "running: C++ says the repair succeeded" "$(grep -q 'Full Repair @Fakeflip SUCCESS' "$out/running-cpp/stderr.txt" "$out/running-cpp/stdout.txt"; echo $?)"
check "running: Rust installed the wireless stack and wrote the option bytes" \
    "$(grep -q '"radio":{"bytes":12293,"error":null,"installed":true}' "$out/running-rs/stdout.txt" && grep -q '"option_bytes_written":true' "$out/running-rs/stdout.txt"; echo $?)"
check "running: Rust put the resources on the card and restarted the Flipper" \
    "$(grep -q '"files_written":4' "$out/running-rs/stdout.txt" && grep -q '"restarted":true' "$out/running-rs/stdout.txt"; echo $?)"
check "running: firmware and wireless stack where they belong" \
    "$(test "$(field "$out/running-rs" booted_firmware)" = true && test "$(field "$out/running-rs" fus_commands)" = 5253; echo $?)"

# An older install of the resources on the card, with a valid manifest: only the differences are sent.
run old-resources cpp --fus running --old-resources
run old-resources rs --fus running --old-resources
compare old-resources 0
check "old-resources: Rust read the card's manifest and sent only the differences" \
    "$(grep -q '"device_manifest":"read"' "$out/old-resources-rs/stdout.txt" && grep -q '"files_written":3' "$out/old-resources-rs/stdout.txt"; echo $?)"

# No SD card: nothing to put on it; both restart the Flipper.
run no-card cpp --fus running --no-sd
run no-card rs --fus running --no-sd
compare no-card 0

# The FUS already running: the C++ app restarts the chip "for consistency" first.
run fus-idle cpp --fus idle
run fus-idle rs --fus idle
compare fus-idle 0

# A FUS stuck in a state the repair does not know: the wireless stack step fails, both go on with the firmware.
run stuck cpp --fus stuck
run stuck rs --fus stuck
compare stuck 3
check "stuck: C++ went on without the wireless stack" "$(grep -q 'Attempting to install the firmware anyway' "$out/stuck-cpp/stderr.txt" "$out/stuck-cpp/stdout.txt"; echo $?)"

# The FUS never finishes deleting the wireless stack: both give up after 120 status polls (they used to poll for
# ever) and go on with the firmware.
run never-settles cpp --fus never-settles
run never-settles rs --fus never-settles
compare never-settles 3
check "never-settles: both gave up after 120 FUS state reads" \
    "$(test "$(grep -c '^IN 2 2 2$' "$out/never-settles-cpp/requests.dfu")" = "$(grep -c '^IN 2 2 2$' "$out/never-settles-rs/requests.dfu")" && grep -q 'did not finish in time' "$out/never-settles-cpp/stderr.txt" && grep -q 'did not finish within 120 polls' "$out/never-settles-rs/stdout.txt"; echo $?)"

# The flash refuses a block of the wireless stack image: that step fails, both go on with the firmware.
run radio-fails cpp --fus running --fail-write-at 0x080D4000
run radio-fails rs --fus running --fail-write-at 0x080D4000
compare radio-fails 3
check "radio-fails: Rust reports the wireless stack not installed" "$(grep -q '"installed":false' "$out/radio-fails-rs/stdout.txt"; echo $?)"

# The flash refuses a block of the firmware: both stop there (the chip stays in its bootloader).
run firmware-fails cpp --fus running --fail-write-at 0x08005000
run firmware-fails rs --fus running --fail-write-at 0x08005000
compare firmware-fails fail
check "firmware-fails: C++ reports the failed download" "$(grep -q 'Firmware Download @Fakeflip ERROR' "$out/firmware-fails-cpp/stderr.txt" "$out/firmware-fails-cpp/stdout.txt"; echo $?)"
check "firmware-fails: Rust reports the failed write" "$(grep -q '"ok":false' "$out/firmware-fails-rs/stdout.txt" && grep -q 'write the firmware' "$out/firmware-fails-rs/stdout.txt"; echo $?)"

# The legacy update (LegacyUpdateOperation: a Flipper without SD card): the chip starts as the firmware; /int is
# backed up, the firmware reboots into its bootloader, the firmware is written and the chip leaves the bootloader
# (the wireless stack is current), then the resources (no card: nothing), /int restored, a restart.
legacy=plain
run legacy cpp --start-firmware --no-sd --fus running
run legacy rs --start-firmware --no-sd --fus running
compare legacy 0
check "legacy: C++ ran the legacy update" "$(grep -q 'Legacy Update @Fakeflip SUCCESS' "$out/legacy-cpp/stderr.txt" "$out/legacy-cpp/stdout.txt"; echo $?)"
check "legacy: both backed up /int, rebooted into DFU and restored /int" \
    "$(ok=0; for side in cpp rs; do grep -q 'SystemRebootRequest mode 1' "$out/legacy-$side/requests.rpc" && grep -q 'StorageWriteRequest /int/' "$out/legacy-$side/requests.rpc" || ok=1; done; echo $ok)"

# The same with a wireless stack older than the update's: boot mode, stack, firmware, option bytes, as the repair.
legacy=radio
run legacy-radio cpp --start-firmware --no-sd --old-radio --fus running
run legacy-radio rs --start-firmware --no-sd --old-radio --fus running
compare legacy-radio 0

# ... and with the stack refused by the flash: the update goes on, and both say it is not plain success.
run legacy-radio-fails cpp --start-firmware --no-sd --old-radio --fus running --fail-write-at 0x080D4000
run legacy-radio-fails rs --start-firmware --no-sd --old-radio --fus running --fail-write-at 0x080D4000
legacy=
compare legacy-radio-fails 3u

# A resources bundle with a file that is not what its manifest says (its SHA-256 in directory.json is right):
# argus-cli refuses it before it opens anything; the C++ app repairs the chip, then refuses the bundle before it
# removes or writes anything on the card (it used to write the file anyway).
fw=$out/firmware-mismatch
run mismatch cpp --fus running
run mismatch rs --fus running
fw=$out/firmware
ce=$(cat "$out/mismatch-cpp/exit.txt") re=$(cat "$out/mismatch-rs/exit.txt")
check "mismatch: C++ fails ($ce) on the bundle" \
    "$(test "$ce" = 255 && grep -q 'Readme.txt differs from the manifest (size or MD5)' "$out/mismatch-cpp/stderr.txt" "$out/mismatch-cpp/stdout.txt"; echo $?)"
check "mismatch: C++ changed nothing on the card" \
    "$(! grep -q 'Storage\(Delete\|Write\|Mkdir\)Request' "$out/mismatch-cpp/requests.txt" && "$fake" --print-dump | cmp -s - "$out/mismatch-cpp/dump.txt"; echo $?)"
check "mismatch: Rust refuses the bundle ($re) before it opens anything" \
    "$(test "$re" = 1 && grep -q 'Readme.txt differs from the manifest' "$out/mismatch-rs/stdout.txt" && ! grep -qv '^$' "$out/mismatch-rs/dfu.log" && test ! -s "$out/mismatch-rs/requests.txt"; echo $?)"

# A Flipper of region 1 (not the development region): after the resources, both write /int/.region_data from the
# region bundle (the C++ app from the folder through its test hook, argus-cli over HTTPS from the fake server), the
# bands of the country the server saw (DE). Same requests, same bytes on the card.
run region cpp --fus running --region 1
run region rs --fus running --region 1
compare region 0
check "region: both wrote the region data" \
    "$(grep -q '^f /int/.region_data ' "$out/region-cpp/dump.txt" && grep -q 'StorageWriteRequest /int/.region_data' "$out/region-rs/requests.rpc"; echo $?)"
check "region: Rust used DE's bands" "$(grep -q '"country":"DE"' "$out/region-rs/stdout.txt"; echo $?)"

# The server names no country: the C++ app takes its locale's territory (none in the C locale), argus-cli --country
# (none given): both write the default bands, without a country code.
fw=$out/firmware-nocountry upd=$upd_nocountry ca=$out/srv_nocountry.pem cpp_locale=C.UTF-8
run region-nocountry cpp --fus running --region 1
run region-nocountry rs --fus running --region 1
fw=$out/firmware upd=$upd_main ca=$out/srv_main.pem cpp_locale=
compare region-nocountry 0
check "region-nocountry: Rust used the default bands" "$(grep -q '"country":""' "$out/region-nocountry-rs/stdout.txt"; echo $?)"

# The legacy update of a region-1 Flipper: /int is restored, then the region data written, in both.
legacy=plain
run region-legacy cpp --start-firmware --no-sd --fus running --region 1
run region-legacy rs --start-firmware --no-sd --fus running --region 1
legacy=
compare region-legacy 0

# ... with an older /int/.region_data on the device: /int is restored first, then the fresh region data is written,
# so the fresh data stays (it decides the legal radio bands; both apps used to restore the old file over it).
legacy=plain
run region-legacy-old cpp --start-firmware --no-sd --fus running --region 1 --old-region-data
run region-legacy-old rs --start-firmware --no-sd --fus running --region 1 --old-region-data
legacy=
compare region-legacy-old 0
fresh=$(grep '^f /int/.region_data ' "$out/region-cpp/dump.txt")
check "region-legacy-old: both end with the fresh region data, not the backup's" \
    "$(test -n "$fresh" && grep -qxF "$fresh" "$out/region-legacy-old-cpp/dump.txt" && grep -qxF "$fresh" "$out/region-legacy-old-rs/dump.txt" &&
       ! "$fake" --old-region-data --print-dump | grep -qxF "$fresh"; echo $?)"
check "region-legacy-old: both restored the backup's file first and then wrote the fresh one (2 writes each)" \
    "$(test "$(grep -c 'StorageWriteRequest /int/.region_data' "$out/region-legacy-old-cpp/requests.rpc")" = 2 &&
       test "$(grep -c 'StorageWriteRequest /int/.region_data' "$out/region-legacy-old-rs/requests.rpc")" = 2; echo $?)"

# A band the firmware cannot hold (200 dBm): both refuse the bundle and write no region data. argus-cli fetches it
# before it changes anything on the firmware side; the C++ app has already put the resources on the card then.
fw=$out/firmware-badband upd=$upd_badband ca=$out/srv_badband.pem
run region-badband cpp --fus running --region 1
run region-badband rs --fus running --region 1
fw=$out/firmware upd=$upd_main ca=$out/srv_main.pem
ce=$(cat "$out/region-badband-cpp/exit.txt") re=$(cat "$out/region-badband-rs/exit.txt")
check "region-badband: C++ fails ($ce): the server returned invalid data" \
    "$(test "$ce" = 255 && grep -q 'Server returned invalid data' "$out/region-badband-cpp/stderr.txt" "$out/region-badband-cpp/stdout.txt"; echo $?)"
check "region-badband: Rust fails ($re) on the bundle" "$(test "$re" = 1 && grep -q 'Band value out of range' "$out/region-badband-rs/stdout.txt"; echo $?)"
check "region-badband: neither wrote region data" \
    "$(! grep -q '/int/.region_data' "$out/region-badband-cpp/dump.txt" "$out/region-badband-rs/dump.txt"; echo $?)"
check "region-badband: the same bootloader requests and flash (the repair itself is done)" \
    "$(requests "$out/region-badband-cpp" | cmp -s - "$(requests "$out/region-badband-rs" > "$out/region-badband-rs.dfu"; echo "$out/region-badband-rs.dfu")" && cmp -s "$out/region-badband-cpp/flash.bin" "$out/region-badband-rs/flash.bin"; echo $?)"

# Only the region scenarios fetched anything, and only the bundle (never a firmware file: argus-cli takes local files).
exec 7>&- 8>&- 9>&-
wait "$srv_main_pid" "$srv_nocountry_pid" "$srv_badband_pid"
check "the update servers saw only region bundle requests ($(cat "$out/srv_main.out" "$out/srv_nocountry.out" "$out/srv_badband.out" | grep -c '^GET '))" \
    "$(cat "$out/srv_main.out" "$out/srv_nocountry.out" "$out/srv_badband.out" | grep '^GET ' | grep -qv '^GET /regions/api/v0/bundle$' && echo 1 || echo 0)"
check "region bundles fetched: 3 + 1 + 1" \
    "$(test "$(grep -c '^GET ' "$out/srv_main.out")" = 3 && test "$(grep -c '^GET ' "$out/srv_nocountry.out")" = 1 && test "$(grep -c '^GET ' "$out/srv_badband.out")" = 1; echo $?)"

echo "compare-repair: $failures failed"
[ "$failures" -eq 0 ]
