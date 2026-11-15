#!/usr/bin/env bash
# scripts/release/finish.sh's pull-and-re-exec (#3381), against a scratch origin and clone.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

command -v jq >/dev/null 2>&1 || die "jq is required"

FINISH="$REPO_ROOT/scripts/release/finish.sh"
NEW_MARKER='NEW finish.sh args=$* reexec=${FINISH_SH_REEXEC:-}'

# gh stub: auth status succeeds, repo view prints a name, everything else fails.
STUB=$(scratch_dir)
cat > "$STUB/gh" <<'STUBEOF'
#!/bin/sh
case "$1 $2" in
  "auth status") exit 0 ;;
  "repo view") echo owner/scratch; exit 0 ;;
esac
exit 1
STUBEOF
chmod +x "$STUB/gh"

# new_clone — bare origin plus a clone on main holding finish.sh; cd into the clone.
new_clone() {
  local base
  base=$(scratch_dir)
  git init -q --bare "$base/origin.git" >/dev/null 2>&1 || die "git init --bare failed"
  git -C "$base/origin.git" symbolic-ref HEAD refs/heads/main
  git clone -q "$base/origin.git" "$base/clone" >/dev/null 2>&1 || die "git clone failed"
  clone="$base/clone"
  git -C "$clone" symbolic-ref HEAD refs/heads/main
  git -C "$clone" config commit.gpgsign false
  git -C "$clone" config core.hooksPath /dev/null
  mkdir -p "$clone/scripts/release"
  cp "$FINISH" "$clone/scripts/release/finish.sh"
  git -C "$clone" add -A
  git -C "$clone" commit -q -m init
  git -C "$clone" push -q origin main >/dev/null 2>&1 || die "git push failed"
  cd "$clone" || die "cd failed"
}

# push_to_origin PATH CONTENT MSG — commit a change to origin/main through the clone,
# then rewind the clone so it is behind origin.
push_to_origin() {
  local keep
  keep=$(git -C "$clone" rev-parse HEAD)
  commit_file "$clone" "$1" "$2" "$3"
  git -C "$clone" push -q origin main >/dev/null 2>&1 || die "git push failed"
  git -C "$clone" reset -q --hard "$keep"
}

new_script() { printf '#!/usr/bin/env bash\necho "%s"\n' "$NEW_MARKER"; }

run_finish() {
  local errf
  errf=$(mktemp "${TMPDIR:-/tmp}/pipeline-test-err.XXXXXX")
  STDOUT=$(PATH="$STUB:$PATH" bash "$clone/scripts/release/finish.sh" "$@" 2>"$errf")
  EXIT=$?
  STDERR=$(cat "$errf")
  rm -f "$errf"
}

unset FINISH_SH_REEXEC

case_begin "changed on origin: pulls, then re-execs the new copy with the same args"
new_clone
push_to_origin scripts/release/finish.sh "$(new_script)" "change finish.sh"
run_finish 0.9.9
assert_exit 0
assert_stdout_contains "finish.sh changed in the pull"
assert_stdout_contains "NEW finish.sh args=0.9.9 reexec=1"
assert_eq "$(git rev-parse origin/main)" "$(git rev-parse HEAD)" "HEAD after pull"

case_begin "unchanged: pulls, no re-exec, continues to step 1"
new_clone
push_to_origin other.txt "x" "unrelated"
run_finish 0.9.9
assert_exit 1
assert_stdout_not_contains "finish.sh changed in the pull"
assert_stdout_contains "Step 1"
assert_stderr_contains "no 'release: 0.9.9' commit"
assert_eq "$(git rev-parse origin/main)" "$(git rev-parse HEAD)" "HEAD after pull"

case_begin "--dry-run: reports the re-exec, doesn't pull or exec"
new_clone
push_to_origin scripts/release/finish.sh "$(new_script)" "change finish.sh"
head=$(git rev-parse HEAD)
run_finish --dry-run 0.9.9
assert_stdout_contains "would run: git pull --ff-only origin main"
assert_stdout_contains "would re-exec the updated script"
assert_stdout_not_contains "NEW finish.sh"
assert_eq "$head" "$(git rev-parse HEAD)" "HEAD"

case_begin "--dry-run: no re-exec line when the script is unchanged"
new_clone
push_to_origin other.txt "x" "unrelated"
run_finish --dry-run 0.9.9
assert_stdout_contains "would run: git pull --ff-only origin main"
assert_stdout_not_contains "would re-exec"

case_begin "FINISH_SH_REEXEC set: doesn't exec again, carries on"
new_clone
push_to_origin scripts/release/finish.sh "$(new_script)" "change finish.sh"
FINISH_SH_REEXEC=1 run_finish 0.9.9
assert_exit 1
assert_stdout_not_contains "NEW finish.sh"
assert_stdout_contains "changed again after a re-exec"
assert_stderr_contains "no 'release: 0.9.9' commit"

finish
