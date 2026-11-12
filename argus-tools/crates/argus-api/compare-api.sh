#!/bin/bash
# Runs the C++ Sarina server (qFlipper-cli chymaera serve) and the Rust one (argus-cli chymaera serve)
# with the same datastore, capture library and files, sends both the same requests
# (tests/fixtures/api_requests.txt) and compares the answers, then the token file's life, the port
# argument, `chymaera parse|import|selftest`, and both servers with the fake Flipper attached
# (a pseudo-terminal: `serve --device` through the C++ test hook, `argus-cli --port PTY`).
# docs/RUST_PORT.md, section 4.
#
#   bash compare-api.sh QFLIPPER_CLI ARGUS_CLI ARGUS_TOOLS FLIPPER_FAKE OUTDIR [--record]
#
# Everything runs in a private network namespace (unshare -rn) whose only interface is its own
# loopback: the servers here can never meet the Sarina API the owner runs, whatever port it uses.
# Where user namespaces are not allowed and $CI is set (a CI container), it uses that machine's
# loopback; anywhere else it refuses. Every server gets its own HOME, XDG_DATA_HOME and
# XDG_RUNTIME_DIR under OUTDIR (which must not exist). QFLIPPER_CLI must carry the test hook
# (backend/testhooks.h), which keeps it off USB, though `chymaera` commands never look for a device.
# --record writes the C++ answers (normalised) to tests/fixtures/api_responses.txt, which
# tests/api.rs replays against the Rust bridge without Qt.
set -u
[ $# -ge 5 ] || { echo "usage: bash compare-api.sh QFLIPPER_CLI ARGUS_CLI ARGUS_TOOLS FLIPPER_FAKE OUTDIR [--record]" >&2; exit 2; }
cpp=$1 rs=$2 tools=$3 fake=$4 out=$5 record=${6:-}
here=$(cd "$(dirname "$0")" && pwd)
fixtures=$here/tests/fixtures

if [ -z "${ARGUS_API_NETNS:-}" ]; then
    for b in "$cpp" "$rs" "$tools" "$fake"; do [ -x "$b" ] || { echo "not executable: $b" >&2; exit 2; }; done
    grep -aq ARGUS_TEST_SERIAL_PORT "$cpp" || { echo "refusing: $cpp has no ARGUS_TEST_SERIAL_PORT hook" >&2; exit 2; }
    [ -e "$out" ] && { echo "refusing: $out exists" >&2; exit 2; }
    if unshare -rn true 2>/dev/null; then
        exec unshare -rn env ARGUS_API_NETNS=private bash "$0" "$@"
    elif [ -n "${CI:-}" ]; then
        echo "note: no user namespaces in this CI container; using its own loopback"
        export ARGUS_API_NETNS=ci
    else
        echo "refusing: cannot make a private network namespace (unshare -rn)" >&2
        exit 2
    fi
fi
if [ "$ARGUS_API_NETNS" = private ]; then
    ip link set lo up || { echo "cannot bring up the namespace's loopback" >&2; exit 2; }
fi
mkdir -p "$out" && chmod 700 "$out"
out=$(cd "$out" && pwd)

failures=0
check() { # check NAME RESULT (0 = ok)
    if [ "$2" -eq 0 ]; then echo "  ok    $1"; else echo "  FAIL  $1"; failures=$((failures + 1)); fi
}

# the capture library both servers read, built once
mkdir -p "$out/lib"
"$tools" capture index --library "$out/lib" --tree "$fixtures/api-files/tree" > "$out/lib.json" || { echo "capture index failed" >&2; exit 1; }
garage=$(sha256sum "$fixtures/api-files/garage.sub" | cut -c1-16)
token=compare-api-test-token

# setup DIR: the files, an old-schema datastore and the library for one server
setup() {
    d=$1
    mkdir -p "$d/home" "$d/data/Argus/chymaera" "$d/data/argus" "$d/run"
    chmod 700 "$d/run" "$d/data/argus"
    cp -r "$fixtures/api-files" "$d/files"
    mkdir "$d/files/folder.nfc"
    mkfifo "$d/files/pipe.sub"
    ln -s garage.sub "$d/files/link.sub"
    sqlite3 "$d/data/Argus/chymaera/chymaera.db" < "$fixtures/api-seed.sql"
    cp -r "$out/lib" "$d/data/argus/captures"
}

# run BIN DIR ARGS...: a CLI with the server's environment
run() {
    b=$1 d=$2; shift 2
    env -i PATH="$PATH" HOME="$d/home" XDG_DATA_HOME="$d/data" XDG_RUNTIME_DIR="$d/run" ARGUS_TOOLS="$tools" \
        ARGUS_TEST_SERIAL_PORT="${test_port:-$d/no-such-port}" QT_QPA_PLATFORM=offscreen "$b" "$@"
}

# serve BIN DIR ARGS... -> $port and $server_pid
serve() {
    b=$1 d=$2; shift 2
    log=$d/serve-$((${serve_n:=0} + 1)).out
    serve_n=$((serve_n + 1))
    # $pre: options before `chymaera` (argus-cli's --port), split on spaces on purpose
    # shellcheck disable=SC2086
    run "$b" "$d" ${pre:-} chymaera serve "$@" > "$log" 2> "$log.err" &
    server_pid=$!
    port=
    for _ in $(seq 100); do
        port=$(sed -n 's/^Sarina control API listening on 127\.0\.0\.1:\([0-9]*\).*/\1/p' "$log")
        [ -n "$port" ] && return 0
        kill -0 "$server_pid" 2>/dev/null || return 1
        timeout 0.1 tail -f /dev/null
    done
    return 1
}

stop_server() {
    kill -TERM "$server_pid" 2>/dev/null
    wait "$server_pid" 2>/dev/null
}

# requests FILE: send each line, read one answer per non-blank line
requests() {
    exec 3<>"/dev/tcp/127.0.0.1/$port"
    while IFS= read -r line; do
        req=${line//@TOKEN@/$token}
        req=${req//@FILES@/$d/files}
        req=${req//@GARAGE@/$garage}
        printf '%s\n' "$req" >&3
        if [ -n "${req//[[:space:]]/}" ]; then
            IFS= read -r -t 20 resp <&3 || resp="(no answer)"
            printf '%s\n' "$resp"
        fi
    done < "$1"
    exec 3>&-
}

normalise() { # normalise DIR < answers
    sed -E -e 's/"timestamp":"[^"]*"/"timestamp":"T"/g' -e 's/"epochMs":[0-9]+/"epochMs":0/g' -e 's/"ts":[0-9]+/"ts":0/g' \
        -e 's/"serverPort":[0-9]+/"serverPort":0/g' -e 's/127\.0\.0\.1:[0-9]+/127.0.0.1:PORT/g' \
        -e "s#$1/files#@FILES@#g" -e "s#$1/data#@DATA@#g" -e "s#$1/run#@RUN@#g"
}

echo "== the recorded requests ($(grep -c '' "$fixtures/api_requests.txt") lines), each server on its own copy"
for side in cpp rs; do
    d=$out/$side
    setup "$d"
    bin=$cpp; [ $side = rs ] && bin=$rs
    if serve "$bin" "$d" 0 "$token"; then
        requests "$fixtures/api_requests.txt" > "$d/answers.raw"
        # two requests in one write, then one in two writes
        exec 3<>"/dev/tcp/127.0.0.1/$port"
        printf '{"cmd":"ping","token":"%s","id":"a"}\n{"cmd":"ping","token":"%s","id":"b"}\n{"cmd":"pi' "$token" "$token" >&3
        timeout 0.3 tail -f /dev/null
        printf 'ng","token":"%s","id":"c"}\n' "$token" >&3
        for _ in 1 2 3; do IFS= read -r -t 20 resp <&3 && printf '%s\n' "$resp"; done >> "$d/answers.raw"
        exec 3>&-
        stop_server
    else
        echo "  $side server did not start: $(cat "$d"/serve-1.out "$d"/serve-1.out.err)"
    fi
    normalise "$d" < "$d/answers.raw" > "$d/answers.txt"
done
check "both servers answered every request ($(grep -c '' "$out/rs/answers.txt") answers)" \
    "$(test "$(grep -c '' "$out/cpp/answers.txt")" -gt 70 && ! grep -q '(no answer)' "$out/cpp/answers.txt" "$out/rs/answers.txt"; echo $?)"
diff "$out/cpp/answers.txt" "$out/rs/answers.txt" > "$out/answers.diff"
check "the same answers, byte for byte (normalised: times, port, folders)" $?
bad=$(grep -o '"timestamp":"[^"]*"' "$out/cpp/answers.raw" "$out/rs/answers.raw" | grep -cvE '"timestamp":"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}"$')
check "every timestamp is local yyyy-MM-ddTHH:mm:ss.zzz" "$(test "$bad" -eq 0; echo $?)"
for side in cpp rs; do
    rows=$(sqlite3 "$out/$side/data/Argus/chymaera/chymaera.db" "SELECT type, name, size, meta, sha256, session_id IS NULL FROM captures ORDER BY id")
    echo "$rows" > "$out/$side/rows.txt"
    sqlite3 "$out/$side/data/Argus/chymaera/chymaera.db" "SELECT session_id IS NULL, severity, source, message, coalesce(detail, '<null>') FROM events ORDER BY id" \
        | normalise "$out/$side" > "$out/$side/events.txt"
done
diff "$out/cpp/rows.txt" "$out/rs/rows.txt" > /dev/null
check "the same capture rows in both datastores" $?
diff "$out/cpp/events.txt" "$out/rs/events.txt" > "$out/events.diff"
check "the same events in both datastores ($(grep -c '' "$out/rs/events.txt"))" $?
if [ "$record" = --record ]; then
    cp "$out/cpp/answers.txt" "$fixtures/api_responses.txt"
    echo "  recorded the C++ answers in tests/fixtures/api_responses.txt"
fi
cmp -s "$out/cpp/answers.txt" "$fixtures/api_responses.txt"
check "the C++ answers are the recorded ones (tests/fixtures/api_responses.txt)" $?

echo "== the token file: made when no token is given, kept by a server that cannot listen, removed on stop"
for side in cpp rs; do
    d=$out/$side-token
    mkdir -p "$d/home" "$d/data" "$d/run" && chmod 700 "$d/run"
    bin=$cpp; [ $side = rs ] && bin=$rs
    serve "$bin" "$d" 0 || { check "$side: started without a token" 1; continue; }
    first_pid=$server_pid first_port=$port
    tok=$(cat "$d/run/argus/token" 2>/dev/null)
    mode=$(stat -c %a "$d/run/argus/token" 2>/dev/null)
    check "$side: wrote a 64-digit token, mode 600 ($mode)" "$(test ${#tok} -eq 64 && test "$mode" = 600; echo $?)"
    printf '{"cmd":"ping","token":"%s"}\n' "$tok" > "$d/ping.txt"
    requests "$d/ping.txt" | grep -q '"pong":true'
    check "$side: the written token works" $?
    cp "$d/run/argus/token" "$d/token.before"
    run "$bin" "$d" chymaera serve "$first_port" > "$d/second.out" 2>&1 &
    second=$!
    for _ in $(seq 50); do kill -0 $second 2>/dev/null || break; timeout 0.1 tail -f /dev/null; done
    kill -TERM $second 2>/dev/null; wait $second; second_exit=$?
    check "$side: a second server on the same port fails (exit $second_exit)" "$(test $second_exit -ne 0 && ! grep -q 'listening on' "$d/second.out"; echo $?)"
    cmp -s "$d/token.before" "$d/run/argus/token"
    check "$side: ... and leaves the running server's token file alone" $?
    port=$first_port
    requests "$d/ping.txt" | grep -q '"pong":true'
    check "$side: the first server still takes its token" $?
    server_pid=$first_pid
    stop_server
    check "$side: stopped by SIGTERM, it removed its token file" "$(test ! -e "$d/run/argus/token"; echo $?)"
done

echo "== the port argument"
for side in cpp rs; do
    bin=$cpp; [ $side = rs ] && bin=$rs
    d=$out/$side-port
    mkdir -p "$d/home" "$d/data" "$d/run" && chmod 700 "$d/run"
    for p in 70000 -1 abc; do
        timeout 10 bash -c "$(declare -f run); run '$bin' '$d' chymaera serve '$p' '$token'" > "$d/out-$p.txt" 2>&1
        e=$?
        check "$side: serve $p is refused (exit $e)" "$(test $e -eq 1 && ! grep -q 'listening on' "$d/out-$p.txt"; echo $?)"
    done
done

echo "== chymaera parse and import"
for side in cpp rs; do
    bin=$cpp; [ $side = rs ] && bin=$rs
    d=$out/$side-cli
    setup "$d"
    for f in garage.sub rawsig.sub badge.nfc remote.ir settings.txt scan.pcap folder.nfc pipe.sub missing.sub; do
        timeout 10 bash -c "$(declare -f run); run '$bin' '$d' chymaera parse '$d/files/$f'" > "$d/parse-$f.txt" 2>/dev/null
        echo "exit $?" >> "$d/parse-$f.txt"
    done
    for f in garage.sub garage.sub link.sub pipe.sub folder.nfc settings.txt; do
        timeout 10 bash -c "$(declare -f run); run '$bin' '$d' chymaera import '$d/files/$f'" 2>/dev/null | normalise "$d"
        echo "exit ${PIPESTATUS[0]}"
    done > "$d/import.txt"
    run "$bin" "$d" chymaera parse > "$d/usage.txt" 2>/dev/null; echo "exit $?" >> "$d/usage.txt"
done
for f in garage.sub rawsig.sub badge.nfc remote.ir settings.txt scan.pcap folder.nfc pipe.sub missing.sub; do
    sed "s#$out/cpp-cli#D#g" "$out/cpp-cli/parse-$f.txt" > "$out/cpp-cli/parse-$f.n"
    sed "s#$out/rs-cli#D#g" "$out/rs-cli/parse-$f.txt" > "$out/rs-cli/parse-$f.n"
    cmp -s "$out/cpp-cli/parse-$f.n" "$out/rs-cli/parse-$f.n"
    check "parse $f: the same output and exit ($(tail -1 "$out/rs-cli/parse-$f.txt"))" $?
done
diff "$out/cpp-cli/import.txt" "$out/rs-cli/import.txt" > "$out/import.diff"
check "import: the same output and exits ($(grep -c '^exit 0' "$out/rs-cli/import.txt") imported or already there)" $?

echo "== a device: the fake Flipper on a pseudo-terminal, attached to each server"
# ask JSON: one request to the server on $port, its answer on stdout
ask() { printf '%s\n' "$1" > "$d/ask.txt"; requests "$d/ask.txt"; }
# wait_log TEXT: until a log.tail message starts with TEXT (at most 60 s); prints that message
wait_log() {
    for _ in $(seq 120); do
        m=$(ask '{"cmd":"log.tail","limit":5000,"token":"@TOKEN@"}' | grep -o "\"message\":\"$1[^\"]*\"" | tail -1)
        [ -n "$m" ] && { echo "$m"; return 0; }
        timeout 0.5 tail -f /dev/null
    done
    echo "(no $1 in the log)"
}
for side in cpp rs; do
    d=$out/$side-device
    mkdir -p "$d/home" "$d/data" "$d/run" && chmod 700 "$d/run"
    sessions=1; [ $side = cpp ] && sessions=2
    timeout 300 "$fake" --pty --captures --sessions $sessions --requests "$d/requests.txt" > "$d/fake.out" 2> "$d/fake.err" &
    fake_pid=$!
    pty=
    for _ in $(seq 100); do
        pty=$(sed -n 's/^pty //p' "$d/fake.out" 2>/dev/null)
        [ -n "$pty" ] && break
        timeout 0.1 tail -f /dev/null
    done
    case "$pty" in /dev/pts/*) ;; *) check "$side: the fake started" 1; continue ;; esac
    if [ $side = cpp ]; then
        test_port=$pty serve "$cpp" "$d" 0 "$token" --device
    else
        pre="--port $pty" serve "$rs" "$d" 0 "$token"
    fi || check "$side: the server started with the device" 1
    {
        for _ in $(seq 120); do
            st=$(ask '{"cmd":"device.status","token":"@TOKEN@"}')
            case "$st" in *'"connected":true'*) break ;; esac
            timeout 0.5 tail -f /dev/null
        done
        echo "$st"
        ask '{"cmd":"capture.pullAll","token":"@TOKEN@","id":1}'
        wait_log "Pulled all capture folders: "
        ask '{"cmd":"capture.pull","token":"@TOKEN@","path":"/ext/nfc","id":2}'
        wait_log "Pulled /ext/nfc: "
        ask '{"cmd":"capture.pull","token":"@TOKEN@","path":"/ext/u2f","id":3}'
        ask '{"cmd":"device.status","token":"@TOKEN@"}'
    } > "$d/answers.txt"
    stop_server
    # the fake writes its request log when its last session ends
    for _ in $(seq 100); do kill -0 "$fake_pid" 2>/dev/null || break; timeout 0.1 tail -f /dev/null; done
    kill "$fake_pid" 2>/dev/null; wait "$fake_pid" 2>/dev/null
    sqlite3 "$d/data/Argus/chymaera/chymaera.db" "SELECT type, name, source_path, size, meta, sha256 FROM captures ORDER BY source_path, sha256" > "$d/rows.txt"
done
diff "$out/cpp-device/answers.txt" "$out/rs-device/answers.txt" > "$out/device-answers.diff"
check "device: the same answers (status, pulls started, the final log lines, a refused path)" $?
check "device: both saw the device and pulled ($(grep -c . "$out/rs-device/rows.txt") rows)" \
    "$(grep -q '"connected":true,"name":"Fakeflip"' "$out/rs-device/answers.txt" && grep -q '0 failed' "$out/rs-device/answers.txt" && test -s "$out/rs-device/rows.txt"; echo $?)"
diff "$out/cpp-device/rows.txt" "$out/rs-device/rows.txt" > "$out/device-rows.diff"
check "device: the same capture rows (kind, name, path, size, meta, sha256)" $?
for side in cpp rs; do
    test -s "$out/$side-device/requests.txt" && ! grep -q 'u2f\|StorageReadRequest .*/assets/' "$out/$side-device/requests.txt"
    check "device: $side never read /ext/u2f or an assets file" $?
done

echo "== chymaera watch with the fake attached"
for side in cpp rs; do
    d=$out/$side-watch
    mkdir -p "$d/home" "$d/data" "$d/run" && chmod 700 "$d/run"
    sessions=1; [ $side = cpp ] && sessions=2
    timeout 60 "$fake" --pty --sessions $sessions > "$d/fake.out" 2> "$d/fake.err" &
    fake_pid=$!
    pty=
    for _ in $(seq 100); do pty=$(sed -n 's/^pty //p' "$d/fake.out" 2>/dev/null); [ -n "$pty" ] && break; timeout 0.1 tail -f /dev/null; done
    if [ $side = cpp ]; then
        test_port=$pty timeout 30 bash -c "$(declare -f run); test_port='$pty' run '$cpp' '$d' chymaera watch 4" > "$d/watch.txt" 2>/dev/null
    else
        timeout 30 bash -c "$(declare -f run); run '$rs' '$d' --port '$pty' chymaera watch 4" > "$d/watch.txt" 2>/dev/null
    fi
    kill "$fake_pid" 2>/dev/null; wait "$fake_pid" 2>/dev/null
    grep -q '^\[device\] Device attached: Fakeflip$' "$d/watch.txt"
    check "watch: $side prints the attached device" $?
done

echo "== selftest"
for side in cpp rs; do
    bin=$cpp; [ $side = rs ] && bin=$rs
    d=$out/$side-selftest
    mkdir -p "$d/home" "$d/data" "$d/run" && chmod 700 "$d/run"
    timeout 60 bash -c "$(declare -f run); run '$bin' '$d' chymaera selftest" > "$d/selftest.txt" 2>&1
    check "$side: chymaera selftest passes ($(grep -c "^  PASS" "$d/selftest.txt") checks)" $?
done

echo "== results in $out"
[ $failures -eq 0 ] && echo "all checks passed" || echo "$failures check(s) failed"
exit $((failures > 0))
