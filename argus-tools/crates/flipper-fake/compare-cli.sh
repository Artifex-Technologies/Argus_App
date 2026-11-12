#!/bin/sh
# Runs the C++ qFlipper-cli and the Rust argus-cli against the same fake Flipper on a
# pseudo-terminal and compares them: exit status, the archives they produce, and the RPC requests
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
check() { # check NAME RESULT (0 = ok)
    if [ "$2" -eq 0 ]; then echo "  ok    $1"; else echo "  FAIL  $1"; failures=$((failures + 1)); fi
}
note() { echo "  note  $1"; }

# start_fake DIR SESSIONS [fake args...] -> sets $port; the fake runs in the background
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

# backup_both NAME [fake args...]: both CLIs back up /int from their own fake with the same args.
# The C++ app opens the port twice (device information, then the backup).
backup_both() {
    name=$1; shift
    start_fake "$out/$name-cpp" 2 "$@"
    ARGUS_TEST_SERIAL_PORT=$port timeout 90 "$cpp" backup "$out/$name-cpp/backup.tgz" > "$out/$name-cpp/stdout.txt" 2> "$out/$name-cpp/stderr.txt"
    echo $? > "$out/$name-cpp/exit.txt"
    stop_fake
    start_fake "$out/$name-rs" 1 "$@"
    timeout 90 "$rs" --port "$port" backup "$out/$name-rs/backup.tgz" > "$out/$name-rs/stdout.txt" 2> "$out/$name-rs/stderr.txt"
    echo $? > "$out/$name-rs/exit.txt"
    stop_fake
}

exit_of() { cat "$out/$1/exit.txt"; }

# requests of the work itself, without session and command id, and without what only the C++
# app asks once connected: its screen (Gui*) and, when its console attaches, devinfo
storage_requests() {
    if [ -n "$(grep '^2 ' "$out/$1/requests.txt")" ]; then grep '^2 ' "$out/$1/requests.txt"; else cat "$out/$1/requests.txt"; fi \
        | cut -d' ' -f3- | grep -v '^Gui\|^PropertyGetRequest'
}

# The C++ restore reads past single zero blocks; plain tar must now read every entry too.
extract() { mkdir -p "$2" && tar -xzf "$1" -C "$2" 2> "$2.err"; }

same_backup() { # same_backup NAME: exit 0 on both, same files and bytes, same storage requests, plain tar reads all
    name=$1
    check "$name: C++ exit 0" "$(test "$(exit_of "$name-cpp")" = 0; echo $?)"
    check "$name: Rust exit 0" "$(test "$(exit_of "$name-rs")" = 0; echo $?)"
    extract "$out/$name-cpp/backup.tgz" "$out/$name-cpp/tree"
    extract "$out/$name-rs/backup.tgz" "$out/$name-rs/tree"
    diff -r "$out/$name-cpp/tree" "$out/$name-rs/tree" > "$out/$name-tree.diff" 2>&1
    check "$name: same files and bytes in both archives" $?
    all=$(tar --ignore-zeros -tzf "$out/$name-cpp/backup.tgz" 2>/dev/null | wc -l)
    plain=$(tar -tzf "$out/$name-cpp/backup.tgz" 2>/dev/null | wc -l)
    check "$name: plain tar lists every entry of the C++ archive ($plain of $all)" "$(test "$plain" -eq "$all" && test "$all" -gt 0; echo $?)"
    storage_requests "$name-cpp" > "$out/$name-cpp/storage.txt"
    storage_requests "$name-rs" > "$out/$name-rs/storage.txt"
    diff "$out/$name-cpp/storage.txt" "$out/$name-rs/storage.txt" > "$out/$name-requests.diff"
    check "$name: same storage requests in the same order" $?
}

echo "== backup (/int to .tar.gz)"
backup_both backup
same_backup backup
note "C++ only: the device probe before the backup ($(grep '^1 ' "$out/backup-cpp/requests.txt" | cut -d' ' -f3 | tr '\n' ' '))"
note "C++ only: $(grep '^2 ' "$out/backup-cpp/requests.txt" | cut -d' ' -f3 | grep '^Gui' | tr '\n' ' ')"

echo "== an empty file's read answer without its data field (valid protobuf)"
backup_both omit-empty-data --fault omit-empty-data
same_backup omit-empty-data

echo "== a broadcast screen frame without its data field"
backup_both frame-without-data --fault frame-without-data
same_backup frame-without-data

echo "== names in /int that would lead outside the backup folder"
backup_both hostile --hostile
same_backup hostile
for side in cpp rs; do
    grep -q 'StorageReadRequest .*\(\.\.\|\\\\\|\\n\|\\u\)' "$out/hostile-$side/requests.txt"
    check "hostile: $side read none of them" "$(test $? -ne 0; echo $?)"
done

for fault in short-read long-read; do
    echo "== a file arrives with a size other than its stat said ($fault)"
    backup_both "$fault" --fault "$fault"
    for side in cpp rs; do
        check "$fault: $side fails (exit $(exit_of "$fault-$side")) and leaves no archive" \
            "$(test "$(exit_of "$fault-$side")" != 0 && test ! -e "$out/$fault-$side/backup.tgz"; echo $?)"
    done
