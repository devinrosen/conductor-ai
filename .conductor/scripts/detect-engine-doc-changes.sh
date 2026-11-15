#!/usr/bin/env bash
set -euo pipefail

# Turns on the Doc Accuracy reviewer (#3336) when the diff touches a doc that
# states engine or workflow rules: docs/workflow/** or the Workflow Semantics
# brief. Also resolves the locked runkon-flow version and the cargo registry
# source directory that holds it, so the reviewer checks rules against the
# engine conductor builds with. Both are exported as runkon_version and
# runkon_src on every path (empty when nothing matched or no source was found):
# exported fields persist across iterations, so a stale value must not survive.
#
# BASE_BRANCH, SCOPE and SINCE are injected by the workflow (see
# detect-workflow-changes.sh).
# shellcheck source=lib-diff-range.sh
source "$(dirname "$0")/lib-diff-range.sh"
resolve_diff_range

diff_tmp=$(mktemp)
trap 'rm -f "$diff_tmp"' EXIT
changed_files_to "$diff_tmp"

doc_files=()
while IFS= read -r -d '' file; do
  [[ -z "$file" ]] && continue
  case "$file" in
    docs/workflow/*|.conductor/agents/review-workflow-semantics.md)
      doc_files+=("$file")
      ;;
  esac
done < "$diff_tmp"

count=${#doc_files[@]}

emit() { # MARKERS_JSON CONTEXT VERSION SRC
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" --arg ver "$3" --arg src "$4" \
    '{markers: $markers, context: $ctx, runkon_version: $ver, runkon_src: $src}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

if [ "$count" -eq 0 ]; then
  emit '[]' "No engine-semantics docs in ${DIFF_DESC}" "" ""
  exit 0
fi

# The exact resolved version, from the lock file of the branch under review.
version=""
if [ -f Cargo.lock ]; then
  version=$(awk '
    $0 == "name = \"runkon-flow\"" { found = 1; next }
    found && /^version = / { gsub(/version = |"/, ""); print; exit }
    /^\[\[package\]\]/ { found = 0 }' Cargo.lock)
  # Cargo.lock comes from the PR branch: accept only a semver string, so it
  # can't redirect the registry glob or inject text into the reviewer prompt.
  if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$'; then
    [ -z "$version" ] || echo "ignoring malformed runkon-flow version in Cargo.lock" >&2
    version=""
  fi
fi

find_src() { # prints the registry src/<index> dir holding runkon-flow-$version
  local d
  for d in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/runkon-flow-"$version"; do
    if [ -d "$d" ]; then
      dirname "$d"
      return 0
    fi
  done
  return 1
}

src=""
if [ -n "$version" ]; then
  src=$(find_src || true)
  if [ -z "$src" ]; then
    cargo fetch --locked --quiet >&2 || true
    src=$(find_src || true)
  fi
fi

if [ -z "$version" ]; then
  echo "detect-engine-doc-changes: no runkon-flow entry in Cargo.lock" >&2
elif [ -z "$src" ]; then
  echo "detect-engine-doc-changes: runkon-flow-${version} source not found in the cargo registry" >&2
fi

file_list=$(IFS=", "; echo "${doc_files[*]}")
emit '["has_engine_doc_changes"]' \
  "Found ${count} engine-semantics doc(s) in ${DIFF_DESC}: ${file_list}; runkon-flow ${version:-unknown}, source ${src:-not found}" \
  "$version" "$src"
