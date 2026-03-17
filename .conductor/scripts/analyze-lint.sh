#!/usr/bin/env bash
set -uo pipefail

ERRORS=0

cargo clippy --workspace --all-targets --exclude conductor-web -- -D warnings 2>&1 || ERRORS=1
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
