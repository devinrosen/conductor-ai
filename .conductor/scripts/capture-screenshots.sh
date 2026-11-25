#!/usr/bin/env bash
# capture-screenshots.sh — mobile-ux-audit's screenshots of conductor-web, built from
# this checkout, showing the real repos and runs. When this branch adds a migration the
# real database doesn't have, conductor-web runs on a copy of it (lib-branch-home.sh,
# #3469), so the branch build never migrates the real database.
set -euo pipefail
# shellcheck source=lib-branch-home.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib-branch-home.sh"

PORT=3000
SCREENSHOT_DIR="conductor-web/frontend/e2e/screenshots/$(date +%Y-%m-%d)"
SERVER_PID=""

cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "Stopping conductor-web server (PID $SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  branch_home_cleanup
}
trap cleanup EXIT

if ! branch_home_setup; then
  echo "ERROR: ${BRANCH_HOME_NOTE}"
  jq -cn --arg c "Failed: ${BRANCH_HOME_NOTE}" '{markers: [], context: $c}' \
    | { echo "<<<FLOW_OUTPUT>>>"; cat; echo "<<<END_FLOW_OUTPUT>>>"; }
  exit 1
fi
echo "conductor-web database: ${BRANCH_HOME_NOTE}"

mkdir -p "$SCREENSHOT_DIR"

echo "=== Starting conductor-web on port $PORT ==="
./target/debug/conductor-web &
SERVER_PID=$!

# Wait for server to be ready
echo "Waiting for server..."
for i in $(seq 1 30); do
  if curl -sf "http://localhost:$PORT" >/dev/null 2>&1; then
    echo "Server ready after ${i}s"
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "ERROR: conductor-web exited unexpectedly"
    cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "Failed: conductor-web server exited before becoming ready"}
<<<END_FLOW_OUTPUT>>>
EOF
    exit 1
  fi
  sleep 1
done

if ! curl -sf "http://localhost:$PORT" >/dev/null 2>&1; then
  echo "ERROR: server did not become ready within 30s"
  cat <<'EOF'
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "Failed: conductor-web server did not start within 30s"}
<<<END_FLOW_OUTPUT>>>
EOF
  exit 1
fi

echo "=== Running Playwright screenshot capture ==="
cd conductor-web/frontend
SCREENSHOT_OUTPUT_DIR="../../$SCREENSHOT_DIR" npx playwright test mobile-ux-screenshots --reporter=list 2>&1
PLAYWRIGHT_EXIT=$?
cd ../..

if [ "$PLAYWRIGHT_EXIT" -ne 0 ]; then
  cat <<EOF
<<<FLOW_OUTPUT>>>
{"markers": [], "context": "Playwright screenshot capture failed (exit $PLAYWRIGHT_EXIT). Screenshots dir: $SCREENSHOT_DIR"}
<<<END_FLOW_OUTPUT>>>
EOF
  exit 1
fi

COUNT=$(find "$SCREENSHOT_DIR" -name '*.png' | wc -l | tr -d ' ')
echo "Captured $COUNT screenshots to $SCREENSHOT_DIR"

cat <<EOF
<<<FLOW_OUTPUT>>>
{"markers": ["screenshots_captured"], "context": "Captured $COUNT mobile screenshots to $SCREENSHOT_DIR"}
<<<END_FLOW_OUTPUT>>>
EOF
