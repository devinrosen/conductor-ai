//! Per-session spool for hook events that could not reach the database
//! (RFC 021, #3160).
//!
//! The hook waits only briefly for SQLite's write lock. When it cannot get it
//! (or any open/write step fails) it appends the trimmed [`HookEvent`] to
//! `<db dir>/session-spool/<session>.jsonl` and exits. The events are applied
//! later, in order, with each event's own timestamp:
//!
//! - by the next hook of the same session ([`record_hook_event`]), before its
//!   own event, inside the same transaction;
//! - by the TUI and web pollers ([`drain_all`]) on every tick.
//!
//! # Files
//!
//! - `<stem>.jsonl`: live file, one `{"v":1, ...HookEvent}` per line.
//! - `<stem>.<ulid>.draining`: a claimed file being applied.
//! - `.lock`: directory-wide `flock` guarding append against rename.
//!
//! `<stem>` is the session id when it is filename-safe, else an encoding of it
//! (see [`session_file_stem`]).
//!
//! # No staleness timeout
//!
//! A drainer renames `<stem>.jsonl` to `.draining` only **while holding the
//! SQLite write lock**, and keeps the lock until commit. So any `.draining`
//! file a lock holder finds is either from a drainer that rolled back or died
//! (never applied) or one that committed but did not get to delete it
//! (applied). Both are safe to apply again, because state updates are absolute
//! assignments, feedback inserts carry a `dedup_key`, and `agent_run_events`
//! inserts are deduplicated in [`ApplyMode::Replay`]. No clock-based guess
//! about whether a drainer is still alive is needed.
//!
//! This is also why the claim stays inside the transaction rather than before
//! `BEGIN IMMEDIATE`: claiming earlier would let another drainer mistake a live
//! drainer's file for a leftover. Only the cheap `has_session_files` check runs
//! before the lock.
//!
//! The flock is held only for the rename or the append, never for database
//! work. It assumes a local filesystem; `CONDUCTOR_HOME` on NFS is unsupported.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::hook::{apply_in_tx, ApplyMode, HookEvent, HookOutcome};
use crate::error::{ConductorError, Result};
use crate::new_id;

const SPOOL_DIR_NAME: &str = "session-spool";
const LOCK_FILE: &str = ".lock";
const LIVE_EXT: &str = ".jsonl";
const DRAINING_EXT: &str = ".draining";
const LINE_VERSION: u32 = 1;
/// Longest session id used verbatim as a file stem.
const MAX_PLAIN_STEM: usize = 128;
/// Longest session id hex-encoded in full; longer ones are hashed.
const MAX_HEX_STEM_BYTES: usize = 60;

#[derive(Debug, Serialize, Deserialize)]
struct SpoolLine {
    v: u32,
    #[serde(flatten)]
    event: HookEvent,
}

/// What [`record_hook_event`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOutcome {
    /// Outcome of the hook's own event.
    pub outcome: HookOutcome,
    /// Spooled events applied before it.
    pub drained: usize,
}

/// What [`drain_all`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainStats {
    /// Sessions whose spool was applied.
    pub sessions: usize,
    /// Spooled events applied.
    pub events: usize,
}

/// The spool directory that pairs with the database at `db_path`.
pub fn spool_dir_for(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(SPOOL_DIR_NAME)
}

/// Filename-safe stem for a session id. Ids made of `[A-Za-z0-9_-]` (and at
/// most 128 chars) are used as-is; anything else is encoded as `x-<hex>`, or
/// `x-h<hash>-<len>` when long. The result never contains `.` or a separator.
pub fn session_file_stem(session_id: &str) -> String {
    let plain = !session_id.is_empty()
        && session_id.len() <= MAX_PLAIN_STEM
        && !session_id.starts_with("x-")
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if plain {
        return session_id.to_string();
    }
    let bytes = session_id.as_bytes();
    if bytes.len() <= MAX_HEX_STEM_BYTES {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        return format!("x-{hex}");
    }
    // FNV-1a, 64-bit: stable across releases, unlike `DefaultHasher`.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("x-h{h:016x}-{}", bytes.len())
}

/// Wrap an I/O error with the operation and path that failed, so logs and the
/// CLI's `spool failed:` message say which step broke.
fn ctx<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> std::io::Error + 'a {
    move |e| std::io::Error::new(e.kind(), format!("{op} {}: {e}", path.display()))
}

