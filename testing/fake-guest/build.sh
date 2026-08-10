#!/usr/bin/env bash
# Build the M0 acceptance guest.
#
# Two steps, both from wasm-tools:
#   1. `component embed` writes the world from wit/ into the core module as a
#      custom section.
#   2. `component new` turns the annotated core module into a real component.
#
# The world in wit/guest.wit imports the same interfaces host.wit declares, so
# a drift between the guest and the published contract fails here rather than
# at server startup.
set -euo pipefail
cd "$(dirname "$0")"

command -v wasm-tools >/dev/null || {
    echo "wasm-tools is required: cargo install wasm-tools" >&2
    exit 1
}

wasm-tools parse guest.wat -o guest-core.wasm
wasm-tools component embed wit guest-core.wasm -o guest-embedded.wasm --world clean:guest/app
wasm-tools component new guest-embedded.wasm -o guest.wasm
wasm-tools validate guest.wasm
rm -f guest-core.wasm guest-embedded.wasm

echo "built guest.wasm"
wasm-tools component wit guest.wasm | head -12
