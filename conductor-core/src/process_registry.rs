//! File-based registry of long-running conductor processes.
//!
//! Each long-running process (MCP server, TUI, web, foreground workflow run)
//! writes `<conductor_dir>/processes/<pid>.json` at startup and removes it on
//! exit. A process is **stale** when the binary on disk reports a different
//! commit than the one the process recorded (read via `<exe> --version`) —
//! i.e. it is running old code. A same-commit rebuild is not stale. When the
//! on-disk commit can't be read (binary missing, too old, web, `--version`
//! fails), it falls back to "binary modified after the process started".
//!
//! A file registry (rather than a DB table) avoids SQLite write-lock
//! contention and works before the database opens. Entries left behind by a
//! crash are pruned lazily by [`list_live`].
//!
//! `process_started_at` is macOS-only. Elsewhere, staleness falls back to the
//! `started_at` recorded at registration, and PID recycling is not detected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::conductor_dir;
use crate::error::Result;
use crate::process_utils::{pid_is_alive, pid_was_recycled, process_started_at_rfc3339};

/// What kind of conductor process a record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    Mcp,
    Tui,
    Web,
    Workflow,
}

impl std::fmt::Display for ProcessKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Mcp => "mcp",
            Self::Tui => "tui",
            Self::Web => "web",
            Self::Workflow => "workflow",
        })
    }
}

/// One registered process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub pid: u32,
    pub kind: ProcessKind,
    pub exe: PathBuf,
    pub git_sha: String,
    pub commit_time: String,
    pub version: String,
    /// RFC 3339 start time of the process.
    pub started_at: String,
    pub parent_pid: Option<u32>,
    pub cwd: Option<PathBuf>,
}

/// A registered process whose binary changed after it started.
#[derive(Debug, Clone)]
pub struct StaleInfo {
    pub record: ProcessRecord,
    /// Modification time of the binary, `None` when the binary no longer exists.
    pub exe_modified_at: Option<SystemTime>,
    /// Commit reported by the binary on disk, when it could be read.
    pub disk_git_sha: Option<String>,
    pub reason: String,
    /// True when this is the calling process itself.
    pub is_self: bool,
}

/// Removes the registry file on drop (only if it still belongs to this pid).
#[derive(Debug)]
pub struct ProcessGuard {
    path: PathBuf,
    pid: u32,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let still_ours = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<ProcessRecord>(&s).ok())
            .is_some_and(|r| r.pid == self.pid);
        if still_ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn processes_dir() -> PathBuf {
    conductor_dir().join("processes")
}

fn record_path(pid: u32) -> PathBuf {
    processes_dir().join(format!("{pid}.json"))
}

fn started_at_for(pid: u32) -> String {
    process_started_at_rfc3339(pid).unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
}

/// Record describing the calling process, built from its build metadata.
fn current_record(kind: ProcessKind) -> Result<ProcessRecord> {
    let pid = std::process::id();
    let exe = std::env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let build_info = crate::build_info::get();
    Ok(ProcessRecord {
        pid,
        kind,
        exe,
        git_sha: build_info.git_sha.to_string(),
        commit_time: build_info.commit_time.to_string(),
        version: crate::build_info::VERSION.to_string(),
        started_at: started_at_for(pid),
        parent_pid: parent_pid(),
        cwd: std::env::current_dir().ok(),
    })
}

/// Register the current process, logging a warning on failure. A failed
/// registration only removes the process from stale-build tracking, so it
/// must not stop startup; the warning keeps that gap visible.
pub fn register_or_warn(kind: ProcessKind) -> Option<ProcessGuard> {
    match register(kind) {
        Ok(guard) => Some(guard),
        Err(e) => {
            tracing::warn!("failed to register {kind} process for stale-build tracking: {e}");
            None
        }
    }
}

