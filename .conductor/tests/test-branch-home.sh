#!/usr/bin/env bash
# lib-branch-home.sh (#3469): whether a branch build may open the real conductor
# database, gets a copy, or must not run. Uses sqlite3 on scratch databases.
set -uo pipefail
source "$(dirname "$0")/lib.sh"

command -v sqlite3 > /dev/null 2>&1 || die "sqlite3 is required"

make_home() { scratch_conductor_home "$1"; }

# make_checkout VERSION — a scratch checkout whose migrations.rs declares VERSION
# (no file when VERSION is empty); print its path.
make_checkout() {
  local d
  d=$(scratch_dir)
  if [ -n "$1" ]; then
    mkdir -p "$d/conductor-core/src/db"
    printf 'use x;\n\npub const LATEST_SCHEMA_VERSION: u32 = %s;\n' "$1" > "$d/conductor-core/src/db/migrations.rs"
  fi
  echo "$d"
}

# setup HOME CHECKOUT — run branch_home_setup there; STDOUT holds
# "rc|mode|CONDUCTOR_HOME|copy schema|copy marker|copy config" then the note, and
# the copy is left in place for the case to inspect (it is under the scratch tmp).
setup() {
  STDOUT=$(cd "$2" && CONDUCTOR_HOME="$1" bash -c '
    source "$SCRIPTS/lib-branch-home.sh"
    branch_home_setup; rc=$?
    v=""; m=""; c=""
    if [ "$BRANCH_HOME_MODE" = copy ]; then
      v=$(sqlite3 "$CONDUCTOR_HOME/conductor.db" "SELECT value FROM _conductor_meta")
      m=$(sqlite3 "$CONDUCTOR_HOME/conductor.db" "SELECT v FROM marker")
      [ -f "$CONDUCTOR_HOME/config.toml" ] && c=config
    fi
    echo "$rc|$BRANCH_HOME_MODE|$CONDUCTOR_HOME|$v|$m|$c"
    echo "$BRANCH_HOME_NOTE"
    echo "$BRANCH_HOME_DIR" > "$CLEANUP_LIST"
  ' 2>&1)
}
field() { printf '%s\n' "$STDOUT" | head -n 1 | cut -d'|' -f"$1"; }
note() { printf '%s\n' "$STDOUT" | sed -n 2p; }

CLEANUP_LIST=$(scratch_dir)/dirs
export CLEANUP_LIST

case_begin "equal versions use the real home"
home=$(make_home 89)
setup "$home" "$(make_checkout 89)"
assert_eq "0|real|$home" "$(field 1)|$(field 2)|$(field 3)" "rc|mode|home"

case_begin "a branch behind the real database must not run, and says to merge the base"
home=$(make_home 90)
setup "$home" "$(make_checkout 89)"
assert_eq "1|behind|$home" "$(field 1)|$(field 2)|$(field 3)" "rc|mode|home"
case "$(note)" in *"schema 90"*"schema 89"*"merge the base"*) ;; *) fail "note: $(note)" ;; esac

case_begin "a branch ahead gets a copy with the data and config; the real database is untouched"
home=$(make_home 89)
before=$(cksum < "$home/conductor.db")
setup "$home" "$(make_checkout 91)"
assert_eq "0|copy" "$(field 1)|$(field 2)" "rc|mode"
[ "$(field 3)" != "$home" ] || fail "CONDUCTOR_HOME still points at the real home"
assert_eq "89|real-data|config" "$(field 4)|$(field 5)|$(field 6)" "copy schema|marker|config"
assert_eq "$before" "$(cksum < "$home/conductor.db")" "real database bytes"
case "$(note)" in *"copy"*"89 to 91"*) ;; *) fail "note: $(note)" ;; esac
rm -rf "$(cat "$CLEANUP_LIST")"

case_begin "a copy honours CONDUCTOR_DB_PATH and unsets it"
home=$(make_home 89)
STDOUT=$(cd "$(make_checkout 90)" && CONDUCTOR_HOME="$(scratch_dir)" CONDUCTOR_DB_PATH="$home/conductor.db" bash -c '
  source "$SCRIPTS/lib-branch-home.sh"
  branch_home_setup
  echo "$BRANCH_HOME_MODE|${CONDUCTOR_DB_PATH-unset}|$(sqlite3 "$CONDUCTOR_HOME/conductor.db" "SELECT v FROM marker")"
  branch_home_cleanup')
assert_eq "copy|unset|real-data" "$STDOUT" "mode|db path|copy marker"

case_begin "an unreadable database is unknown: don't run"
home=$(scratch_dir)
echo "not a database" > "$home/conductor.db"
setup "$home" "$(make_checkout 89)"
assert_eq "1|unknown|$home" "$(field 1)|$(field 2)|$(field 3)" "rc|mode|home"

case_begin "an unparseable migrations.rs is unknown"
home=$(make_home 89)
d=$(make_checkout "")
mkdir -p "$d/conductor-core/src/db" && echo "// nothing" > "$d/conductor-core/src/db/migrations.rs"
setup "$home" "$d"
assert_eq "1|unknown" "$(field 1)|$(field 2)" "rc|mode"

case_begin "no real database, or a checkout that isn't conductor, uses the real home"
setup "$(scratch_dir)" "$(make_checkout 90)"
assert_eq "0|real" "$(field 1)|$(field 2)" "no database"
setup "$(make_home 89)" "$(make_checkout "")"
assert_eq "0|real" "$(field 1)|$(field 2)" "no migrations.rs"

case_begin "branch_home_cleanup removes the copy"
home=$(make_home 89)
STDOUT=$(cd "$(make_checkout 90)" && CONDUCTOR_HOME="$home" bash -c '
  source "$SCRIPTS/lib-branch-home.sh"
  branch_home_setup; d=$BRANCH_HOME_DIR; branch_home_cleanup
  [ -e "$d" ] && echo left || echo gone')
assert_eq "gone" "$STDOUT" "copy dir"

case_begin "real_schema_version: none without a database, fails on a broken one"
STDOUT=$(CONDUCTOR_HOME="$(scratch_dir)" bash -c 'source "$SCRIPTS/lib-branch-home.sh"; real_schema_version')
assert_eq "none" "$STDOUT" "no database"
home=$(scratch_dir)
echo junk > "$home/conductor.db"
CONDUCTOR_HOME="$home" bash -c 'source "$SCRIPTS/lib-branch-home.sh"; real_schema_version' > /dev/null && fail "expected a failure"

finish
