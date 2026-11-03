//! File-based registry of long-running conductor processes.
//!
//! Each long-running process (MCP server, TUI, web, foreground workflow run)
//! writes `<conductor_dir>/processes/<pid>.json` at startup and removes it on
//! exit. A process is **stale** when the binary it was started from was
//! modified after the process started — i.e. it is running old code.
//!
//! A file registry (rather than a DB table) avoids SQLite write-lock
//! contention and works before the database opens. Entries left behind by a
//! crash are pruned lazily by [`list_live`].
//!
//! `process_started_at` is macOS-only. Elsewhere, staleness falls back to the
//! `started_at` recorded at registration, and PID recycling is not detected.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// Register the current process. Keep the returned guard alive for the
/// lifetime of the process.
pub fn register(kind: ProcessKind) -> Result<ProcessGuard> {
    let pid = std::process::id();
    let exe = std::env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let record = ProcessRecord {
        pid,
        kind,
        exe,
        git_sha: crate::build_info::GIT_SHA.to_string(),
        commit_time: crate::build_info::COMMIT_TIME.to_string(),
        version: crate::build_info::VERSION.to_string(),
        started_at: started_at_for(pid),
        parent_pid: parent_pid(),
        cwd: std::env::current_dir().ok(),
    };
    let dir = processes_dir();
    std::fs::create_dir_all(&dir)?;
    let path = record_path(pid);
    let tmp = dir.join(format!("{pid}.json.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?)?;
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
        let Some(record) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<ProcessRecord>(&s).ok())
        else {
            continue;
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

/// Stale when the binary was modified after the process started, or removed.
fn is_stale(started_at: SystemTime, exe_mtime: Option<SystemTime>) -> bool {
    match exe_mtime {
        Some(m) => m > started_at,
        None => true,
    }
}

fn parse_started_at(s: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| SystemTime::from(t.with_timezone(&chrono::Utc)))
}

fn exe_mtime(exe: &Path) -> Option<SystemTime> {
    std::fs::metadata(exe).and_then(|m| m.modified()).ok()
}

/// Staleness of one record, or `None` when it is up to date.
fn staleness_of(record: &ProcessRecord) -> Option<StaleInfo> {
    let started = parse_started_at(&record.started_at)?;
    let mtime = exe_mtime(&record.exe);
    if !is_stale(started, mtime) {
        return None;
    }
    let reason = if mtime.is_none() {
        "binary removed".to_string()
    } else {
        "binary rebuilt after process started".to_string()
    };
    Some(StaleInfo {
        is_self: record.pid == std::process::id(),
        record: record.clone(),
        exe_modified_at: mtime,
        reason,
    })
}

/// Every live registered process whose binary changed after it started.
pub fn stale_processes() -> Vec<StaleInfo> {
    list_live().iter().filter_map(staleness_of).collect()
}

/// Staleness of the calling process, checked directly (does not need it to be registered).
pub fn own_staleness(kind: ProcessKind) -> Option<StaleInfo> {
    let pid = std::process::id();
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let record = ProcessRecord {
        pid,
        kind,
        exe,
        git_sha: crate::build_info::GIT_SHA.to_string(),
        commit_time: crate::build_info::COMMIT_TIME.to_string(),
        version: crate::build_info::VERSION.to_string(),
        started_at: started_at_for(pid),
        parent_pid: None,
        cwd: None,
    };
    staleness_of(&record)
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
            format!(
                "- {} pid {}{me} (build {}, started {}) — {}: {}{hint}",
                r.kind,
                r.pid,
                r.git_sha,
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
    fn is_stale_truth_table() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let later = t0 + Duration::from_secs(10);
        let earlier = t0 - Duration::from_secs(10);
        assert!(is_stale(t0, Some(later)));
        assert!(!is_stale(t0, Some(earlier)));
        assert!(!is_stale(t0, Some(t0)));
        assert!(is_stale(t0, None));
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

    #[test]
    fn register_lists_self_and_drop_removes_file() {
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
    fn format_stale_warning_names_kind_and_pid() {
        assert!(format_stale_warning(&[]).is_none());
        let info = StaleInfo {
            record: fake_record(4242),
            exe_modified_at: None,
            reason: "binary removed".into(),
            is_self: false,
        };
        let text = format_stale_warning(&[info]).unwrap();
        assert!(text.contains("mcp pid 4242"));
        assert!(text.contains("/mcp"));
    }
}