/// Register the current process. Keep the returned guard alive for the
/// lifetime of the process.
pub fn register(kind: ProcessKind) -> Result<ProcessGuard> {
    let record = current_record(kind)?;
    let pid = record.pid;
    let dir = processes_dir();
    std::fs::create_dir_all(&dir)?;
    let path = record_path(pid);
    let tmp = dir.join(format!("{pid}.json.tmp"));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, &path)?;
    Ok(ProcessGuard { path, pid })
}

#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    Some(unsafe { libc::getppid() } as u32)
}

#[cfg(not(unix))]
fn parent_pid() -> Option<u32> {
    None
}

/// All registered processes that are still alive. Dead or recycled PIDs have
/// their files removed. Never fails: I/O errors are ignored.
pub fn list_live() -> Vec<ProcessRecord> {
    let Ok(entries) = std::fs::read_dir(processes_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let record = match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<ProcessRecord>(&s).map_err(|e| e.to_string()))
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("skipping unreadable process record {}: {e}", path.display());
                continue;
            }
        };
        if !pid_is_alive(record.pid) || pid_was_recycled(record.pid, &record.started_at) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        out.push(record);
    }
    out.sort_by_key(|r| r.pid);
    out
}

fn parse_started_at(s: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| SystemTime::from(t.with_timezone(&chrono::Utc)))
}

fn exe_mtime(exe: &Path) -> Option<SystemTime> {
    std::fs::metadata(exe).and_then(|m| m.modified()).ok()
}

/// How long `<exe> --version` may run before the probe gives up.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Extract the commit from `<name> <version> (<sha> <commit time>)`. Accepts
/// 7-40 lowercase hex characters; `unknown` and anything else give `None`.
fn parse_version_sha(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?;
    let open = line.rfind('(')?;
    let inner = line[open + 1..].split(')').next()?;
    let token = inner.split_whitespace().next()?;
    let valid = (7..=40).contains(&token.len())
        && token
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    valid.then(|| token.to_string())
}

/// Run `<exe> --version` and parse the commit, killing the child on timeout.
/// `Ok(None)` means the binary ran but embeds no commit (a definite answer);
/// `Err` means the probe itself failed (spawn, timeout, non-zero exit, read),
/// which may be transient and must not be cached.
fn probe_disk_sha(exe: &Path) -> std::result::Result<Option<String>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {VERSION_PROBE_TIMEOUT:?}"));
            }
        }
    };
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "no stdout".to_string())?
        .read_to_string(&mut out)
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(parse_version_sha(&out))
}

/// Whether `<exe> --version` is safe to run for this kind. `conductor-web` has
/// no `--version` handling, so running it would start the server.
fn kind_supports_version_flag(kind: ProcessKind) -> bool {
    !matches!(kind, ProcessKind::Web)
}

/// How long a failed probe is remembered. Short, so a transient failure (load,
/// an overlapping relink) is retried soon, but a binary whose probe fails every
/// time isn't re-spawned (up to `VERSION_PROBE_TIMEOUT` each) on every call.
const FAILED_PROBE_TTL: Duration = Duration::from_secs(30);

struct CacheEntry {
    mtime: SystemTime,
    len: u64,
    sha: Option<String>,
    /// Set when the probe failed: the entry is a negative cache that expires
    /// after [`FAILED_PROBE_TTL`] rather than lasting until the file changes.
    failed_at: Option<Instant>,
}

static DISK_SHA_CACHE: Mutex<Option<HashMap<PathBuf, CacheEntry>>> = Mutex::new(None);

