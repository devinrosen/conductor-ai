#!/usr/bin/env bash
# fmt-fix.sh against a scratch one-file crate with the real cargo fmt.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

if ! cargo fmt --version >/dev/null 2>&1; then
  die "rustfmt is required: rustup component add rustfmt"
fi

GOOD='pub fn add(a: i32, b: i32) -> i32 {
    a + b
}'
BAD='pub fn add(a:i32,b:i32)->i32{a+b}'

# new_crate CONTENT — scratch repo with a committed crate and Cargo.lock; cd into it.
new_crate() {
  repo=$(scratch_repo)
  cd "$repo" || die "cd failed"
  printf '[package]\nname = "scratch"\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
  mkdir -p src
  printf '%s\n' "$1" > src/lib.rs
  printf '# lock v1\n' > Cargo.lock
  printf 'target\n' > .gitignore
  git add -A
  git commit -q -m init
}
commits() { git rev-list --count HEAD; }

case_begin "clean code: exit 0, no commit"
new_crate "$GOOD"
before=$(commits)
run_script fmt-fix.sh
assert_exit 0
assert_eq "$before" "$(commits)" "commit count"

case_begin "unformatted code: committed as 'style: cargo fmt'"
new_crate "$BAD"
before=$(commits)
run_script fmt-fix.sh
assert_exit 0
assert_eq "$((before + 1))" "$(commits)" "commit count"
assert_eq "style: cargo fmt" "$(git log -1 --format=%s)" "subject"
assert_eq "" "$(git status --porcelain)" "tree status"

case_begin "uncommitted tracked change: exit 1, names the file, no commit"
new_crate "$GOOD"
printf '%s\n// edit\n' "$GOOD" > src/lib.rs
before=$(commits)
run_script fmt-fix.sh
assert_exit 1
assert_stderr_contains "src/lib.rs"
assert_eq "$before" "$(commits)" "commit count"
assert_eq " M src/lib.rs" "$(git status --porcelain)" "tree status"

case_begin "lockfile only: committed alone as chore(deps)"
new_crate "$GOOD"
printf '# lock v2\n' > Cargo.lock
before=$(commits)
run_script fmt-fix.sh
assert_exit 0
assert_eq "$((before + 1))" "$(commits)" "commit count"
assert_eq "chore(deps): update lockfiles" "$(git log -1 --format=%s)" "subject"
assert_eq "Cargo.lock" "$(git show --name-only --format= HEAD)" "files in commit"

case_begin "untracked file: warns, is not committed, exit 0"
new_crate "$GOOD"
echo junk > stray.txt
before=$(commits)
run_script fmt-fix.sh
assert_exit 0
assert_stderr_contains "stray.txt"
assert_eq "$before" "$(commits)" "commit count"
assert_eq "?? stray.txt" "$(git status --porcelain)" "tree status"

case_begin "parse error: exit 1"
new_crate "pub fn broken( {"
before=$(commits)
run_script fmt-fix.sh
assert_exit 1
assert_eq "$before" "$(commits)" "commit count"

case_begin "parse error with FMT_FIX_ALLOW_FAILURE=1: exit 0, no commit"
new_crate "pub fn broken( {"
before=$(commits)
run_script fmt-fix.sh FMT_FIX_ALLOW_FAILURE=1
assert_exit 0
assert_stderr_contains "leaving it to verify"
assert_eq "$before" "$(commits)" "commit count"

case_begin "FMT_FIX_ALLOW_FAILURE=1 does not relax the tracked-changes check"
new_crate "$GOOD"
printf '%s\n// edit\n' "$GOOD" > src/lib.rs
run_script fmt-fix.sh FMT_FIX_ALLOW_FAILURE=1
assert_exit 1

finish
