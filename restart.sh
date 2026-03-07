#!/bin/sh
# Pull latest main, rebuild, and launch the TUI.
set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "==> Pulling latest main..."
cd "$REPO_ROOT"
git pull

echo "==> Building..."
"$REPO_ROOT/build.sh"

echo "==> Starting TUI..."
cargo run --bin conductor-tui
