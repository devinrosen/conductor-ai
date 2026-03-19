#!/usr/bin/env bash
# Check test coverage against a minimum threshold.
# Requires cargo-llvm-cov: cargo install cargo-llvm-cov
#
# Usage: .conductor/scripts/coverage-check.sh
# Env:   MIN_COVERAGE (default: 80)
#
# Markers emitted:
#   below_threshold — coverage is below the minimum

set -euo pipefail

MIN_COVERAGE="${MIN_COVERAGE:-80}"

if ! command -v cargo-llvm-cov &>/dev/null; then
    echo "cargo-llvm-cov not installed. Install with: cargo install cargo-llvm-cov"
    echo "Skipping coverage check."
    echo "CONDUCTOR_OUTPUT"
    echo "context: coverage check skipped — cargo-llvm-cov not installed"
    echo "markers: []"
    exit 0
fi

echo "Running coverage check (minimum: ${MIN_COVERAGE}%)..."
COVERAGE_OUTPUT=$(cargo llvm-cov --workspace --summary-only 2>&1 || true)
echo "$COVERAGE_OUTPUT"

# Extract the line coverage percentage from the summary
COVERAGE=$(echo "$COVERAGE_OUTPUT" | grep -oP 'TOTAL\s+\S+\s+\S+\s+(\d+\.\d+)' | grep -oP '\d+\.\d+$' || echo "0")

echo "Coverage: ${COVERAGE}%"

if (( $(echo "$COVERAGE < $MIN_COVERAGE" | bc -l 2>/dev/null || echo 1) )); then
    echo "Coverage ${COVERAGE}% is below threshold ${MIN_COVERAGE}%"
    echo "CONDUCTOR_OUTPUT"
    echo "context: coverage ${COVERAGE}% below threshold ${MIN_COVERAGE}%"
    echo "markers: [\"below_threshold\"]"
else
    echo "Coverage ${COVERAGE}% meets threshold ${MIN_COVERAGE}%"
    echo "CONDUCTOR_OUTPUT"
    echo "context: coverage ${COVERAGE}% meets threshold ${MIN_COVERAGE}%"
    echo "markers: []"
fi
