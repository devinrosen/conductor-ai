#!/bin/sh
# Install the conductor CLI from this checkout into ~/.cargo/bin.
#
# That binary is also the conductor MCP server (`conductor mcp serve`), so run
# this after pulling, then reconnect the MCP server (/mcp in Claude Code).
# It works even when the installed binary is too old to have
# `conductor dev reinstall`.
set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
# Same target dir as build.sh, so the install reuses its build cache.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.conductor/cargo-target}"

echo "==> Installing conductor CLI (and MCP server) from $REPO_ROOT..."
cd "$REPO_ROOT"
cargo install --path conductor-cli --locked --force

echo "==> Installed: $("${CARGO_HOME:-$HOME/.cargo}/bin/conductor" --version)"
echo "==> Reconnect the conductor MCP server (/mcp in Claude Code) to use it."
