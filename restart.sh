#!/bin/sh
# Pull latest changes on the current branch, rebuild, and launch the TUI.
# This doesn't touch the installed CLI in ~/.cargo/bin, which is also the
# conductor MCP server; run ./install-cli.sh for that. It warns below when stale.
set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.conductor/cargo-target}"

echo "==> Pulling latest changes..."
cd "$REPO_ROOT"
git pull

echo "==> Building..."
"$REPO_ROOT/build.sh"

# Warn when the installed CLI (the MCP server) wasn't built from HEAD.
# `conductor --version` prints "conductor <version> (<short sha> <commit time>)"
# since 0.17.1; older builds print no sha and always count as stale.
INSTALLED_CLI="${CARGO_HOME:-$HOME/.cargo}/bin/conductor"
INSTALLED_VERSION="not installed"
INSTALLED_SHA=""
if [ -x "$INSTALLED_CLI" ]; then
  INSTALLED_VERSION="$("$INSTALLED_CLI" --version 2>/dev/null || echo unknown)"
  INSTALLED_SHA="$(echo "$INSTALLED_VERSION" | sed -n 's/^conductor [^ ]* (\([0-9a-f][0-9a-f]*\) .*/\1/p')"
fi
HEAD_SHA="$(git rev-parse HEAD)"
case "$HEAD_SHA" in
  "${INSTALLED_SHA:-none}"*)
    echo "==> Installed CLI / MCP server is up to date: $INSTALLED_VERSION"
    ;;
  *)
    echo "==> Warning: the installed CLI / MCP server ($INSTALLED_VERSION) isn't built from HEAD."
    echo "    Run ./install-cli.sh, then reconnect the MCP server (/mcp in Claude Code)."
    ;;
esac

echo "==> Starting TUI..."
exec "${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/conductor-tui"
