#!/usr/bin/env bash
set -euo pipefail

# FMT_FIX_ALLOW_FAILURE=1: used before verify. If rustfmt can't format the code
# (usually a parse error), leave it for verify to report instead of failing the run.
if ! cargo fmt --all; then
  if [ -n "${FMT_FIX_ALLOW_FAILURE:-}" ]; then
    echo "cargo fmt failed (likely a parse error); leaving it to verify" >&2
    exit 0
  fi
  exit 1
fi
if ! git diff --quiet; then
  git add -A
  git commit -m "style: cargo fmt"
fi
