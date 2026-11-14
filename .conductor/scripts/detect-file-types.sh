#!/usr/bin/env bash
set -euo pipefail

# BASE_BRANCH, SCOPE and SINCE are injected by the workflow. BASE_BRANCH comes
# from the resolve-pr-base step; there is no silent fallback to a guessed base
# (#2736). An incremental review decides from the commits since the last review.
# shellcheck source=lib-diff-range.sh
source "$(dirname "$0")/lib-diff-range.sh"
resolve_diff_range

# -z gives NUL-separated, unquoted names.
# Command substitution would drop the NULs, so go through a temp file.
diff_tmp=$(mktemp)
trap 'rm -f "$diff_tmp"' EXIT
changed_files_to "$diff_tmp"

# Filter for code files, excluding .conductor/, docs/, .github/, and root-level *.md.
# Shell scripts under .conductor/scripts/ and .conductor/tests/ count as code, so
# the security, error-handling and test-coverage reviewers cover them.
# CI files (.github/workflows/*.yml|yaml, .github/actions/*/action.yml|yaml) are
# not code, but they get a security review (#3335).
#
# Markers:
#   has_code_changes      any code file: turns on all the code reviewers
#   has_security_changes  any code file or CI file: turns on the security reviewer
#                         (the workflow DSL has no OR, so it needs its own marker)
code_files=()
ci_files=()
while IFS= read -r -d '' file; do
  [[ -z "$file" ]] && continue

  # Pipeline shell scripts run gh and git, so they count as code
  # and so do the pipeline test scripts that run them
  if [[ "$file" == .conductor/scripts/*.sh || "$file" == .conductor/tests/*.sh ]]; then
    code_files+=("$file")
    continue
  fi

  # CI workflows and composite actions run with the repo token
  case "$file" in
    .github/workflows/*.yml|.github/workflows/*.yaml|.github/actions/*/action.yml|.github/actions/*/action.yaml)
      ci_files+=("$file")
      continue
      ;;
  esac

  # Exclude specific directories and root-level .md files
  [[ "$file" == .conductor/* ]] && continue
  [[ "$file" == docs/* ]] && continue
  [[ "$file" == .github/* ]] && continue
  [[ "$file" == *.md && "$file" != */* ]] && continue

  # Include only code file extensions
  case "$file" in
    *.rs|*.ts|*.tsx|*.js|*.css|Cargo.toml|Cargo.lock)
      code_files+=("$file")
      ;;
  esac
done < "$diff_tmp"

count=${#code_files[@]}
ci_count=${#ci_files[@]}

# jq escapes every character in the file names; a hand-built JSON string would
# be invalid for names with quotes or backslashes and the engine would drop the
# markers without any error.
emit() {
  echo "<<<FLOW_OUTPUT>>>"
  jq -cn --argjson markers "$1" --arg ctx "$2" '{markers: $markers, context: $ctx}'
  echo "<<<END_FLOW_OUTPUT>>>"
}

if [ "$count" -gt 0 ]; then
  file_list=$(IFS=", "; echo "${code_files[*]}")
  ctx="Found ${count} code file(s) in ${DIFF_DESC}: ${file_list}"
  if [ "$ci_count" -gt 0 ]; then
    ci_list=$(IFS=", "; echo "${ci_files[*]}")
    ctx="${ctx}; ${ci_count} CI file(s): ${ci_list}"
  fi
  emit '["has_code_changes","has_security_changes"]' "$ctx"
elif [ "$ci_count" -gt 0 ]; then
  ci_list=$(IFS=", "; echo "${ci_files[*]}")
  emit '["has_security_changes"]' "No code files in ${DIFF_DESC}; found ${ci_count} CI file(s): ${ci_list}"
else
  emit '[]' "No code or CI files in ${DIFF_DESC}"
fi
