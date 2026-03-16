#!/usr/bin/env bash
set -euo pipefail

git push --force-with-lease origin HEAD
branch=$(git rev-parse --abbrev-ref HEAD)
echo "Pushed branch: $branch"