/// Create the spool directory owner-only: it holds prompts and shell commands.
fn create_spool_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // A directory made by an earlier version keeps its old mode.
        let meta = fs::metadata(dir)?;
        if meta.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Open options for spool-owned files: owner read/write only on unix.
fn private_open_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Directory-wide advisory lock. Released on drop.
struct DirLock {
    #[allow(dead_code)]
    file: File,
}

impl DirLock {
    fn acquire(dir: &Path) -> std::io::Result<DirLock> {
        let lock_path = dir.join(LOCK_FILE);
        let file = private_open_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(ctx("cannot open spool lock", &lock_path))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            loop {
                // SAFETY: the fd is valid for the lifetime of `file`.
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
                if rc == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(ctx("cannot lock", &lock_path)(err));
                }
            }
        }
        Ok(DirLock { file })
    }
}

/// Append `ev` to its session's live spool file. Creates the directory.
pub fn append(dir: &Path, ev: &HookEvent) -> Result<()> {
    let Some(session_id) = ev.session_id.as_deref() else {
        return Err(ConductorError::Io(std::io::Error::other(
            "cannot spool an event without a session_id",
        )));
    };
    let mut line = serde_json::to_vec(&SpoolLine {
        v: LINE_VERSION,
        event: ev.clone(),
    })
    .map_err(|e| {
        ConductorError::Io(std::io::Error::other(format!(
            "cannot serialize {:?} event for session {session_id}: {e}",
            ev.hook_event_name
        )))
    })?;
    line.push(b'\n');

    create_spool_dir(dir).map_err(ctx("cannot create spool dir", dir))?;
    let _lock = DirLock::acquire(dir)?;
    let path = dir.join(format!("{}{LIVE_EXT}", session_file_stem(session_id)));
    let mut f = private_open_options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(ctx("cannot open spool file", &path))?;
    f.write_all(&line)
        .map_err(ctx("cannot write spool file", &path))?;
    Ok(())
}

/// Files of one session claimed for draining.
#[derive(Debug, Default)]
struct Claimed {
    /// Leftover `.draining` files, oldest first. May have been applied.
    leftovers: Vec<PathBuf>,
    /// The live file just renamed. Never applied.
    fresh: Option<PathBuf>,
}

impl Claimed {
    fn all_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.leftovers.iter().chain(self.fresh.iter())
    }
}

/// Split a spool file name into its session stem.
fn stem_of(name: &str) -> Option<&str> {
    if let Some(stem) = name.strip_suffix(LIVE_EXT) {
        return (!stem.is_empty() && !stem.contains('.')).then_some(stem);
    }
    let rest = name.strip_suffix(DRAINING_EXT)?;
    let (stem, _ulid) = rest.rsplit_once('.')?;
    (!stem.is_empty() && !stem.contains('.')).then_some(stem)
}

fn has_session_files(dir: &Path, stem: &str) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    rd.flatten()
        .any(|e| e.file_name().to_str().and_then(stem_of) == Some(stem))
}

/// Claim a session's spool files. **The caller must hold the SQLite write
/// lock** (see the module docs).
fn claim(dir: &Path, stem: &str) -> std::io::Result<Claimed> {
    let _lock = DirLock::acquire(dir)?;
    let mut claimed = Claimed::default();
    let mut live = None;
    for entry in fs::read_dir(dir)
        .map_err(ctx("cannot list spool dir", dir))?
        .flatten()
    {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if stem_of(name) != Some(stem) {
            continue;
        }
        if name.ends_with(DRAINING_EXT) {
            claimed.leftovers.push(entry.path());
        } else {
            live = Some(entry.path());
        }
    }
    claimed.leftovers.sort();
    if let Some(live) = live {
        let target = dir.join(format!("{stem}.{}{DRAINING_EXT}", new_id()));
        fs::rename(&live, &target).map_err(ctx("cannot claim spool file", &live))?;
        claimed.fresh = Some(target);
    }
    Ok(claimed)
}

