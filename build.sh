#!/bin/sh
# Build script: build frontend then Rust workspace.
# Run this after pulling main or setting up a new worktree.

set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "==> Building frontend..."
cd "$REPO_ROOT/conductor-web/frontend"
npm install
npm run build

echo "==> Building Rust workspace..."
cd "$REPO_ROOT"
cargo build --workspace

echo "==> Done."
