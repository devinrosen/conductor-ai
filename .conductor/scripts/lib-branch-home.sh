#!/usr/bin/env bash
# lib-branch-home.sh — keep a branch build of conductor off the real database (#3469).
#
# Every `conductor` command opens the database, and opening applies pending
# migrations. A build from a branch that adds a migration therefore migrates
# whatever database it opens, and once the real ~/.conductor/conductor.db is on
# the newer schema every installed binary (MCP server, TUI, web, the `conductor`
# that running workflows' script steps call) refuses to open it.
#
# Pipeline scripts that run the branch build source this file:
#
#   throwaway_home            — prints a new empty temp dir; branch_home_setup
#                               copies the real home into it. Static `workflow
#                               validate` needs no isolation (read-only, #3470).
#   branch_home_setup         — for checks that need the real data (dry-run smokes,
#                               screenshots of real runs). Compares the branch's
#                               LATEST_SCHEMA_VERSION with the real database's
#                               schema_version and sets BRANCH_HOME_MODE:
#                                 real    — not ahead (or no real database); the
#                                           real home is left as it is.
#                                 copy    — branch is ahead: the database is copied
#                                           (VACUUM INTO) with config.toml into a temp
#                                           home, and CONDUCTOR_HOME points at it
#                                           (CONDUCTOR_DB_PATH unset). Run IDs exist
#                                           only in the copy.
#                                 behind  — the real database is newer than the
#                                           branch; the branch build would refuse to
#                                           open it.
#                                 unknown — a version couldn't be read, or the copy
#                                           failed. Don't run the branch build.
#                               BRANCH_HOME_NOTE says what was decided, in one line.
#                               Returns 0 for real and copy, 1 otherwise.
#   branch_home_cleanup       — removes the copy (call it from an EXIT trap).
#   real_schema_version       — prints the real database's schema_version, "none"
#                               when there is no database; when it can't be read,
#                               prints why (sqlite3's first error line) and
#                               returns 1. Read-only.
#
# The real database is $CONDUCTOR_DB_PATH when set and non-empty, else
# ${CONDUCTOR_HOME:-$HOME/.conductor}/conductor.db, as conductor itself resolves it.
# The branch version comes from conductor-core/src/db/migrations.rs under the
# current directory; a checkout without that file (not conductor) can't migrate
# anything, so it counts as not ahead.

# Results for the scripts that source this file.
# shellcheck disable=SC2034
BRANCH_HOME_MODE=""
# shellcheck disable=SC2034
BRANCH_HOME_NOTE=""
BRANCH_HOME_DIR=""

real_db_path() {
  if [ -n "${CONDUCTOR_DB_PATH:-}" ]; then
    printf '%s' "${CONDUCTOR_DB_PATH}"
  else
    printf '%s' "${CONDUCTOR_HOME:-${HOME}/.conductor}/conductor.db"
  fi
}

# real_schema_version — read-only; "none" when the database doesn't exist. On
# failure it prints the reason instead of a version.
real_schema_version() {
  local db v
  db=$(real_db_path)
  if [ ! -e "${db}" ]; then
    echo none
    return 0
  fi
  if ! command -v sqlite3 > /dev/null 2>&1; then
    echo "sqlite3 is not installed"
    return 1
  fi
  if ! v=$(sqlite3 -readonly "${db}" "SELECT value FROM _conductor_meta WHERE key = 'schema_version'" 2>&1); then
    printf '%s\n' "${v}" | head -n 1
    return 1
  fi
  case "${v}" in
    '' | *[!0-9]*)
      echo "no numeric schema_version in _conductor_meta (got '${v}')"
      return 1
      ;;
  esac
  echo "${v}"
}

# branch_schema_version — LATEST_SCHEMA_VERSION of this checkout, "none" when the
# checkout has no migrations.rs; returns 1 when the file is there but unparseable.
branch_schema_version() {
  local f=conductor-core/src/db/migrations.rs v
  if [ ! -f "${f}" ]; then
    echo none
    return 0
  fi
  v=$(sed -n -E 's/^pub const LATEST_SCHEMA_VERSION: u32 = ([0-9]+);.*/\1/p' "${f}" | head -n 1)
  [ -n "${v}" ] || return 1
  echo "${v}"
}

throwaway_home() {
  mktemp -d "${TMPDIR:-/tmp}/conductor-home.XXXXXX"
}

# shellcheck disable=SC2034  # BRANCH_HOME_MODE/NOTE are read by the caller
branch_home_setup() {
  local real branch db err
  BRANCH_HOME_DIR=""
  if ! real=$(real_schema_version); then
    BRANCH_HOME_MODE=unknown
    BRANCH_HOME_NOTE="could not read the schema version of the conductor database at $(real_db_path) (${real}); not running this branch's build against it"
    return 1
  fi
  if ! branch=$(branch_schema_version); then
    BRANCH_HOME_MODE=unknown
    BRANCH_HOME_NOTE="could not read LATEST_SCHEMA_VERSION from conductor-core/src/db/migrations.rs; not running this branch's build against the real database"
    return 1
  fi
  if [ "${real}" = none ] || [ "${branch}" = none ] || [ "${branch}" -eq "${real}" ]; then
    BRANCH_HOME_MODE=real
    BRANCH_HOME_NOTE="real conductor database (schema ${real}, branch ${branch})"
    return 0
  fi
  if [ "${branch}" -lt "${real}" ]; then
    BRANCH_HOME_MODE=behind
    BRANCH_HOME_NOTE="the conductor database (schema ${real}) is newer than this branch (schema ${branch}), so this branch's build can't open it; merge the base branch"
    return 1
  fi
  db=$(real_db_path)
  BRANCH_HOME_DIR=$(throwaway_home) || {
    BRANCH_HOME_MODE=unknown
    BRANCH_HOME_NOTE="could not create a temp dir for a copy of the conductor database"
    return 1
  }
  # A plain connection: VACUUM INTO reads the source and writes only the new file
  # (under -readonly sqlite3 can't create the output).
  if ! err=$(sqlite3 "${db}" "VACUUM INTO '${BRANCH_HOME_DIR}/conductor.db'" 2>&1 > /dev/null); then
    branch_home_cleanup
    BRANCH_HOME_MODE=unknown
    BRANCH_HOME_NOTE="could not copy the conductor database at ${db} ($(printf '%s\n' "${err}" | head -n 1)); not running this branch's build (schema ${branch}) against it (schema ${real})"
    return 1
  fi
  local cfg
  cfg="${CONDUCTOR_HOME:-${HOME}/.conductor}/config.toml"
  if [ -f "${cfg}" ] && ! err=$(cp "${cfg}" "${BRANCH_HOME_DIR}/config.toml" 2>&1); then
    branch_home_cleanup
    BRANCH_HOME_MODE=unknown
    BRANCH_HOME_NOTE="could not copy ${cfg} next to the database copy (${err}); not running this branch's build"
    return 1
  fi
  export CONDUCTOR_HOME="${BRANCH_HOME_DIR}"
  unset CONDUCTOR_DB_PATH
  BRANCH_HOME_MODE=copy
  BRANCH_HOME_NOTE="a copy of the conductor database: this branch migrates it from schema ${real} to ${branch}, so the run IDs exist only in the copy, which was deleted"
  return 0
}

branch_home_cleanup() {
  if [ -n "${BRANCH_HOME_DIR}" ]; then
    rm -rf "${BRANCH_HOME_DIR}"
    BRANCH_HOME_DIR=""
  fi
}
