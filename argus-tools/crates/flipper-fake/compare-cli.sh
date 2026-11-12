#!/bin/sh
# Runs the C++ qFlipper-cli and the Rust argus-cli against the same fake Flipper on a
# pseudo-terminal and compares them: exit status, the files they produce, and the RPC requests
# they send (decoded by the fake). docs/RUST_PORT.md, section 4.
#
#   sh compare-cli.sh QFLIPPER_CLI ARGUS_CLI FLIPPER_FAKE OUTDIR
#
# QFLIPPER_CLI must be built with the test hook (backend/testhooks.h); without it the C++ CLI
# would watch USB and could find a real Flipper, so the script refuses such a binary. The C++
# CLI only ever sees the fake's /dev/pts/N; no real device is opened, nothing is flashed, and
# the hook keeps it off the network. Everything is written under OUTDIR (which must not exist).
set -u
[ $# -eq 4 ] || { echo "usage: sh compare-cli.sh QFLIPPER_CLI ARGUS_CLI FLIPPER_FAKE OUTDIR" >&2; exit 2; }
cpp=$1 rs=$2 fake=$3 out=$4
for b in "$cpp" "$rs" "$fake"; do [ -x "$b" ] || { echo "not executable: $b" >&2; exit 2; }; done
grep -aq ARGUS_TEST_SERIAL_PORT "$cpp" || { echo "refusing: $cpp has no ARGUS_TEST_SERIAL_PORT hook (it could open a real Flipper)" >&2; exit 2; }
[ -e "$out" ] && { echo "refusing: $out exists" >&2; exit 2; }
mkdir -p "$out" && chmod 700 "$out"

failures=0
check() { # check NAME CONDITION-RESULT(0 = ok)
    if [ "$2" -eq 0 ]; then echo "  ok    $1"; else echo "  FAIL  $1"; failures=$((failures + 1)); fi
}

# start_fake DIR SESSIONS [fake args...] -> sets $port, fake runs in the background
start_fake() {
    dir=$1 sessions=$2; shift 2
    mkdir -p "$dir"
    timeout 120 "$fake" --pty --sessions "$sessions" --requests "$dir/requests.txt" --record "$dir/wire.bin" "$@" > "$dir/fake.out" 2> "$dir/fake.err" &
    fake_pid=$!
    port=
    i=0
    while [ $i -lt 100 ]; do
        port=$(sed -n 's/^pty //p' "$dir/fake.out" 2>/dev/null)
        [ -n "$port" ] && break
        i=$((i + 1)); timeout 0.1 tail -f /dev/null
    done
    case "$port" in /dev/pts/*) ;; *) echo "fake did not start: $(cat "$dir/fake.err")" >&2; exit 1 ;; esac
}

stop_fake() { wait "$fake_pid" 2>/dev/null; }

run_cpp() { # run_cpp DIR ARGS... ; the C++ CLI sees only the fake's pty
    dir=$1; shift
    ARGUS_TEST_SERIAL_PORT=$port timeout 90 "$cpp" "$@" > "$dir/stdout.txt" 2> "$dir/stderr.txt"
    echo $? > "$dir/exit.txt"
}

run_rs() {
    dir=$1; shift
    timeout 90 "$rs" --port "$port" "$@" > "$dir/stdout.txt" 2> "$dir/stderr.txt"
    echo $? > "$dir/exit.txt"
}

# requests without session and command id: "Type arg"
strip() { cut -d' ' -f3- "$1"; }

# The C++ restore reads past single zero blocks (backend/tararchive.cpp), so extract the same way.
extract() { # extract TGZ DIR
    mkdir -p "$2" && tar --ignore-zeros -xzf "$1" -C "$2" 2> "$2.err"
}

note() { echo "  note  $1"; }

echo "== backup (/int to .tar.gz)"
start_fake "$out/backup-cpp" 2
run_cpp "$out/backup-cpp" backup "$out/backup-cpp/backup.tgz"
stop_fake
start_fake "$out/backup-rs" 1
run_rs "$out/backup-rs" backup "$out/backup-rs/backup.tgz"
stop_fake
check "C++ exit 0" "$(test "$(cat "$out/backup-cpp/exit.txt")" = 0; echo $?)"
check "Rust exit 0" "$(test "$(cat "$out/backup-rs/exit.txt")" = 0; echo $?)"
extract "$out/backup-cpp/backup.tgz" "$out/backup-cpp/tree"
extract "$out/backup-rs/backup.tgz" "$out/backup-rs/tree"
diff -r "$out/backup-cpp/tree" "$out/backup-rs/tree" > "$out/backup-tree.diff" 2>&1
check "same files and bytes in both archives (read as the C++ restore reads)" $?
plain_cpp=$(tar -tzf "$out/backup-cpp/backup.tgz" 2>/dev/null | wc -l)
all_cpp=$(tar --ignore-zeros -tzf "$out/backup-cpp/backup.tgz" 2>/dev/null | wc -l)
plain_rs=$(tar -tzf "$out/backup-rs/backup.tgz" 2>/dev/null | wc -l)
check "plain tar reads every entry of the Rust archive ($plain_rs)" "$(test "$plain_rs" -eq "$all_cpp"; echo $?)"
[ "$plain_cpp" -lt "$all_cpp" ] && note "plain tar stops after $plain_cpp of $all_cpp entries in the C++ archive (its zero block after a 512-aligned file)"
# The C++ app's first session is its device-information probe; its second session is the backup.
grep '^1 ' "$out/backup-cpp/requests.txt" > "$out/backup-cpp/probe.txt"
grep '^2 ' "$out/backup-cpp/requests.txt" > "$out/backup-cpp/work.txt"
# The C++ session also starts the screen stream and shows a status screen on the Flipper (Gui*
# requests); the Rust CLI has no screen to drive. Everything else must match.
strip "$out/backup-cpp/work.txt" | grep -v '^Gui' > "$out/backup-cpp/work.stripped"
strip "$out/backup-rs/requests.txt" > "$out/backup-rs/work.stripped"
diff "$out/backup-cpp/work.stripped" "$out/backup-rs/work.stripped" > "$out/backup-requests.diff"
check "same storage requests in the same order (ids aside)" $?
note "C++ only, before the backup: $(strip "$out/backup-cpp/work.txt" | grep '^Gui' | tr '\n' ' ')"
echo "  C++ probe before the backup (Rust does not send it):"
sed 's/^/    /' "$out/backup-cpp/probe.txt"

echo "== backup when the device sends one byte less than its stat said"
start_fake "$out/short-cpp" 2 --fault short-read
run_cpp "$out/short-cpp" backup "$out/short-cpp/backup.tgz"
stop_fake
start_fake "$out/short-rs" 1 --fault short-read
run_rs "$out/short-rs" backup "$out/short-rs/backup.tgz"
stop_fake
echo "  C++ exit $(cat "$out/short-cpp/exit.txt"), archive: $(test -f "$out/short-cpp/backup.tgz" && echo yes || echo no)"
echo "  Rust exit $(cat "$out/short-rs/exit.txt"), archive: $(test -f "$out/short-rs/backup.tgz" && echo yes || echo no)"
check "Rust refuses (exit 1, no archive)" "$(test "$(cat "$out/short-rs/exit.txt")" = 1 && test ! -e "$out/short-rs/backup.tgz"; echo $?)"
if [ -f "$out/short-cpp/backup.tgz" ]; then
    extract "$out/short-cpp/backup.tgz" "$out/short-cpp/tree"
    note "C++ archive anyway, favorites.txt is $(wc -c < "$out/short-cpp/tree/int/favorites.txt") bytes (the device's stat said 37)"
fi

echo "== results in $out"
[ $failures -eq 0 ] && echo "all checks passed" || echo "$failures check(s) failed"
exit $((failures > 0))