done

# ---- capture pull into the datastore (qFlipper-cli chymaera pull-all | argus-cli capture pull-all) ----
# run_pull NAME SIDE DATA_HOME ARGS...: one pull from a fresh fake with the capture files
run_pull() {
    name=$1 side=$2 data=$3; shift 3
    if [ "$side" = cpp ]; then
        start_fake "$out/$name-cpp" 2 --captures
        XDG_DATA_HOME=$data ARGUS_TEST_SERIAL_PORT=$port timeout 90 "$cpp" chymaera "$@" > "$out/$name-cpp/stdout.txt" 2> "$out/$name-cpp/stderr.txt"
        echo $? > "$out/$name-cpp/exit.txt"
    else
        start_fake "$out/$name-rs" 1 --captures
        XDG_DATA_HOME=$data timeout 90 "$rs" --port "$port" capture "$@" > "$out/$name-rs/stdout.txt" 2> "$out/$name-rs/stderr.txt"
        echo $? > "$out/$name-rs/exit.txt"
        # argus-cli refuses a non-capture path before opening the port; don't let the fake
        # (started by this script) wait for it
        [ "$name" = outside ] && kill "$fake_pid" 2>/dev/null
    fi
    stop_fake
}

rows() { # rows DATA_HOME: the captures table without times and local paths, sorted
    sqlite3 "$1/Argus/chymaera/chymaera.db" "SELECT type, name, source_path, size, meta, sha256 FROM captures ORDER BY source_path, sha256"
}

echo "== capture pull-all into the datastore"
mkdir -p "$out/data-cpp" "$out/data-rs"
run_pull pull-all cpp "$out/data-cpp" pull-all
run_pull pull-all rs "$out/data-rs" pull-all
check "pull-all: C++ exit 0" "$(test "$(exit_of pull-all-cpp)" = 0; echo $?)"
check "pull-all: Rust exit 0" "$(test "$(exit_of pull-all-rs)" = 0; echo $?)"
rows "$out/data-cpp" > "$out/pull-all-cpp/rows.txt"
rows "$out/data-rs" > "$out/pull-all-rs/rows.txt"
diff "$out/pull-all-cpp/rows.txt" "$out/pull-all-rs/rows.txt" > "$out/pull-all-rows.diff"
check "pull-all: same captures recorded ($(wc -l < "$out/pull-all-rs/rows.txt") rows: kind, name, path, size, meta, sha256)" $?
storage_requests pull-all-cpp > "$out/pull-all-cpp/storage.txt"
storage_requests pull-all-rs > "$out/pull-all-rs/storage.txt"
diff "$out/pull-all-cpp/storage.txt" "$out/pull-all-rs/storage.txt" > "$out/pull-all-requests.diff"
check "pull-all: same storage requests in the same order" $?
note "C++ only, once connected: $(grep '^2 ' "$out/pull-all-cpp/requests.txt" | cut -d' ' -f3- | grep '^Gui\|^PropertyGetRequest' | tr '\n' ' ')"
for side in cpp rs; do
    grep -q 'u2f\|StorageReadRequest .*/assets/' "$out/pull-all-$side/requests.txt"
    check "pull-all: $side never read /ext/u2f or an assets file" "$(test $? -ne 0; echo $?)"
done

echo "== each app pulls again into the other's datastore: nothing new"
run_pull again cpp "$out/data-rs" pull-all
run_pull again rs "$out/data-cpp" pull-all
check "again: C++ exit 0" "$(test "$(exit_of again-cpp)" = 0; echo $?)"
check "again: Rust exit 0, 0 new" "$(test "$(exit_of again-rs)" = 0 && grep -q '"new":0' "$out/again-rs/stdout.txt"; echo $?)"
rows "$out/data-rs" > "$out/again-cpp-rows.txt"
rows "$out/data-cpp" > "$out/again-rs-rows.txt"
check "again: C++ added nothing to the Rust datastore" "$(cmp -s "$out/again-cpp-rows.txt" "$out/pull-all-rs/rows.txt"; echo $?)"
check "again: Rust added nothing to the C++ datastore" "$(cmp -s "$out/again-rs-rows.txt" "$out/pull-all-cpp/rows.txt"; echo $?)"

echo "== a path outside the capture folders"
run_pull outside cpp "$out/data-cpp" pull /int
run_pull outside rs "$out/data-rs" pull /int
for side in cpp rs; do
    check "outside: $side refuses (exit $(exit_of "outside-$side"))" "$(test "$(exit_of "outside-$side")" = 1; echo $?)"
    # no request file at all: the CLI never opened the port
    ! [ -f "$out/outside-$side/requests.txt" ] || ! grep -q 'StorageReadRequest\|StorageListRequest /int' "$out/outside-$side/requests.txt"
    check "outside: $side read nothing from /int" $?
done

echo "== results in $out"
[ $failures -eq 0 ] && echo "all checks passed" || echo "$failures check(s) failed"
exit $((failures > 0))
