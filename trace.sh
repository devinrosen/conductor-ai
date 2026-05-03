#!/bin/sh
# Launch the TUI with debug tracing for the workflow engine, sending logs to conductor.log.
# Useful for diagnosing workflow stalls (e.g. step transitions that never advance).
# Tail the log in another terminal: `tail -f conductor.log`.
set -e

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.conductor/cargo-target}"

LOG_FILE="${CONDUCTOR_LOG_FILE:-$REPO_ROOT/conductor.log}"
RUST_LOG="${RUST_LOG:-runkon_flow=debug,conductor_core::workflow=debug}"
export RUST_LOG

echo "==> RUST_LOG=$RUST_LOG"
echo "==> Logging stderr to $LOG_FILE"
exec "${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/conductor-tui" 2>"$LOG_FILE"
