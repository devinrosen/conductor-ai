#!/usr/bin/env bash
# Steps after the `release/X.Y.Z` -> `main` release PR merges.
#
# Usage:
#   scripts/release/finish.sh [--dry-run] <version>
#     e.g. scripts/release/finish.sh 0.21.1
#
# Run it from the main checkout, on `main`, with no uncommitted changes to tracked files.
# Preflight (before any side effect): gh and jq are installed, gh is authenticated, the
# checkout is on main and clean. Then `git fetch --prune --tags origin`.
#
#   1. Verify the release: find the `release: X.Y.Z` commit on origin/main, wait for the
#      Release workflow run on it, then check that tag vX.Y.Z on origin points at that
#      commit and that the GitHub release vX.Y.Z exists.
#   2. Close the X.Y.Z milestone (left open, and reported, if it still has open issues).
#   3. For every later `release/*` branch with no `v` tag, fast-forward it to main when it
#      has no commits of its own (a plain, non-force push). Otherwise print that main needs
#      merging in; this script never merges or force-pushes.
#   4. `git pull --ff-only origin main`, then `./install-cli.sh`.
#   5. Print what is left by hand: `claude plugin update conductor@conductor-ai`, then
#      restart Claude Code or run `/mcp`, plus anything steps 1-4 could not do.
#
# --dry-run runs the read-only checks (including the wait for the Release workflow) and
# prints each action it would take instead of taking it.
#
# Idempotent: a re-run skips the wait when the tag and release exist, skips a closed
# milestone and branches already at main. Exits 1 when something is left over.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

usage() {
  echo "usage: $0 [--dry-run] <version>   (e.g. 0.21.1)" >&2
  exit 2
}

dry=0
v=
for arg in "$@"; do
  case $arg in
    --dry-run) dry=1 ;;
    -*) usage ;;
    *) [[ -z $v ]] || usage; v=$arg ;;
  esac
