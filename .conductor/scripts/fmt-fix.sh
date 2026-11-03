#!/usr/bin/env bash
set -euo pipefail

# Runs `cargo fmt --all` and commits only what rustfmt changed.
#
# Every step that runs before this script (implement, implement-ticket,
# address-reviews, lint-fix-impl) commits its own work. A dirty tree here means
# that step didn't finish, so fail and name the files rather than commit them
# under a formatting message (#3178).
#
# FMT_FIX_ALLOW_FAILURE=1: used before verify. If rustfmt can't format the code
# (usually a parse error), leave it for verify to report instead of failing the
# run. It does not relax the dirty-tree check.

dirty=$(git status --porcelain --untracked-files=all)
if [ -n "${dirty}" ]; then
  {
    echo "fmt-fix: the working tree has uncommitted changes before cargo fmt ran:"
    echo "${dirty}" | sed 's/^/  /'
    echo "The previous step should have committed these, so it probably ended early."
    echo "Nothing was committed. Resume the run from that step, or commit or discard"
    echo "the changes, then resume from this one."
  } >&2
  exit 1
fi

if ! cargo fmt --all; then
  if [ -n "${FMT_FIX_ALLOW_FAILURE:-}" ]; then
    echo "cargo fmt failed (likely a parse error); leaving it to verify" >&2
    exit 0
  fi
  exit 1
fi

# The tree was clean before rustfmt ran, so every change is rustfmt's. rustfmt
# only rewrites tracked files, so stage those and nothing else.
if ! git diff --quiet; then
  git add -u
  git commit -m "style: cargo fmt"
fi
