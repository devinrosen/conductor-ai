#!/usr/bin/env bash
set -uo pipefail

ERRORS=0

# Build frontend if needed (required for conductor-web to compile)
if [ ! -d conductor-web/frontend/dist ]; then
  if command -v bun &>/dev/null; then
    (cd conductor-web/frontend && bun install && bun run build) 2>&1 || true
  fi
fi

# Detect which conductor-* crates have changed files.
# If FEATURE_BASE_BRANCH is set (passed by the workflow), diff against the
# merge-base with that branch so committed changes within the worktree are
# included. Falling back to HEAD only sees uncommitted edits, which means
# scope shrinks the moment the worktree commits — see issue #2777.
BASE="${FEATURE_BASE_BRANCH:-}"
if [ -n "$BASE" ]; then
  DIFF_TARGET=$(git merge-base HEAD "origin/$BASE" 2>/dev/null \
              || git merge-base HEAD "$BASE" 2>/dev/null \
              || echo HEAD)
else
  DIFF_TARGET=HEAD
fi

CHANGED_CRATES=$(git diff --name-only "$DIFF_TARGET" | grep '^conductor-' | cut -d/ -f1 | sort -u)

if [ -z "$CHANGED_CRATES" ]; then
  # No crate-level changes detected — fall back to full workspace (matches CI)
  if [ -d conductor-web/frontend/dist ]; then
    cargo clippy --workspace --all-targets -- -D warnings 2>&1 || ERRORS=1
  else
    echo "Warning: frontend not built, excluding conductor-web from clippy"
    cargo clippy --workspace --all-targets --exclude conductor-web -- -D warnings 2>&1 || ERRORS=1
  fi
else
  # Scope clippy to changed crates only
  for crate in $CHANGED_CRATES; do
    if [ "$crate" = "conductor-web" ] && [ ! -d conductor-web/frontend/dist ]; then
      echo "Warning: frontend not built, skipping conductor-web clippy"
      continue
    fi
    cargo clippy -p "$crate" --all-targets -- -D warnings 2>&1 || ERRORS=1
  done
fi
cargo fmt --all --check 2>&1 || ERRORS=1

# Validate changed or new .wf files
for f in $(git diff --name-only "$DIFF_TARGET" -- '*.wf') $(git ls-files --others --exclude-standard -- '*.wf'); do
  [ -f "$f" ] || continue
  name=$(basename "$f" .wf)
  conductor workflow validate "$name" --path . 2>&1 \
    || cargo run --bin conductor -- workflow validate "$name" --path . 2>&1 \
    || ERRORS=1
done

if [ "$ERRORS" -eq 1 ]; then
  cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": ["has_lint_errors"], "context": "Lint errors found"}
<<<END_FLOW_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "All lint checks passed"}
<<<END_FLOW_OUTPUT>>>
EOF
fi
