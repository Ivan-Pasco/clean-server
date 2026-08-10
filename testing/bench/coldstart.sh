#!/usr/bin/env bash
# Cold-start measurement (§1.8: "sub-100 ms from process spawn to first request
# served with a warm composition cache").
#
# Measures wall-clock from exec to the first successful response, which is what
# an operator actually waits for — a server that has bound its listener but
# cannot yet serve has not started.
set -uo pipefail
cd "$(dirname "$0")/../.."

RUNS="${RUNS:-10}"
PORT="${PORT:-3410}"

GUEST="$(pwd)/testing/fake-guest/guest.wasm"
BRIDGE="$(pwd)/testing/fake-bridge/bridge.wasm"
BINARY="target/release/clean-server"

for artifact in "$GUEST" "$BRIDGE" "$BINARY"; do
    [ -e "$artifact" ] || {
        echo "missing $artifact" >&2
        exit 1
    }
done

command -v python3 >/dev/null || {
    echo "python3 is required for sub-second timing" >&2
    exit 1
}

CONFIG="$(mktemp -t clean-server-cold-XXXXXX).toml"
cat > "$CONFIG" <<EOF
[host]
name = "clean-server"
version = "0.0.0"
component-model = "0.3.0"
deployment-mode = "production"

[guest]
name = "acceptance"
wasm = "$GUEST"
world = "clean:host/server@0.1"

[runtime]
# The documented default shape, not a tuned one: cold start is dominated by
# composition and by pre-filling instances-min.
instances-min = 8
instances-max = 128

[bridges]
"clean:fake-bridge/store" = "$BRIDGE"

[server]
listen = "127.0.0.1:$PORT"
allow-plaintext = true
EOF

trap 'rm -f "$CONFIG"' EXIT

echo "clean-server cold start"
echo "  §1.8 target: under 100 ms, spawn to first request served"
echo "  runs:        $RUNS"
echo

total=0
worst=0
best=999999

for run in $(seq 1 "$RUNS"); do
    # time_ns, not perf_counter_ns: perf_counter's origin is per-process, so
    # comparing it across two separate python3 invocations is meaningless (it
    # produced negative durations). time_ns is epoch-based and comparable.
    start_ns="$(python3 -c 'import time; print(time.time_ns())')"

    "$BINARY" "$CONFIG" > /dev/null 2>&1 &
    pid=$!

    # Poll as fast as the machine allows; the loop's own cost is included in
    # the figure, which keeps it honest rather than flattering.
    while ! curl -sf -o /dev/null "http://127.0.0.1:$PORT/" 2>/dev/null; do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "  run $run: server exited before serving" >&2
            break
        fi
    done

    end_ns="$(python3 -c 'import time; print(time.time_ns())')"

    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null

    us=$(( (end_ns - start_ns) / 1000 ))
    total=$(( total + us ))
    [ "$us" -gt "$worst" ] && worst=$us
    [ "$us" -lt "$best" ] && best=$us
    printf "  run %-3s %8s us  (%s ms)\n" "$run" "$us" "$(( us / 1000 ))"
done

average=$(( total / RUNS ))
echo
printf "  best %s us   average %s us   worst %s us\n" "$best" "$average" "$worst"
printf "  (worst = %s ms against a 100 ms target)\n" "$(( worst / 1000 ))"
if [ "$(( worst / 1000 ))" -lt 100 ]; then
    echo "  every run under the 100 ms target"
else
    echo "  WARNING: at least one run exceeded the 100 ms target"
fi
