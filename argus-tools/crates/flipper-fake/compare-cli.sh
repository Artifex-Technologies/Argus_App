#!/bin/sh
# Runs the C++ qFlipper-cli and the Rust argus-cli against the same fake Flipper on a
# pseudo-terminal and compares them: exit status, the archives they produce, the RPC requests
# they send (decoded by the fake) and, for restores and the factory reset, the fake's storage
# afterwards (every file's size and MD5). docs/RUST_PORT.md, section 4.
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

# ---- restore and factory reset (phase 4: they change the fake's storage) ----
# dumps of the fake's storage: d PATH | f PATH SIZE MD5, sorted
"$fake" --print-dump > "$out/standard.dump"
"$fake" --print-dump --changed-int > "$out/changed.dump"
"$fake" --print-dump --int-conflict > "$out/conflict.dump"
"$fake" --write-archives "$out/archives" || { echo "fake could not write the test archives" >&2; exit 1; }

# wait_fake: the client has exited; give the fake a moment to finish, then end it (an app that
# refused before opening the port leaves it waiting for a client)
wait_fake() {
    i=0
    while kill -0 "$fake_pid" 2>/dev/null && [ $i -lt 30 ]; do i=$((i + 1)); timeout 0.1 tail -f /dev/null; done
    kill "$fake_pid" 2>/dev/null
    stop_fake
}

# run_change NAME SIDE START COMMAND...: one run of a command that changes the device, from a
# fresh fake started with START (none, --changed-int or --int-conflict); its storage is dumped at the end
run_change() {
    name=$1 side=$2 start=$3; shift 3
    d="$out/$name-$side"
    if [ "$start" = none ]; then start=; fi
    if [ "$side" = cpp ]; then
        start_fake "$d" 2 --dump "$d/dump.txt" $start
        mkdir -p "$d/tmp"
        TMPDIR="$d/tmp" ARGUS_TEST_SERIAL_PORT=$port timeout 90 "$cpp" "$@" > "$d/stdout.txt" 2> "$d/stderr.txt" < /dev/null
    else
        start_fake "$d" 1 --dump "$d/dump.txt" $start
        timeout 90 "$rs" --port "$port" "$@" --yes > "$d/stdout.txt" 2> "$d/stderr.txt" < /dev/null
    fi
    echo $? > "$d/exit.txt"
    wait_fake
}

changes() { # changes NAME-SIDE: the requests of the work that change the device
    [ -f "$out/$1/requests.txt" ] || return 0
    storage_requests "$1" | grep 'Storage\(Write\|Mkdir\|Delete\|Rename\)Request\|SystemRebootRequest\|SystemFactoryResetRequest'
}

same_restore() { # same_restore NAME START_DUMP: both exit 0, same storage afterwards, same requests
    name=$1
    check "$name: C++ exit 0" "$(test "$(exit_of "$name-cpp")" = 0; echo $?)"
    check "$name: Rust exit 0" "$(test "$(exit_of "$name-rs")" = 0; echo $?)"
    check "$name: the same bytes on the fake afterwards" "$(cmp -s "$out/$name-cpp/dump.txt" "$out/$name-rs/dump.txt"; echo $?)"
    storage_requests "$name-cpp" > "$out/$name-cpp/storage.txt"
    storage_requests "$name-rs" > "$out/$name-rs/storage.txt"
    diff "$out/$name-cpp/storage.txt" "$out/$name-rs/storage.txt" > "$out/$name-requests.diff"
    check "$name: same requests in the same order ($(grep -c 'Write' "$out/$name-rs/storage.txt") write frames, $(grep -c 'Ping' "$out/$name-rs/storage.txt") pings)" $?
}

echo "== restore: each app restores its own backup and the other's onto a changed /int"
for from in cpp rs; do
    run_change "restore-$from" cpp --changed-int restore "$out/backup-$from/backup.tgz"
    run_change "restore-$from" rs --changed-int restore "$out/backup-$from/backup.tgz"
    same_restore "restore-$from"
done
check "restore: both backups leave the same bytes" "$(cmp -s "$out/restore-cpp-cpp/dump.txt" "$out/restore-rs-rs/dump.txt"; echo $?)"
grep -v '^f /int/extra.txt ' "$out/restore-cpp-rs/dump.txt" > "$out/restored-without-extra.dump"
check "restore: /int is back as backed up, the file added since stays" \
    "$(cmp -s "$out/restored-without-extra.dump" "$out/standard.dump" && grep -q '^f /int/extra.txt ' "$out/restore-cpp-rs/dump.txt"; echo $?)"

