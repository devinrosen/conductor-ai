#!/usr/bin/env bash
set -euo pipefail

# Fetch PR metadata as JSON
pr_json=$(gh pr view "${PR_NUMBER}" --json title,body,labels,author,milestone,closingIssuesReferences,number,baseRefName,headRefName)

# Fetch PR diff, truncated to avoid overwhelming downstream context
max_diff_chars=50000
pr_diff=$(gh pr diff "${PR_NUMBER}" | head -c "$max_diff_chars")

# Build context string and emit CONDUCTOR_OUTPUT with proper JSON escaping
context=$(printf '## PR Metadata\n```json\n%s\n```\n\n## PR Diff\n```diff\n%s\n```' "$pr_json" "$pr_diff")
output=$(jq -n --arg context "$context" '{"markers": [], "context": $context}')

cat <<EOF
<<<CONDUCTOR_OUTPUT>>>
${output}
<<<END_CONDUCTOR_OUTPUT>>>
EOF
