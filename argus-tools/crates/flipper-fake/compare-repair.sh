#!/bin/sh
# Runs the C++ full repair (qFlipper-cli with no command, on a Flipper in recovery mode: FullRepairOperation)
# and the Rust one (argus-cli recovery repair) against the same fake Flipper across its lives (flipper-fake
# --recovery: the bootloader on a Unix socket, then the firmware on a pseudo-terminal), and compares them: exit
# status, the bootloader requests in order, the bytes on the fake chip (all of its flash), the option bytes
# (RDP untouched), the FUS and resets, and what each says. docs/RUST_PORT.md, section 4.
#
#   sh compare-repair.sh QFLIPPER_CLI ARGUS_CLI FLIPPER_FAKE OUTDIR
#
# QFLIPPER_CLI must be built with the test hooks (backend/testhooks.h): the script refuses a binary without
# them, because without them the C++ CLI would watch USB and could find a real Flipper. With them it sees only
# what the fake's state file says, reaches the bootloader only over the fake's socket (never libusb), the
# firmware only on the fake's /dev/pts/N, and reads the update files from a made-up local folder (never the
# network). It runs with its own HOME and XDG folders under OUTDIR, which must not exist. Nothing is flashed.
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

failures=0
check() { # check NAME RESULT (0 = ok)
    if [ "$2" -eq 0 ]; then echo "  ok    $1"; else echo "  FAIL  $1"; failures=$((failures + 1)); fi
}

# run SCENARIO SIDE [fake args...]: one repair by one side on a fresh fake; leaves $out/SCENARIO-SIDE/
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
            ARGUS_TEST_FIRMWARE_DIR="$fw" \
            timeout 300 "$cpp" -d 2 > stdout.txt 2> stderr.txt)
    else
        (cd "$dir" && exec timeout 300 "$rs" --dfu-socket dfu.sock --poll-ms 200 recovery repair "$fw/firmware.dfu" \
            "$fw/ob.data" "$fw/radio.bin" --yes > stdout.txt 2> stderr.txt)
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
            skip = after_id && n >= 4 && lines[1] == "IN 3 0 6" && lines[2] == "OUT 6 0 -" && lines[3] == "IN 3 0 6"
            after_id = 0
            for (i = 0; i < n; i++) if (!skip || (i != 2 && i != 3)) print lines[i]
        }
        END { if (n) flush() }
    ' "$1/dfu.log"
}
field() { sed -n "s/^$2 //p" "$1/chip.txt"; }

# compare SCENARIO EXPECTED_EXIT(0|fail): both sides already run
compare() {
    name=$1 want=$2 c=$out/$1-cpp r=$out/$1-rs
    ce=$(cat "$c/exit.txt") re=$(cat "$r/exit.txt")
    if [ "$want" = 0 ]; then
        check "$name: C++ exit 0 ($ce)" "$(test "$ce" = 0; echo $?)"
        check "$name: Rust exit 0 ($re)" "$(test "$re" = 0; echo $?)"
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
    check "$name: RDP untouched (0xAA, level 0) in both" \
        "$(field "$c" option_bytes | grep -q '^aa' && field "$r" option_bytes | grep -q '^aa'; echo $?)"
}

echo "compare-repair: $out"

# A Flipper in normal use (the wireless stack running), as the C++ app finds one in recovery mode.
run running cpp --fus running
run running rs --fus running
compare running 0
check "running: C++ says the repair succeeded" "$(grep -q 'Full Repair @Fakeflip SUCCESS' "$out/running-cpp/stderr.txt" "$out/running-cpp/stdout.txt"; echo $?)"
check "running: Rust installed the wireless stack and wrote the option bytes" \
    "$(grep -q '"radio":{"bytes":12293,"error":null,"installed":true}' "$out/running-rs/stdout.txt" && grep -q '"option_bytes_written":true' "$out/running-rs/stdout.txt"; echo $?)"
check "running: firmware and wireless stack where they belong" \
    "$(test "$(field "$out/running-rs" booted_firmware)" = true && test "$(field "$out/running-rs" fus_commands)" = 5253; echo $?)"

# The FUS already running: the C++ app restarts the chip "for consistency" first.
run fus-idle cpp --fus idle
run fus-idle rs --fus idle
compare fus-idle 0

# A FUS stuck in a state the repair does not know: the wireless stack step fails, both go on with the firmware.
run stuck cpp --fus stuck
run stuck rs --fus stuck
compare stuck 0
check "stuck: C++ went on without the wireless stack" "$(grep -q 'Attempting to install the firmware anyway' "$out/stuck-cpp/stderr.txt" "$out/stuck-cpp/stdout.txt"; echo $?)"
check "stuck: Rust reports the wireless stack not installed" "$(grep -q '"installed":false' "$out/stuck-rs/stdout.txt"; echo $?)"

# The FUS never finishes deleting the wireless stack: both give up after 120 status polls (they used to poll for
# ever) and go on with the firmware.
run never-settles cpp --fus never-settles
run never-settles rs --fus never-settles
compare never-settles 0
check "never-settles: both gave up after 120 FUS state reads" \
    "$(test "$(grep -c '^IN 2 2 2$' "$out/never-settles-cpp/requests.dfu")" = "$(grep -c '^IN 2 2 2$' "$out/never-settles-rs/requests.dfu")" && grep -q 'did not finish in time' "$out/never-settles-cpp/stderr.txt" && grep -q 'did not finish within 120 polls' "$out/never-settles-rs/stdout.txt"; echo $?)"

# The flash refuses a block of the wireless stack image: that step fails, both go on with the firmware.
run radio-fails cpp --fus running --fail-write-at 0x080D4000
run radio-fails rs --fus running --fail-write-at 0x080D4000
compare radio-fails 0
check "radio-fails: Rust reports the wireless stack not installed" "$(grep -q '"installed":false' "$out/radio-fails-rs/stdout.txt"; echo $?)"

# The flash refuses a block of the firmware: both stop there (the chip stays in its bootloader).
run firmware-fails cpp --fus running --fail-write-at 0x08005000
run firmware-fails rs --fus running --fail-write-at 0x08005000
compare firmware-fails fail
check "firmware-fails: C++ reports the failed download" "$(grep -q 'Firmware Download @Fakeflip ERROR' "$out/firmware-fails-cpp/stderr.txt" "$out/firmware-fails-cpp/stdout.txt"; echo $?)"
check "firmware-fails: Rust reports the failed write" "$(grep -q '"ok":false' "$out/firmware-fails-rs/stdout.txt" && grep -q 'write the firmware' "$out/firmware-fails-rs/stdout.txt"; echo $?)"

echo "compare-repair: $failures failed"
[ "$failures" -eq 0 ]
