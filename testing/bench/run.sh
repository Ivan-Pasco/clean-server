#!/usr/bin/env bash
# End-to-end throughput measurement against the §1.8 envelope.
#
# Boots the release binary with the acceptance guest and drives each
# representative route with `oha`. Reports throughput and p99 per route.
#
# This measures the machine it runs on. §1.8's envelope names 4-core reference
# hardware with handlers doing 1-3 DB queries; the acceptance guest does no I/O
# at all, so these numbers are an upper bound on host overhead, not a
# prediction of application throughput. See docs/performance.md.
set -uo pipefail
cd "$(dirname "$0")/../.."

DURATION="${DURATION:-6s}"
CONNECTIONS="${CONNECTIONS:-50}"
PORT="${PORT:-3400}"

command -v oha >/dev/null || {
    echo "oha is required: cargo install oha" >&2
    exit 1
}

GUEST="$(pwd)/testing/fake-guest/guest.wasm"
BRIDGE="$(pwd)/testing/fake-bridge/bridge.wasm"
for artifact in "$GUEST" "$BRIDGE"; do
    [ -f "$artifact" ] || {
        echo "missing $artifact — run the build.sh next to it" >&2
        exit 1
    }
done

BINARY="target/release/clean-server"
[ -x "$BINARY" ] || {
    echo "missing $BINARY — run: cargo build --release" >&2
    exit 1
}

CONFIG="$(mktemp -t clean-server-bench-XXXXXX).toml"
cat > "$CONFIG" <<EOF
[host]
name = "clean-server"
version = "0.0.0"
component-model = "0.3.0"
deployment-mode = "production"

[guest]
name = "acceptance"
wasm = "$GUEST"
world = "server"

[runtime]
# A pool deep enough that checkout is never the bottleneck; the point is to
# measure the request path, not pool contention.
instances-min = 16
instances-max = 128

[bridges]
"clean:fake-bridge/store" = "$BRIDGE"

[server]
listen = "127.0.0.1:$PORT"
# The measurement is of plaintext HTTP; TLS is a separate question.
allow-plaintext = true
EOF

LOG="$(mktemp -t clean-server-bench-log-XXXXXX)"
"$BINARY" "$CONFIG" > "$LOG" 2>&1 &
SERVER_PID=$!

cleanup() {
    kill -TERM "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    rm -f "$CONFIG" "$LOG" /tmp/clean-server-bench-body
}
trap cleanup EXIT

# Wait for the listener rather than sleeping a guessed interval.
for _ in $(seq 1 100); do
    if curl -sf -o /dev/null "http://127.0.0.1:$PORT/" 2>/dev/null; then
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "server exited during startup:" >&2
        cat "$LOG" >&2
        exit 1
    fi
    sleep 0.1
done

curl -sf -o /dev/null "http://127.0.0.1:$PORT/" || {
    echo "server never became ready:" >&2
    cat "$LOG" >&2
    exit 1
}

echo "clean-server end-to-end throughput"
echo "  binary:      $BINARY (release)"
echo "  duration:    $DURATION per route, $CONNECTIONS connections"
echo "  guest:       testing/fake-guest (no I/O — host overhead only)"
echo
printf "%-26s %14s %12s %12s\n" "route" "req/sec" "p50" "p99"
printf -- "%s\n" "----------------------------------------------------------------"

measure() {
    local label="$1" path="$2"
    shift 2
    local out
    out="$(oha -z "$DURATION" -c "$CONNECTIONS" --no-tui "$@" \
        "http://127.0.0.1:$PORT$path" 2>&1)"

    local rps p50 p99 ok
    field() {
        # Squeeze whitespace, drop the leading space, then take field N.
        printf '%s\n' "$out" | grep -- "$1" | head -1 \
            | tr -s ' \t' ' ' | sed 's/^ //' | cut -d' ' -f"$2"
    }
    rps="$(field 'Requests/sec' 2)"
    p50="$(field '50.00% in' 3)"
    p99="$(field '99.00% in' 3)"
    ok="$(field 'Success rate' 3)"

    printf "%-26s %14.0f %9sms %9sms" "$label" "${rps:-0}" "${p50:-?}" "${p99:-?}"
    # A route that did not return 100% success invalidates its own number.
    if [ "${ok:-}" != "100.00%" ]; then
        printf "   (success rate %s)" "${ok:-unknown}"
    fi
    printf "\n"
}

measure "GET / (hello world)"      "/"
measure "GET /users/:id (param)"   "/users/4821"
measure "GET /counter (bridge)"    "/counter"
measure "GET /_health"             "/_health"

head -c 1024 /dev/zero | tr '\0' 'x' > /tmp/clean-server-bench-body
measure "POST /echo (1KB body)"    "/echo" -m POST -D /tmp/clean-server-bench-body

echo
echo "Measured on: $(uname -m) $(uname -s), $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo '?') logical cores"
echo "These numbers describe this machine. See docs/performance.md for what"
echo "they do and do not certify against the §1.8 envelope."