for good in old-format record-padding large-file; do
    echo "== restore: $good"
    run_change "good-$good" cpp --changed-int restore "$out/archives/good-$good.tgz"
    run_change "good-$good" rs --changed-int restore "$out/archives/good-$good.tgz"
    same_restore "good-$good"
done

echo "== restore over a file where the backup has a folder (/int/sub): refused before writing"
run_change conflict cpp --int-conflict restore "$out/backup-rs/backup.tgz"
run_change conflict rs --int-conflict restore "$out/backup-cpp/backup.tgz"
for side in cpp rs; do
    check "conflict: $side refuses (exit $(exit_of "conflict-$side")), writes nothing, the storage is unchanged" \
        "$(test "$(exit_of "conflict-$side")" != 0 && test -z "$(changes "conflict-$side")" && cmp -s "$out/conflict-$side/dump.txt" "$out/conflict.dump"; echo $?)"
done

echo "== restore onto storage that refuses every change (access denied): both fail at the first write"
run_change denied cpp --read-only restore "$out/backup-rs/backup.tgz"
run_change denied rs --read-only restore "$out/backup-cpp/backup.tgz"
for side in cpp rs; do
    check "denied: $side fails (exit $(exit_of "denied-$side")) and the storage is unchanged" \
        "$(test "$(exit_of "denied-$side")" != 0 && cmp -s "$out/denied-$side/dump.txt" "$out/standard.dump"; echo $?)"
done
storage_requests denied-cpp > "$out/denied-cpp/storage.txt"
storage_requests denied-rs > "$out/denied-rs/storage.txt"
diff "$out/denied-cpp/storage.txt" "$out/denied-rs/storage.txt" > "$out/denied-requests.diff"
check "denied: same requests, both stop after the first refused write" $?

echo "== hostile archives: both apps refuse each before writing anything"
{ cut -f1 "$out/archives/hostile.txt" | sed 's/^/hostile-/'; echo bomb; } > "$out/archives/refuse.txt"
while read -r a; do
    run_change "$a" cpp --changed-int restore "$out/archives/$a.tgz"
    run_change "$a" rs --changed-int restore "$out/archives/$a.tgz"
    check "$a: both refuse (C++ exit $(exit_of "$a-cpp"), Rust exit $(exit_of "$a-rs"))" \
        "$(test "$(exit_of "$a-cpp")" != 0 && test "$(exit_of "$a-rs")" = 1; echo $?)"
    # the Rust CLI checks the archive before it opens the port: the fake never saw it
    check "$a: nothing written (C++: no change requests, storage unchanged; Rust: port never opened)" \
        "$(test -z "$(changes "$a-cpp")" && cmp -s "$out/$a-cpp/dump.txt" "$out/changed.dump" && test ! -e "$out/$a-rs/requests.txt"; echo $?)"
done < "$out/archives/refuse.txt"
check "hostile: no file was written outside its folder (no escaped.txt anywhere under $out)" "$(test -z "$(find "$out" -name escaped.txt)"; echo $?)"

echo "== factory reset (qFlipper-cli erase | argus-cli erase --yes)"
run_change erase cpp none erase
run_change erase rs none erase
same_restore erase
check "erase: /int is empty afterwards" "$(! grep -q '^[df] /int/' "$out/erase-rs/dump.txt"; echo $?)"

echo "== nothing changes the device without --yes (argus-cli)"
for c in "restore $out/backup-cpp/backup.tgz" "erase" "reboot" "mkdir /int/x" "rm /int/favorites.txt" "mv /int/a /int/b" "push $out/standard.dump /int/x"; do
    # shellcheck disable=SC2086
    "$rs" --port "$out/no-such-port" $c > "$out/confirm.out" 2>&1
    code=$?
    check "argus-cli $(echo "$c" | cut -d' ' -f1) without --yes: exit $code (2), the port is not opened" \
        "$(test $code -eq 2 && grep -q '"confirm"' "$out/confirm.out"; echo $?)"
done

echo "== results in $out"
[ $failures -eq 0 ] && echo "all checks passed" || echo "$failures check(s) failed"
exit $((failures > 0))
