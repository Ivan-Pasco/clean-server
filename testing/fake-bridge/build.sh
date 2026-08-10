#!/usr/bin/env bash
# Build the composition-test bridge. Same two-step as the guest: embed the
# world, then turn the annotated core module into a component.
set -euo pipefail
cd "$(dirname "$0")"

command -v wasm-tools >/dev/null || {
    echo "wasm-tools is required: cargo install wasm-tools" >&2
    exit 1
}

wasm-tools parse bridge.wat -o bridge-core.wasm
wasm-tools component embed wit bridge-core.wasm -o bridge-embedded.wasm --world clean:fake-bridge/fake-bridge
wasm-tools component new bridge-embedded.wasm -o bridge.wasm
wasm-tools validate bridge.wasm
rm -f bridge-core.wasm bridge-embedded.wasm

echo "built bridge.wasm"
wasm-tools component wit bridge.wasm | head -10
