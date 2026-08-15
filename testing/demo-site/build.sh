#!/usr/bin/env bash
# Build the demo site guest.
#
# Same two wasm-tools steps as the acceptance guest, against the same
# host.wit (symlinked into wit/deps/http/), so a drift between this guest and
# the published contract fails here rather than at server startup.
set -euo pipefail
cd "$(dirname "$0")"

command -v wasm-tools >/dev/null || {
    echo "wasm-tools is required: cargo install wasm-tools" >&2
    exit 1
}

python3 gen.py

wasm-tools parse site.wat -o site-core.wasm
wasm-tools component embed wit site-core.wasm -o site-embedded.wasm --world clean:demo/site
wasm-tools component new site-embedded.wasm -o site.wasm
wasm-tools validate site.wasm
rm -f site-core.wasm site-embedded.wasm

echo "built site.wasm"
