#!/usr/bin/env bash
set -uo pipefail

ERRORS=0

# Build frontend if needed (required for conductor-web to compile)
if [ ! -d conductor-web/frontend/dist ]; then
  if command -v bun &>/dev/null; then
    (cd conductor-web/frontend && bun install && bun run build) 2>&1 || true
  fi
fi

# Detect which conductor-* crates have changed files
CHANGED_CRATES=$(git diff --name-only HEAD | grep '^conductor-' | cut -d/ -f1 | sort -u)

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
for f in $(git diff --name-only HEAD -- '*.wf') $(git ls-files --others --exclude-standard -- '*.wf'); do
  [ -f "$f" ] || continue
  name=$(basename "$f" .wf)
  conductor workflow validate "$name" --path . 2>&1 \
    || cargo run --bin conductor -- workflow validate "$name" --path . 2>&1 \
    || ERRORS=1
done

if [ "$ERRORS" -eq 1 ]; then
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_lint_errors"], "context": "Lint errors found"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
else
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "All lint checks passed"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
fi
