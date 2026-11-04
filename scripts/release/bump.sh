#!/usr/bin/env bash
# Version bump for the `release: X.Y.Z` commit.
#
# Usage:
#   scripts/release/bump.sh <version>
#     e.g. scripts/release/bump.sh 0.18.1
#
# Edits `[workspace.package] version` in the root Cargo.toml and `"version"` in
# plugin/.claude-plugin/plugin.json (plugin_tests fails when the two differ), then
# runs `cargo update -w --offline` to refresh Cargo.lock. Then it checks:
#   1. Cargo.lock equals the old lockfile with only the `version` line of each workspace
#      package (a [[package]] with no `source`) changed. Any other change, such as a
#      third-party dependency moving, fails the script.
#   2. Neither Cargo.toml's [workspace.package] nor plugin.json still has the old version.
#
# Refuses to run if the files it edits have uncommitted changes. If a check fails the
# edits are left in place for inspection and the undo command is printed.
# Ported from runkon/runkon's scripts/release/bump.sh.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

usage() {
  echo "usage: $0 <version>   (e.g. 0.18.1)" >&2
  exit 2
}

[[ $# -eq 1 ]] || usage
new=$1
if [[ ! $new =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid version '$new': expected X.Y.Z or X.Y.Z-suffix (e.g. 0.18.1)" >&2
  exit 2
fi

plugin=plugin/.claude-plugin/plugin.json

old=$(awk '
  /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
  in_pkg && /^version = "/ { v = $0; sub(/^version = "/, "", v); sub(/".*$/, "", v); print v; exit }
' Cargo.toml)
[[ -n $old ]] || { echo "error: no version found in [workspace.package] of Cargo.toml" >&2; exit 1; }
if [[ $old == "$new" ]]; then
  echo "error: already at $new" >&2
  exit 1
fi

plugin_old=$(jq -r .version "$plugin")
if [[ $plugin_old != "$old" ]]; then
  echo "error: $plugin is at $plugin_old but Cargo.toml is at $old; fix that first" >&2
  exit 1
fi

files=(Cargo.toml Cargo.lock "$plugin")
if ! git diff --quiet HEAD -- "${files[@]}"; then
  echo "error: uncommitted changes in files this script edits; commit or revert them first:" >&2
  git diff --name-only HEAD -- "${files[@]}" | sed 's/^/  /' >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp Cargo.lock "$tmp/Cargo.lock.orig"
undo="git checkout -- ${files[*]}"

# Cargo.toml: only the [workspace.package] version line.
awk -v old="$old" -v new="$new" '
  /^\[/ { sect = $0 }
  { if (sect == "[workspace.package]" && $0 == "version = \"" old "\"") $0 = "version = \"" new "\""; print }
' Cargo.toml > "$tmp/out"
cat "$tmp/out" > Cargo.toml   # keeps the file's mode
if ! grep -qxF "version = \"$new\"" Cargo.toml; then
  echo "error: failed to rewrite [workspace.package] version in Cargo.toml" >&2
  echo "undo: $undo" >&2
  exit 1
fi

# plugin.json: only the top-level "version" line, keeping the file's formatting.
awk -v old="$old" -v new="$new" '
  !done && $0 ~ "^  \"version\": \"" old "\"" { sub("\"" old "\"", "\"" new "\""); done = 1 }
  { print }
' "$plugin" > "$tmp/out"
cat "$tmp/out" > "$plugin"
if [[ $(jq -r .version "$plugin") != "$new" ]]; then
  echo "error: failed to rewrite \"version\" in $plugin" >&2
  echo "undo: $undo" >&2
  exit 1
fi

if ! cargo update -w --offline > "$tmp/cargo.log" 2>&1; then
  cat "$tmp/cargo.log" >&2
  echo "error: cargo update -w --offline failed (--offline needs the crates.io index cached; any earlier build fills it)" >&2
  echo "undo: $undo" >&2
  exit 1
fi

# Check 1: Cargo.lock == old lockfile with only workspace packages' versions bumped.
awk -v old="$old" -v new="$new" '
  function flush(   i) {
    for (i = 1; i <= n; i++) {
      if (!has_source && buf[i] == "version = \"" old "\"") buf[i] = "version = \"" new "\""
      print buf[i]
    }
    n = 0; has_source = 0
  }
  /^\[\[package\]\]$/ { flush() }
  { buf[++n] = $0; if ($0 ~ /^source = /) has_source = 1 }
  END { flush() }
' "$tmp/Cargo.lock.orig" > "$tmp/Cargo.lock.expected"

if ! diff -u "$tmp/Cargo.lock.expected" Cargo.lock > "$tmp/lock.diff"; then
  cat "$tmp/lock.diff" >&2
  echo "error: Cargo.lock changed beyond the workspace crates' own version lines (diff above, expected -> actual)." >&2
  echo "       Most likely a third-party dependency moved. Packages involved:" >&2
  awk '
    /^[-+ ]name = / { gsub(/^[-+ ]name = "|"$/, ""); last = $0 }
    /^[-+][^-+]/ || /^[-+]$/ { if (last != "" && !(last in seen)) { seen[last] = 1; print "  " last } }
  ' "$tmp/lock.diff" >&2
  echo "undo: $undo" >&2
  exit 1
fi

# Check 2: no stale old version where the bump owns it.
if awk -v old="$old" '/^\[/ { p = ($0 == "[workspace.package]") } p && $0 == "version = \"" old "\""' Cargo.toml | grep -q . \
  || [[ $(jq -r .version "$plugin") == "$old" ]]; then
  echo "error: Cargo.toml [workspace.package] or $plugin still has $old" >&2
  echo "undo: $undo" >&2
  exit 1
fi

echo "bumped $old -> $new"
git diff --stat HEAD -- "${files[@]}"
