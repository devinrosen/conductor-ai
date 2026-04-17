#!/bin/sh
# Pull latest changes on the current branch, rebuild, and launch the TUI.
set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.conductor/cargo-target}"

echo "==> Pulling latest changes..."
cd "$REPO_ROOT"
git pull

echo "==> Building..."
"$REPO_ROOT/build.sh"

echo "==> Starting TUI..."
exec "${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/conductor-tui"