/// Parse a spool file, skipping blank, torn, unknown-version and garbage lines.
fn read_events(path: &Path) -> std::io::Result<Vec<HookEvent>> {
    let data = fs::read(path).map_err(ctx("cannot read spool file", path))?;
    let mut out = Vec::new();
    for raw in data.split(|b| *b == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<SpoolLine>(raw) {
            Ok(l) if l.v == LINE_VERSION => out.push(l.event),
            Ok(l) => {
                tracing::warn!(file = %path.display(), v = l.v, "skipping spool line with unknown version")
            }
            Err(e) => {
                tracing::warn!(file = %path.display(), "skipping unparseable spool line: {e}")
            }
        }
    }
    Ok(out)
}

fn is_busy(e: &ConductorError) -> bool {
    matches!(
        e,
        ConductorError::Database(rusqlite::Error::SqliteFailure(f, _))
            if matches!(
                f.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// Apply every line of `path` in order, each in its own savepoint so one
/// poison event cannot wedge the session. A busy error aborts the drain.
fn apply_file(tx: &Connection, path: &Path, mode: ApplyMode) -> Result<usize> {
    let mut applied = 0;
    for ev in read_events(path)? {
        tx.execute_batch("SAVEPOINT spool_line")?;
        match apply_in_tx(tx, &ev, mode) {
            Ok(_) => {
                tx.execute_batch("RELEASE spool_line")?;
                applied += 1;
            }
            Err(e) if is_busy(&e) => return Err(e),
            Err(e) => {
                tx.execute_batch("ROLLBACK TO spool_line; RELEASE spool_line")?;
                tracing::warn!(
                    file = %path.display(),
                    session = ?ev.session_id,
                    "dropping spooled hook event that failed to apply: {e}"
                );
            }
        }
    }
    Ok(applied)
}

/// Apply a claim: leftovers in replay mode first, then the fresh file.
fn apply_claimed(tx: &Connection, claimed: &Claimed) -> Result<usize> {
    let mut n = 0;
    for p in &claimed.leftovers {
        n += apply_file(tx, p, ApplyMode::Replay)?;
    }
    if let Some(p) = &claimed.fresh {
        n += apply_file(tx, p, ApplyMode::Fresh)?;
    }
    Ok(n)
}

/// Inside an open write transaction: claim a session's spool files and apply
/// them. Both the hook and the poller go through here so the ordering (claim
/// under the write lock, commit, only then delete) lives in one place. The
/// caller commits, then calls [`remove_claimed`].
fn claim_and_apply(tx: &Connection, dir: &Path, stem: &str) -> Result<(Claimed, usize)> {
    let claimed = claim(dir, stem)?;
    let n = apply_claimed(tx, &claimed)?;
    Ok((claimed, n))
}

fn remove_claimed(claimed: &Claimed) {
    for p in claimed.all_paths() {
        if let Err(e) = fs::remove_file(p) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(file = %p.display(), "could not delete drained spool file: {e}");
            }
        }
    }
}

/// Hook path: in one IMMEDIATE transaction, apply this session's spooled
/// events, then `ev`; commit; delete the drained files.
///
/// On error nothing is committed and drained files stay as `.draining`
/// leftovers. The caller should then [`append`] `ev`, which orders it after
/// them.
pub fn record_hook_event(conn: &Connection, dir: &Path, ev: &HookEvent) -> Result<RecordOutcome> {
    // Checked before taking the write lock so the common case (no spool) does
    // no file work inside the transaction. A file that appears after this
    // check belongs to an event that raced this hook; it is drained next time.
    let stem = ev
        .session_id
        .as_deref()
        .map(session_file_stem)
        .filter(|stem| has_session_files(dir, stem));
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let mut claimed = Claimed::default();
    let mut drained = 0;
    if let Some(stem) = stem {
        (claimed, drained) = claim_and_apply(&tx, dir, &stem)?;
    }
    let outcome = apply_in_tx(&tx, ev, ApplyMode::Fresh)?;
    tx.commit()?;
    remove_claimed(&claimed);
    Ok(RecordOutcome { outcome, drained })
}

/// Poller path: drain every session's spool, one IMMEDIATE transaction per
/// session. A busy database skips that session until the next tick. Costs one
/// `read_dir` (no database lock) when there is nothing to do.
pub fn drain_all(conn: &Connection, dir: &Path) -> Result<DrainStats> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DrainStats::default()),
        Err(e) => return Err(ctx("cannot list spool dir", dir)(e).into()),
    };
    let stems: BTreeSet<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(stem_of).map(str::to_string))
        .collect();

    let mut stats = DrainStats::default();
    for stem in stems {
        match drain_session(conn, dir, &stem) {
            Ok(n) => {
                stats.sessions += 1;
                stats.events += n;
            }
            Err(e) if is_busy(&e) => {
                tracing::debug!(session = %stem, "spool drain deferred: database busy")
            }
            Err(e) => tracing::warn!(session = %stem, "spool drain failed: {e}"),
        }
    }
    Ok(stats)
}