done
[[ -n $v ]] || usage
if [[ ! $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "invalid version '$v': expected X.Y.Z (e.g. 0.21.1)" >&2
  exit 2
fi
tag="v$v"

die() { echo "error: $*" >&2; exit 1; }
say() { echo "==> $*"; }

leftovers=
leftover() { leftovers="${leftovers}${*}"$'\n'; }

# --- Preflight -------------------------------------------------------------
for tool in gh jq; do
  command -v "$tool" >/dev/null || die "$tool not found on PATH"
done
gh auth status >/dev/null 2>&1 || die "gh is not authenticated (run: gh auth login)"
branch=$(git rev-parse --abbrev-ref HEAD)
[[ $branch == main ]] || die "on '$branch'; run this from the checkout on main"
git diff --quiet HEAD || die "uncommitted changes to tracked files; commit or revert them first"
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
git fetch --prune --tags origin

# --- 1. Verify the release -------------------------------------------------
say "Step 1: verify release $tag"
sha=$(git log origin/main --format='%H %s' | awk -v s="release: $v" '{h=$1; sub(/^[^ ]+ /, "")} $0 == s {print h; exit}')
[[ -n $sha ]] || die "no 'release: $v' commit on origin/main; has the release PR merged?"
echo "release commit: $sha"

tag_sha() { git ls-remote --tags origin "refs/tags/$tag^{}" | awk '{print $1}'; }

if [[ $(tag_sha) == "$sha" ]] && gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  echo "already released: tag and GitHub release exist"
else
  run_id=
  for _ in $(seq 1 30); do
    run_id=$(gh run list --repo "$repo" --workflow release.yml --limit 30 \
      --json databaseId,headSha,createdAt \
      | jq -r --arg sha "$sha" '[.[] | select(.headSha == $sha)] | sort_by(.createdAt) | last | .databaseId // empty')
    [[ -n $run_id ]] && break
    echo "waiting for the Release workflow run on ${sha:0:8}..."
    sleep 10
  done
  [[ -n $run_id ]] || die "no Release workflow run for $sha; see https://github.com/$repo/actions/workflows/release.yml"
  if ! gh run watch "$run_id" --repo "$repo" --exit-status; then
    die "Release workflow run $run_id failed; see: gh run view $run_id --repo $repo --log-failed"
  fi
  git fetch --prune --tags origin
  found=$(tag_sha)
  [[ -n $found ]] || die "tag $tag not found on origin after the Release workflow finished"
  [[ $found == "$sha" ]] || die "tag $tag points at $found, not the release commit $sha"
fi
url=$(gh release view "$tag" --repo "$repo" --json url -q .url) || die "no GitHub release $tag"
echo "tag $tag -> ${sha:0:8}; release: $url"

# --- 2. Close the milestone ------------------------------------------------
say "Step 2: close milestone $v"
ms=$(gh api "repos/$repo/milestones?state=all&per_page=100" --paginate \
  | jq -c --arg t "$v" '[.[] | select(.title == $t)] | first // empty')
if [[ -z $ms ]]; then
  echo "warning: no milestone titled $v"
  leftover "no milestone titled $v to close"
else
  ms_state=$(jq -r .state <<<"$ms")
  ms_num=$(jq -r .number <<<"$ms")
  ms_open=$(jq -r .open_issues <<<"$ms")
  if [[ $ms_state == closed ]]; then
    echo "milestone $v is already closed"
  elif [[ $ms_open -gt 0 ]]; then
    echo "milestone $v still has $ms_open open issue(s); not closing it:"
    gh issue list --repo "$repo" --milestone "$v" --state open
    leftover "milestone $v has $ms_open open issue(s); move them, then close it"
  elif [[ $dry == 1 ]]; then
    echo "[dry-run] would close milestone $v (#$ms_num)"
  else
    gh api -X PATCH "repos/$repo/milestones/$ms_num" -f state=closed >/dev/null
    echo "closed milestone $v"
  fi
fi

# --- 3. Other open release branches ----------------------------------------
say "Step 3: other open release branches"
main_sha=$(git rev-parse origin/main)
found_any=0
for b in $(git for-each-ref --format='%(refname:strip=3)' 'refs/remotes/origin/release/*'); do
  [[ $b != "$v" ]] || continue
  [[ $(printf '%s\n%s\n' "$v" "$b" | sort -V | tail -n1) == "$b" ]] || continue
  if git rev-parse -q --verify "refs/tags/v$b" >/dev/null; then continue; fi
  found_any=1
  ref=origin/release/$b
  if git merge-base --is-ancestor origin/main "$ref"; then
    echo "release/$b: already contains main"
  elif git merge-base --is-ancestor "$ref" origin/main; then
    n=$(git rev-list --count "$ref..origin/main")
    if [[ $dry == 1 ]]; then
      echo "[dry-run] would fast-forward release/$b by $n commit(s)"
    elif err=$(git push origin "$main_sha:refs/heads/release/$b" 2>&1); then
      echo "release/$b: fast-forwarded $n commit(s)"
    else
      echo "$err" >&2
      echo "release/$b: fast-forward push rejected"
      leftover "release/$b: fast-forward push failed; merge main into it by hand"
    fi
  else
    own=$(git rev-list --count --cherry-pick --right-only "origin/main...$ref")
    behind=$(git rev-list --count "$ref..origin/main")
    if [[ $own -eq 0 ]]; then
      echo "release/$b: its commits are already on main but it cannot be fast-forwarded ($behind behind); main still needs merging in"
    else
      echo "release/$b: $own commit(s) of its own, $behind behind main; main needs merging in"
    fi
    echo "  git switch release/$b && git pull && git merge origin/main && git push"
    leftover "release/$b: merge main into it (git switch release/$b && git pull && git merge origin/main && git push)"
  fi
done
[[ $found_any == 1 ]] || echo "no other open release branches"

# --- 4. Update and install -------------------------------------------------
say "Step 4: pull main and install"
if [[ $dry == 1 ]]; then
  echo "[dry-run] would run: git pull --ff-only origin main"
  echo "[dry-run] would run: ./install-cli.sh"
else
  git pull --ff-only origin main
  ./install-cli.sh
fi

# --- 5. What's left --------------------------------------------------------
say "Remaining manual steps"
echo "  claude plugin update conductor@conductor-ai"
echo "  then restart Claude Code, or run /mcp to reconnect the conductor MCP server"
if [[ -n $leftovers ]]; then
  echo
  echo "Left over from this run:"
  printf '%s' "$leftovers" | sed 's/^/  - /'
  exit 1
fi