/// Commit embedded in the binary at `exe`, or `None` when it is missing or
/// can't report one. Cached per path by the file's (mtime, size), so a binary
/// is only run again after it changes. A failed probe (timeout, spawn error,
/// non-zero exit) is logged and negatively cached for [`FAILED_PROBE_TTL`] only,
/// so a transient failure is retried soon instead of sticking until the file
/// changes, while a persistent one isn't re-spawned on every call.
pub fn disk_build_sha(exe: &Path) -> Option<String> {
    let meta = std::fs::metadata(exe).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();
    {
        let cache = DISK_SHA_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.as_ref().and_then(|c| c.get(exe)) {
            if entry.mtime == mtime && entry.len == len {
                match entry.failed_at {
                    None => return entry.sha.clone(),
                    Some(t) if t.elapsed() < FAILED_PROBE_TTL => return None,
                    Some(_) => {}
                }
            }
        }
    }
    let (sha, failed_at) = match probe_disk_sha(exe) {
        Ok(sha) => (sha, None),
        Err(e) => {
            tracing::warn!("cannot read build commit of {}: {e}", exe.display());
            (None, Some(Instant::now()))
        }
    };
    DISK_SHA_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(
            exe.to_path_buf(),
            CacheEntry {
                mtime,
                len,
                sha: sha.clone(),
                failed_at,
            },
        );
    sha
}

