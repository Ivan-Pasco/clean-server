#!/usr/bin/env bash
# scripts/install_hooks.sh — install repo git hooks into .git/hooks/.
#
# Idempotent. Safe to re-run. Overwrites existing hooks with the same name
# only if they are the ones this script previously installed (identified by
# a marker line). Refuses to overwrite hand-written hooks.
#
# See system-documents/TEST_STRATEGY.md § 3.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOK_SRC="$REPO_ROOT/scripts/hooks"
HOOK_DST="$REPO_ROOT/.git/hooks"

MARKER="# scripts/hooks/"

if [[ ! -d "$HOOK_DST" ]]; then
  echo "install_hooks: .git/hooks directory not found; is this a git checkout?" >&2
  exit 1
fi

install_one() {
  local name="$1"
  local src="$HOOK_SRC/$name"
  local dst="$HOOK_DST/$name"

  if [[ ! -f "$src" ]]; then
    echo "install_hooks: source hook not found: $src" >&2
    exit 1
  fi

  if [[ -e "$dst" ]] && ! grep -q "$MARKER" "$dst"; then
    echo "install_hooks: refusing to overwrite existing $name (no marker line)."
    echo "               Remove $dst manually if you want to replace it." >&2
    return 0
  fi

  install -m 0755 "$src" "$dst"
  echo "install_hooks: installed $name"
}

install_one pre-commit
install_one pre-push

echo
echo "Hooks installed. Bypass with --no-verify only in emergencies."
echo "See system-documents/TEST_STRATEGY.md for tier definitions."
