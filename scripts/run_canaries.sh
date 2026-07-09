#!/usr/bin/env bash
# run_canaries.sh — Layer 2 canary orchestrator for clean-server.
#
# For each canary compatible with the server host:
#   1. Locate the canary's compiled .wasm (built by the compiler CI, either
#      unpacked locally or downloaded from a compiler release).
#   2. Parse the canary's `.cln` source header for the `// Expected output:`
#      block.
#   3. Invoke `canary_runner <wasm>` and capture its stdout.
#   4. Diff captured stdout against the expected block.
#
# On any failure (LinkError at instantiation, runtime trap, stdout diff), emit
# a single-line JSON record on stderr and mark the overall run as failed. Exit
# 0 iff every applicable canary passed; non-zero otherwise.
#
# Usage:
#   scripts/run_canaries.sh --cln-dir <dir> --wasm-dir <dir> [--runner <bin>]
#     [--only <name[,name...]>] [--verbose]
#
# Required flags:
#   --cln-dir   Directory containing canary .cln sources (headers).
#   --wasm-dir  Directory containing pre-compiled canary .wasm files.
#
# Optional flags:
#   --runner    Path to canary_runner binary. Defaults to
#               `target/release/canary_runner` if present, otherwise
#               `target/debug/canary_runner`.
#   --only      Comma-separated canary names to run (without extension).
#   --verbose   Show per-canary pass line even on success.
#
# The server host runs every canary except the browser-only ones (ui, canvas,
# api) per the L2 umbrella prompt (see
# https://errors.cleanlanguage.dev/prompts/a935f7cb-7b26-11f1-9586-da25a95a496b).

set -uo pipefail

CLN_DIR=""
WASM_DIR=""
RUNNER=""
ONLY=""
VERBOSE=0

# Canaries excluded from the server host (browser-only per umbrella prompt).
EXCLUDE=("api" "canvas" "ui")

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cln-dir)  CLN_DIR="$2"; shift 2 ;;
    --wasm-dir) WASM_DIR="$2"; shift 2 ;;
    --runner)   RUNNER="$2"; shift 2 ;;
    --only)     ONLY="$2"; shift 2 ;;
    --verbose|-v) VERBOSE=1; shift ;;
    -h|--help)
      sed -n '2,32p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$CLN_DIR" || -z "$WASM_DIR" ]]; then
  echo "Error: --cln-dir and --wasm-dir are required." >&2
  exit 2
fi

if [[ ! -d "$CLN_DIR" ]]; then
  echo "Error: cln-dir not found: $CLN_DIR" >&2
  exit 2
fi

if [[ ! -d "$WASM_DIR" ]]; then
  echo "Error: wasm-dir not found: $WASM_DIR" >&2
  exit 2
fi

if [[ -z "$RUNNER" ]]; then
  if [[ -x "target/release/canary_runner" ]]; then
    RUNNER="target/release/canary_runner"
  elif [[ -x "target/debug/canary_runner" ]]; then
    RUNNER="target/debug/canary_runner"
  else
    echo "Error: canary_runner binary not found. Build with:" >&2
    echo "  cargo build --release --bin canary_runner" >&2
    exit 2
  fi
fi

if [[ ! -x "$RUNNER" ]]; then
  echo "Error: runner not executable: $RUNNER" >&2
  exit 2
fi

is_excluded() {
  local name="$1"
  for x in "${EXCLUDE[@]}"; do
    [[ "$name" == "$x" ]] && return 0
  done
  return 1
}

is_selected() {
  local name="$1"
  [[ -z "$ONLY" ]] && return 0
  local IFS=,
  for pick in $ONLY; do
    [[ "$name" == "$pick" ]] && return 0
  done
  return 1
}

# Extract the `// Expected output:` block from a canary source file.
# Prints one expected line per stdout line. Empty output = no header found.
# Rule: lines matching `//` + two-or-more spaces/tabs + text belong to the
# block; blank `//` lines are treated as empty output lines; any other line
# terminates the block.
extract_expected() {
  local cln_file="$1"
  awk '
    /^\/\/ Expected output:/ { in_block = 1; next }
    in_block == 1 {
      if ($0 ~ /^\/\/[ \t][ \t]+/) {
        line = $0
        sub(/^\/\/[ \t][ \t]+/, "", line)
        print line
      } else if ($0 ~ /^\/\/[ \t]*$/) {
        print ""
      } else {
        exit
      }
    }
  ' "$cln_file"
}

json_escape() {
  # Escape backslash, double quote, newline, tab, carriage return for JSON.
  python3 -c '
import json, sys
sys.stdout.write(json.dumps(sys.stdin.read()))
' 2>/dev/null || {
    # Fallback: crude perl escape if python3 isn't available.
    perl -e '
      undef $/;
      $_ = <STDIN>;
      s/\\/\\\\/g; s/"/\\"/g; s/\n/\\n/g; s/\r/\\r/g; s/\t/\\t/g;
      print "\"$_\"";
    '
  }
}

PASS=0
FAIL=0
SKIP=0
FAIL_NAMES=()

for cln in "$CLN_DIR"/*.cln; do
  [[ -e "$cln" ]] || continue
  name="$(basename "$cln" .cln)"

  if ! is_selected "$name"; then
    continue
  fi

  if is_excluded "$name"; then
    SKIP=$((SKIP + 1))
    [[ $VERBOSE -eq 1 ]] && echo "SKIP  $name (browser-only)"
    continue
  fi

  wasm="$WASM_DIR/$name.wasm"
  if [[ ! -f "$wasm" ]]; then
    FAIL=$((FAIL + 1))
    FAIL_NAMES+=("$name")
    printf '{"canary":"%s","status":"missing_wasm","expected_wasm":"%s"}\n' \
      "$name" "$wasm" >&2
    continue
  fi

  expected="$(extract_expected "$cln")"
  if [[ -z "$expected" ]]; then
    FAIL=$((FAIL + 1))
    FAIL_NAMES+=("$name")
    printf '{"canary":"%s","status":"no_expected_output","cln":"%s"}\n' \
      "$name" "$cln" >&2
    continue
  fi

  # Run the canary. Capture stdout, stderr, and exit code independently.
  set +e
  actual="$("$RUNNER" "$wasm" 2>/tmp/canary_stderr_$$)"
  exit_code=$?
  runner_stderr="$(cat /tmp/canary_stderr_$$ 2>/dev/null || true)"
  rm -f /tmp/canary_stderr_$$
  set -e

  if [[ $exit_code -ne 0 ]]; then
    FAIL=$((FAIL + 1))
    FAIL_NAMES+=("$name")
    esc_stderr="$(printf '%s' "$runner_stderr" | json_escape)"
    printf '{"canary":"%s","status":"runner_error","exit_code":%d,"stderr":%s}\n' \
      "$name" "$exit_code" "$esc_stderr" >&2
    continue
  fi

  if [[ "$actual" != "$expected" ]]; then
    FAIL=$((FAIL + 1))
    FAIL_NAMES+=("$name")
    esc_expected="$(printf '%s' "$expected" | json_escape)"
    esc_actual="$(printf '%s' "$actual" | json_escape)"
    printf '{"canary":"%s","status":"diff","expected":%s,"actual":%s}\n' \
      "$name" "$esc_expected" "$esc_actual" >&2
    continue
  fi

  PASS=$((PASS + 1))
  [[ $VERBOSE -eq 1 ]] && echo "PASS  $name"
done

echo
echo "Canary summary: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
if [[ $FAIL -gt 0 ]]; then
  echo "Failed canaries: ${FAIL_NAMES[*]}" >&2
  exit 1
fi
exit 0