/// [`disk_build_sha`] for a record. The single gate for every caller: records
/// without a build commit (`unknown`) and kinds that can't take `--version`
/// are never probed.
pub fn disk_build_sha_for(record: &ProcessRecord) -> Option<String> {
    if record.git_sha != "unknown" && kind_supports_version_flag(record.kind) {
        disk_build_sha(&record.exe)
    } else {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StaleReason {
    UnparseableStart,
    Removed,
    Rebuilt,
    DifferentBuild { disk: String, running: String },
}

impl std::fmt::Display for StaleReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnparseableStart => write!(f, "unparseable start time"),
            Self::Removed => write!(f, "binary removed"),
            Self::Rebuilt => write!(f, "binary rebuilt after process started"),
            Self::DifferentBuild { disk, running } => {
                write!(f, "binary on disk is build {disk} (running {running})")
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Current,
    Stale(StaleReason),
}

/// Decide staleness. When both commits are known only they count (compared by
/// prefix, since short hashes can grow); otherwise fall back to file times.
fn judge(
    record_sha: &str,
    disk_sha: Option<&str>,
    started: Option<SystemTime>,
    mtime: Option<SystemTime>,
) -> Verdict {
    if record_sha != "unknown" {
        if let Some(disk) = disk_sha {
            return if disk.starts_with(record_sha) || record_sha.starts_with(disk) {
                Verdict::Current
            } else {
                Verdict::Stale(StaleReason::DifferentBuild {
                    disk: disk.to_string(),
                    running: record_sha.to_string(),
                })
            };
        }
    }
    match (started, mtime) {
        (None, _) => Verdict::Stale(StaleReason::UnparseableStart),
        (_, None) => Verdict::Stale(StaleReason::Removed),
        (Some(s), Some(m)) if m > s => Verdict::Stale(StaleReason::Rebuilt),
        _ => Verdict::Current,
    }
}

/// Staleness of one record, or `None` when it is up to date. A record whose
/// `started_at` cannot be parsed and can't be judged by commit is reported
/// stale, since it cannot be verified.
fn staleness_of(record: &ProcessRecord) -> Option<StaleInfo> {
    let mtime = exe_mtime(&record.exe);
    let disk_sha = disk_build_sha_for(record);
    let started = parse_started_at(&record.started_at);
    let reason = match judge(&record.git_sha, disk_sha.as_deref(), started, mtime) {
        Verdict::Current => return None,
        Verdict::Stale(StaleReason::UnparseableStart) => {
            format!("{} {:?}", StaleReason::UnparseableStart, record.started_at)
        }
        Verdict::Stale(r) => r.to_string(),
    };
    Some(StaleInfo {
        is_self: record.pid == std::process::id(),
        record: record.clone(),
        exe_modified_at: mtime,
        disk_git_sha: disk_sha,
        reason,
    })
}

/// Every live registered process whose binary changed after it started.
pub fn stale_processes() -> Vec<StaleInfo> {
    list_live().iter().filter_map(staleness_of).collect()
}

/// Staleness of the calling process, checked directly (does not need it to be registered).
pub fn own_staleness(kind: ProcessKind) -> Option<StaleInfo> {
    match current_record(kind) {
        Ok(record) => staleness_of(&record),
        Err(e) => {
            // Unlike staleness_of, we cannot even build a record to judge, so
            // report nothing, but leave a trace of why.
            tracing::warn!("cannot check staleness of this {kind} process: {e}");
            None
        }
    }
}

/// One line per stale process; `None` when the list is empty.
pub fn format_stale_warning(stale: &[StaleInfo]) -> Option<String> {
    if stale.is_empty() {
        return None;
    }
    let lines: Vec<String> = stale
        .iter()
        .map(|s| {
            let r = &s.record;
            let hint = match r.kind {
                ProcessKind::Mcp => "; reconnect with /mcp in its Claude Code session",
                _ => "; restart it",
            };
            let me = if s.is_self { " (this process)" } else { "" };
            let build = match &s.disk_git_sha {
                Some(disk) => format!("build {} → {disk} on disk", r.git_sha),
                None => format!("build {}", r.git_sha),
            };
            format!(
                "- {} pid {}{me} ({build}, started {}) — {}: {}{hint}",
                r.kind,
                r.pid,
                r.started_at,
                s.reason,
                r.exe.display()
            )
        })
        .collect();
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parse_version_sha_cases() {
        assert_eq!(
            parse_version_sha("conductor 0.17.1 (98a7cdb 2026-11-03T10:00:00Z)\n").as_deref(),
            Some("98a7cdb")
        );
        assert_eq!(
            parse_version_sha("conductor-tui 0.17.1 (98a7cdb2 2026-11-03)").as_deref(),
            Some("98a7cdb2")
        );
        assert_eq!(parse_version_sha("conductor 0.1.0 (unknown unknown)"), None);
        assert_eq!(parse_version_sha("garbage"), None);
        assert_eq!(parse_version_sha("x 1 (zzzzzzz 2026)"), None);
        assert_eq!(parse_version_sha(""), None);
    }

    #[test]
    fn judge_truth_table() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let later = t0 + Duration::from_secs(10);
        let earlier = t0 - Duration::from_secs(10);
        // Same commit with a newer mtime is current (the bug).
        assert_eq!(
            judge("abc1234", Some("abc1234"), Some(t0), Some(later)),
            Verdict::Current
        );
        // Prefix-equal shas are the same commit.
        assert_eq!(
            judge("98a7cdb", Some("98a7cdb2"), Some(t0), Some(later)),
            Verdict::Current
        );
        // Different commit is stale and names both.
        match judge("abc1234", Some("def5678"), Some(t0), Some(earlier)) {
            Verdict::Stale(StaleReason::DifferentBuild { disk, running }) => {
                assert_eq!((disk.as_str(), running.as_str()), ("def5678", "abc1234"))
            }
            v => panic!("{v:?}"),
        }
        // Fallbacks.
        assert!(matches!(
            judge("abc1234", None, Some(t0), Some(later)),
            Verdict::Stale(_)
        ));
        assert!(matches!(
            judge("unknown", Some("def5678"), Some(t0), Some(later)),
            Verdict::Stale(_)
        ));
        assert_eq!(
            judge("abc1234", None, Some(t0), Some(earlier)),
            Verdict::Current
        );
        assert_eq!(
            judge("abc1234", None, Some(t0), None),
            Verdict::Stale(StaleReason::Removed)
        );
        // A bad start time is reported before a missing binary.
        let bad = Verdict::Stale(StaleReason::UnparseableStart);
        assert_eq!(judge("abc1234", None, None, None), bad);
        assert_eq!(judge("abc1234", None, None, Some(t0)), bad);
        // An unknown record sha ignores the disk commit and uses file times.
        assert_eq!(
            judge("unknown", Some("def5678"), Some(t0), Some(earlier)),
            Verdict::Current
        );
    }

    #[test]
    fn staleness_of_unparseable_start_with_missing_exe_reports_start_time() {
        let mut r = fake_record(4242);
        r.started_at = "garbage".into();
        let info = staleness_of(&r).unwrap();
        assert!(
            info.reason.starts_with("unparseable start time"),
            "{}",
            info.reason
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_probe_is_negatively_cached_until_ttl_expires() {
        let dir = script_dir();
        let counter = dir.join("count");
        let body = format!("echo x >> {}\nexit 1", counter.display());
        let exe = write_script(&dir, "conductor", &body);
        let runs = || std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(disk_build_sha(&exe), None);
        assert_eq!(disk_build_sha(&exe), None);
        assert_eq!(runs(), 1, "second call within the TTL must not re-probe");
        // Age the entry past the TTL: the probe is retried.
        {
            let mut cache = DISK_SHA_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            let entry = cache.as_mut().unwrap().get_mut(&exe).unwrap();
            entry.failed_at = Instant::now().checked_sub(FAILED_PROBE_TTL + Duration::from_secs(1));
            assert!(entry.failed_at.is_some());
        }
        assert_eq!(disk_build_sha(&exe), None);
        assert_eq!(runs(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn script_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conductor-procreg-{}-{}",
            std::process::id(),
            ulid::Ulid::new()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn same_commit_rebuild_is_not_stale_and_different_commit_is() {
        let dir = script_dir();
        let exe = write_script(
            &dir,
            "conductor",
            "echo 'conductor 0.0.0 (abc1234 2026-01-01T00:00:00Z)'",
        );
        let mut r = fake_record(4242);
        r.exe = exe;
        r.started_at = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert!(staleness_of(&r).is_none());
        r.git_sha = "def5678".into();
        let info = staleness_of(&r).unwrap();
        assert_eq!(info.disk_git_sha.as_deref(), Some("abc1234"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn probe_timeout_falls_back_to_mtime() {
        let dir = script_dir();
        let exe = write_script(&dir, "conductor", "exec sleep 30");
        let mut r = fake_record(4242);
        r.exe = exe;
        r.started_at = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let t = Instant::now();
        let info = staleness_of(&r).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(info.disk_git_sha.is_none());
        assert_eq!(info.reason, "binary rebuilt after process started");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn disk_sha_is_cached_until_binary_changes() {
        let dir = script_dir();
        let counter = dir.join("count");
        let body = format!(
            "echo x >> {}\necho 'conductor 0.0.0 (abc1234 2026-01-01T00:00:00Z)'",
            counter.display()
        );
        let exe = write_script(&dir, "conductor", &body);
        let runs = || std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(disk_build_sha(&exe).as_deref(), Some("abc1234"));
        assert_eq!(disk_build_sha(&exe).as_deref(), Some("abc1234"));
        assert_eq!(runs(), 1);
        write_script(&dir, "conductor", &format!("{body}\n# changed"));
        assert_eq!(disk_build_sha(&exe).as_deref(), Some("abc1234"));
        assert_eq!(runs(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn web_kind_is_never_probed() {
        let dir = script_dir();
        let marker = dir.join("ran");
        let exe = write_script(
            &dir,
            "conductor-web",
            &format!(
                "touch {}\necho 'conductor 0.0.0 (abc1234 2026-01-01T00:00:00Z)'",
                marker.display()
            ),
        );
        let mut r = fake_record(4242);
        r.kind = ProcessKind::Web;
        r.exe = exe;
        r.started_at = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let info = staleness_of(&r).unwrap();
        assert_eq!(info.reason, "binary rebuilt after process started");
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn fake_record(pid: u32) -> ProcessRecord {
        ProcessRecord {
            pid,
            kind: ProcessKind::Mcp,
            exe: PathBuf::from("/nonexistent/conductor"),
            git_sha: "abc1234".into(),
            commit_time: "unknown".into(),
            version: "0.0.0".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            parent_pid: None,
            cwd: None,
        }
    }

    /// Tests that touch this process's own record path must not interleave
    /// under `cargo test` (threads share one pid).
    static SELF_RECORD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn register_or_warn_returns_guard_on_success() {
        let _lock = SELF_RECORD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let guard = register_or_warn(ProcessKind::Workflow).expect("registration succeeds");
        assert!(record_path(pid).exists());
        drop(guard);
        assert!(!record_path(pid).exists());
    }

    #[test]
    fn register_or_warn_returns_none_when_registration_fails() {
        let _lock = SELF_RECORD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        std::fs::create_dir_all(processes_dir()).unwrap();
        // A directory in place of the record file makes the final rename fail.
        let path = record_path(pid);
        std::fs::create_dir_all(&path).unwrap();
        assert!(register(ProcessKind::Workflow).is_err());
        assert!(register_or_warn(ProcessKind::Workflow).is_none());
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_file(processes_dir().join(format!("{pid}.json.tmp")));
    }

    #[test]
    fn register_lists_self_and_drop_removes_file() {
        let _lock = SELF_RECORD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pid = std::process::id();
        let guard = register(ProcessKind::Workflow).unwrap();
        assert!(list_live().iter().any(|r| r.pid == pid));
        assert!(record_path(pid).exists());
        drop(guard);
        assert!(!record_path(pid).exists());
    }

    #[test]
    fn dead_pid_is_pruned_and_garbage_skipped() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        std::fs::create_dir_all(processes_dir()).unwrap();
        let dead_path = record_path(dead_pid);
        std::fs::write(
            &dead_path,
            serde_json::to_vec(&fake_record(dead_pid)).unwrap(),
        )
        .unwrap();
        let garbage = processes_dir().join("garbage-test.json");
        std::fs::write(&garbage, "not json").unwrap();

        let live = list_live();
        assert!(!live.iter().any(|r| r.pid == dead_pid));
        assert!(!dead_path.exists());
        assert!(garbage.exists());
        let _ = std::fs::remove_file(garbage);
    }

    #[test]
    fn staleness_of_flags_missing_exe_and_marks_self() {
        let info = staleness_of(&fake_record(std::process::id())).unwrap();
        assert!(info.is_self);
        assert!(info.exe_modified_at.is_none());
        assert_eq!(info.reason, "binary removed");
        let other = staleness_of(&fake_record(std::process::id().wrapping_add(1))).unwrap();
        assert!(!other.is_self);
    }

    #[test]
    fn staleness_of_unparseable_started_at_is_stale() {
        let mut r = fake_record(4242);
        r.started_at = "garbage".into();
        let info = staleness_of(&r).unwrap();
        assert!(info.reason.contains("unparseable"));
    }

    #[test]
    fn staleness_of_fresh_binary_is_none() {
        let mut r = fake_record(4242);
        r.exe = std::env::current_exe().unwrap();
        r.git_sha = "unknown".into();
        r.started_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        assert!(staleness_of(&r).is_none());
    }

    #[test]
    fn own_staleness_is_none_for_unmodified_test_binary() {
        // The test binary was built before this process started.
        assert!(own_staleness(ProcessKind::Workflow).is_none());
    }

    #[test]
    fn stale_processes_is_subset_of_live() {
        let live: Vec<u32> = list_live().iter().map(|r| r.pid).collect();
        assert!(stale_processes()
            .iter()
            .all(|s| live.contains(&s.record.pid)));
    }

    #[test]
    fn format_stale_warning_names_kind_and_pid() {
        assert!(format_stale_warning(&[]).is_none());
        let info = StaleInfo {
            record: fake_record(4242),
            exe_modified_at: None,
            disk_git_sha: Some("def5678".into()),
            reason: "binary removed".into(),
            is_self: false,
        };
        let text = format_stale_warning(&[info]).unwrap();
        assert!(text.contains("mcp pid 4242"));
        assert!(text.contains("def5678 on disk"));
        assert!(text.contains("/mcp"));
    }
}
