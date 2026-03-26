#!/usr/bin/env bash
set -euo pipefail

PORT=3000
SCREENSHOT_DIR="conductor-web/frontend/e2e/screenshots/$(date +%Y-%m-%d)"
SERVER_PID=""

cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "Stopping conductor-web server (PID $SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

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
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Failed: conductor-web server exited before becoming ready"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
    exit 1
  fi
  sleep 1
done

if ! curl -sf "http://localhost:$PORT" >/dev/null 2>&1; then
  echo "ERROR: server did not become ready within 30s"
  cat <<'EOF'
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Failed: conductor-web server did not start within 30s"}
<<<END_CONDUCTOR_OUTPUT>>>
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
<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": "Playwright screenshot capture failed (exit $PLAYWRIGHT_EXIT). Screenshots dir: $SCREENSHOT_DIR"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
  exit 1
fi

COUNT=$(find "$SCREENSHOT_DIR" -name '*.png' | wc -l | tr -d ' ')
echo "Captured $COUNT screenshots to $SCREENSHOT_DIR"

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["screenshots_captured"], "context": "Captured $COUNT mobile screenshots to $SCREENSHOT_DIR"}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
