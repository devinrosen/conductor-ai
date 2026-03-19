#!/usr/bin/env bash
# Run cargo-audit to check for known vulnerabilities in dependencies.
# Install: cargo install cargo-audit
#
# Usage: .conductor/scripts/cargo-audit.sh
# Exit code 0 = clean, non-zero = vulnerabilities found
#
# Markers emitted:
#   has_vulnerabilities — at least one advisory found

set -euo pipefail

if ! command -v cargo-audit &>/dev/null; then
    echo "cargo-audit not installed. Install with: cargo install cargo-audit"
    echo "{{failure}}"
    exit 1
fi

echo "Running cargo audit..."
if cargo audit 2>&1; then
    echo "No vulnerabilities found."
    echo "CONDUCTOR_OUTPUT"
    echo "context: clean audit — no advisories"
    echo "markers: []"
else
    echo "Vulnerabilities found!"
    echo "CONDUCTOR_OUTPUT"
    echo "context: cargo audit found advisories"
    echo "markers: [\"has_vulnerabilities\"]"
fi