fn drain_session(conn: &Connection, dir: &Path, stem: &str) -> Result<usize> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let (claimed, n) = claim_and_apply(&tx, dir, stem)?;
    tx.commit()?;
    remove_claimed(&claimed);
    Ok(n)
}

/// Poller entry point: drain the spool that pairs with the database at
/// `db_path`, logging (never returning) a failure so a tick carries on.
pub fn drain_for_db(conn: &Connection, db_path: &Path) -> DrainStats {
    match drain_all(conn, &spool_dir_for(db_path)) {
        Ok(stats) => stats,
        Err(e) => {
            tracing::warn!("session spool drain failed: {e}");
            DrainStats::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::hook::{apply_hook_event, HookEnv};
    use crate::test_helpers::setup_db;
    use serde_json::{json, Value};

    const T0: &str = "2026-11-02T10:00:00+00:00";
    const T1: &str = "2026-11-02T10:00:05+00:00";
    const T2: &str = "2026-11-02T10:00:10+00:00";
    const T3: &str = "2026-11-02T10:00:15+00:00";

    fn ev(name: &str, extra: Value, at: &str) -> HookEvent {
        let mut v = json!({
            "hook_event_name": name,
            "session_id": "s1",
            "cwd": "/tmp/ws/feat-test/src",
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        let env = HookEnv {
            attended: true,
            claude_pid: Some(std::process::id()),
            tmux_pane: Some("%3".into()),
        };
        HookEvent::from_payload(&v, env, at.to_string())
    }

    fn perm(at: &str) -> HookEvent {
        ev(
            "PermissionRequest",
            json!({"tool_name": "Bash", "prompt_id": "p1", "tool_input": {"command": "ls"}}),
            at,
        )
    }

    fn post(at: &str) -> HookEvent {
        ev(
            "PostToolUse",
            json!({"tool_name": "Bash", "prompt_id": "p1"}),
            at,
        )
    }

    fn status(conn: &Connection) -> String {
        conn.query_row(
            "SELECT status FROM agent_runs WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn dir_files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n != LOCK_FILE)
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn stem_passthrough_and_encoding() {
        let uuid = "6f1c2a9e-1b2c-4d3e-8f9a-0123456789ab";
        assert_eq!(session_file_stem(uuid), uuid);
        for bad in ["../x", "a/b", "a.b", "", "x-61", "with space"] {
            let s = session_file_stem(bad);
            assert!(s.starts_with("x-"), "{bad} -> {s}");
            assert!(s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        }
        assert_ne!(session_file_stem("a/b"), session_file_stem("a.b"));
        let long = "é".repeat(200);
        let s = session_file_stem(&long);
        assert!(s.starts_with("x-h") && s.len() < 40);
        assert_ne!(s, session_file_stem(&"é".repeat(201)));
    }

    #[test]
    fn stem_of_parses_file_names() {
        assert_eq!(stem_of("abc.jsonl"), Some("abc"));
        assert_eq!(stem_of("abc.01ABC.draining"), Some("abc"));
        assert_eq!(stem_of(".lock"), None);
        assert_eq!(stem_of("notes.txt"), None);
    }

    #[test]
    fn missing_dir_is_a_noop() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let stats = drain_all(&conn, &tmp.path().join("nope")).unwrap();
        assert_eq!(stats, DrainStats::default());
    }

    #[test]
    fn append_then_drain_applies_in_order_with_own_timestamps() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        append(&dir, &ev("SessionStart", json!({"source": "startup"}), T0)).unwrap();
        append(&dir, &perm(T1)).unwrap();
        append(&dir, &post(T2)).unwrap();

        let stats = drain_all(&conn, &dir).unwrap();
        assert_eq!(
            stats,
            DrainStats {
                sessions: 1,
                events: 3
            }
        );
        assert_eq!(status(&conn), "running");
        let (st, resp_at): (String, String) = conn
            .query_row(
                "SELECT status, responded_at FROM feedback_requests",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(st, "responded");
        assert_eq!(resp_at, T2);
        let created: String = conn
            .query_row("SELECT created_at FROM feedback_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(created, T1);
        assert!(dir_files(&dir).is_empty());
    }

    #[test]
    fn non_empty_spool_is_applied_before_the_hooks_own_event() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();
        append(&dir, &perm(T1)).unwrap();

        let out = record_hook_event(&conn, &dir, &post(T2)).unwrap();
        assert_eq!(out.drained, 2);
        assert!(matches!(out.outcome, HookOutcome::Applied { .. }));
        assert_eq!(status(&conn), "running");
        assert_eq!(count(&conn, "feedback_requests"), 1);
        assert!(dir_files(&dir).is_empty());
    }

    #[test]
    fn record_without_spool_writes_directly_and_creates_no_dir() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        let out = record_hook_event(&conn, &dir, &ev("SessionStart", json!({}), T0)).unwrap();
        assert_eq!(out.drained, 0);
        assert_eq!(status(&conn), "idle");
        assert!(!dir.exists());
    }

    #[test]
    fn crash_replay_leaves_one_feedback_and_no_duplicate_events() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        let events = [ev("SessionStart", json!({}), T0), perm(T1)];
        for e in &events {
            append(&dir, e).unwrap();
        }
        // Keep a copy of the file, drain it, then put it back as a leftover:
        // a drain that committed but died before deleting.
        let live = dir.join("s1.jsonl");
        let saved = fs::read(&live).unwrap();
        drain_all(&conn, &dir).unwrap();
        let before = (
            count(&conn, "feedback_requests"),
            count(&conn, "agent_run_events"),
        );
        assert_eq!(before.0, 1);
        fs::write(dir.join("s1.01HZZZZZZZZZZZZZZZZZZZZZZZ.draining"), saved).unwrap();

        let stats = drain_all(&conn, &dir).unwrap();
        assert_eq!(stats.events, 2);
        let after = (
            count(&conn, "feedback_requests"),
            count(&conn, "agent_run_events"),
        );
        assert_eq!(before, after);
        assert!(dir_files(&dir).is_empty());
    }

    #[test]
    fn leftover_is_applied_before_live_file() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();
        append(&dir, &perm(T1)).unwrap();
        // Turn the current file into a leftover, then spool the answer.
        fs::rename(
            dir.join("s1.jsonl"),
            dir.join("s1.01HZZZZZZZZZZZZZZZZZZZZZZZ.draining"),
        )
        .unwrap();
        append(&dir, &post(T2)).unwrap();

        drain_all(&conn, &dir).unwrap();
        // Wrong order would leave waiting_for_feedback.
        assert_eq!(status(&conn), "running");
    }

    #[test]
    fn torn_and_garbage_lines_are_skipped() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();
        let live = dir.join("s1.jsonl");
        let mut f = OpenOptions::new().append(true).open(&live).unwrap();
        f.write_all(b"not json\n\n{\"v\":99,\"received_at\":\"x\",\"env\":{}}\n{\"v\":1,\"rece")
            .unwrap();
        drop(f);
        // A torn tail has no newline, so the next append joins it; that line
        // is lost but nothing else is.
        let stats = drain_all(&conn, &dir).unwrap();
        assert_eq!(stats.events, 1);
        assert_eq!(status(&conn), "idle");
        assert!(dir_files(&dir).is_empty());
    }

    #[test]
    fn ignored_events_are_consumed() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        // Unknown session: SessionEnd is ignored.
        append(&dir, &ev("SessionEnd", json!({"reason": "other"}), T0)).unwrap();
        drain_all(&conn, &dir).unwrap();
        assert!(dir_files(&dir).is_empty());
        assert_eq!(count(&conn, "agent_runs"), 0);
    }

    #[test]
    fn concurrent_appends_produce_valid_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        let mut e = post(T3);
                        e.tool_summary = Some(format!("t{t}-{i}-{}", "x".repeat(500)));
                        append(&dir, &e).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let data = fs::read_to_string(dir.join("s1.jsonl")).unwrap();
        let lines: Vec<&str> = data.lines().collect();
        assert_eq!(lines.len(), 200);
        for l in lines {
            let parsed: SpoolLine = serde_json::from_str(l).unwrap();
            assert_eq!(parsed.v, 1);
        }
    }

    #[test]
    fn busy_database_leaves_files_for_the_next_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        let db = tmp.path().join("c.db");
        let conn = crate::test_helpers::open_test_db_file(&db);
        crate::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
        crate::test_helpers::insert_test_worktree(
            &conn,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();

        let other = crate::db::open_database_compat_with_busy_timeout(
            &db,
            std::time::Duration::from_millis(20),
        )
        .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let err = record_hook_event(&other, &dir, &ev("Stop", json!({}), T1)).unwrap_err();
        assert!(is_busy(&err));
        // Nothing was claimed while the lock was unavailable.
        assert_eq!(dir_files(&dir), vec!["s1.jsonl".to_string()]);
        let stats = drain_all(&other, &dir).unwrap();
        assert_eq!(stats.sessions, 0);
        conn.execute_batch("COMMIT").unwrap();

        let stats = drain_all(&other, &dir).unwrap();
        assert_eq!(stats.events, 1);
        assert_eq!(status(&conn), "idle");
        // The direct path still works afterwards.
        apply_hook_event(&conn, &post(T2)).unwrap();
    }

    #[test]
    fn append_without_session_id_errors_and_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        let mut e = ev("SessionStart", json!({}), T0);
        e.session_id = None;
        let err = append(&dir, &e).unwrap_err();
        assert!(err.to_string().contains("session_id"), "{err}");
        assert!(!dir.exists());
    }

    #[test]
    fn append_errors_name_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        // A file where the directory should be.
        let blocked = tmp.path().join("session-spool");
        fs::write(&blocked, b"x").unwrap();
        let err = append(&blocked, &ev("SessionStart", json!({}), T0)).unwrap_err();
        assert!(err.to_string().contains("session-spool"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn spool_dir_and_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        // A directory left by an older version with default modes is tightened.
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("s1.jsonl")), 0o600);
        assert_eq!(mode(&dir.join(LOCK_FILE)), 0o600);
    }

    #[test]
    fn failed_drain_rolls_back_and_the_next_drain_applies_once() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("session-spool");
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();
        append(&dir, &perm(T1)).unwrap();
        // A leftover that applies cleanly, then one that cannot be read (a
        // directory): the drain fails after applying events in the transaction.
        fs::create_dir(&dir).ok();
        let good = dir.join("s1.01AAAAAAAAAAAAAAAAAAAAAAAA.draining");
        fs::rename(dir.join("s1.jsonl"), &good).unwrap();
        append(&dir, &post(T2)).unwrap();
        let bad = dir.join("s1.01BBBBBBBBBBBBBBBBBBBBBBBB.draining");
        fs::create_dir(&bad).unwrap();

        assert!(drain_session(&conn, &dir, "s1").is_err());
        assert_eq!(count(&conn, "agent_runs"), 0);
        assert_eq!(count(&conn, "feedback_requests"), 0);
        assert!(good.exists() && bad.exists());
        let live_claimed = dir_files(&dir)
            .iter()
            .filter(|n| n.ends_with(DRAINING_EXT))
            .count();
        assert_eq!(live_claimed, 3, "{:?}", dir_files(&dir));

        fs::remove_dir(&bad).unwrap();
        assert_eq!(drain_session(&conn, &dir, "s1").unwrap(), 3);
        assert_eq!(count(&conn, "feedback_requests"), 1);
        assert_eq!(status(&conn), "running");
        assert!(dir_files(&dir).is_empty());
    }

    #[test]
    fn drain_for_db_drains_a_quiet_sessions_spool() {
        let conn = setup_db();
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("conductor.db");
        let dir = spool_dir_for(&db_path);
        append(&dir, &ev("SessionStart", json!({}), T0)).unwrap();
        append(&dir, &perm(T1)).unwrap();

        let stats = drain_for_db(&conn, &db_path);
        assert_eq!(
            stats,
            DrainStats {
                sessions: 1,
                events: 2
            }
        );
        assert_eq!(status(&conn), "waiting_for_feedback");
        assert!(dir_files(&dir).is_empty());
        // Nothing to do, and a missing directory, are both quiet no-ops.
        assert_eq!(drain_for_db(&conn, &db_path), DrainStats::default());
        assert_eq!(
            drain_for_db(&conn, &tmp.path().join("nope/c.db")),
            DrainStats::default()
        );
    }
}
