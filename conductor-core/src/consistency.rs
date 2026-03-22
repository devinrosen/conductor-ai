//! Desync detection protocol: detect state desynchronization between SQLite and filesystem.
//!
//! Covers pattern: desync-detection-protocol@1.0.0
//!
//! `ConsistencyChecker` follows the manager pattern (`&Connection`, `&Config`).
//! It is report-only: no auto-fix except for confirmed-orphaned agent runs
//! (tmux window absent), which transition to `failed`.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::config::Config;
use crate::error::Result;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Flag describing the desynchronization state of an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesyncFlag {
    /// Entity is consistent between declared (SQLite) and observed (reality).
    Ok,
    /// SQLite says active/running but the real-world resource has moved ahead
    /// or disappeared (e.g. tmux window gone while DB still says "running").
    DesyncAhead,
    /// Filesystem/process exists but SQLite has no record (rare, detected only
    /// in future scanning modes).
    DesyncPhantom,
    /// Entity exists in both but its state is outdated or stale.
    DesyncStale,
}

impl std::fmt::Display for DesyncFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "ok"),
            Self::DesyncAhead => write!(f, "desync_ahead"),
            Self::DesyncPhantom => write!(f, "desync_phantom"),
            Self::DesyncStale => write!(f, "desync_stale"),
        }
    }
}

/// A single desync finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesyncFinding {
    /// Entity type (e.g. "worktree", "agent_run", "workflow_run", "repo").
    pub entity_type: String,
    /// Entity identifier (slug, ID, etc).
    pub entity_id: String,
    /// What was expected (from SQLite).
    pub declared: String,
    /// What was observed (from filesystem/process).
    pub observed: String,
    /// Classification of the desync.
    pub flag: DesyncFlag,
    /// Human-readable description.
    pub message: String,
}

/// Aggregated desync report across all entity types.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesyncReport {
    pub findings: Vec<DesyncFinding>,
}

impl DesyncReport {
    /// Whether any desyncs were detected.
    pub fn has_desyncs(&self) -> bool {
        self.findings.iter().any(|f| f.flag != DesyncFlag::Ok)
    }

    /// Count of non-OK findings.
    pub fn desync_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.flag != DesyncFlag::Ok)
            .count()
    }
}

// ---------------------------------------------------------------------------
// Trait for tmux interaction (allows mocking in tests)
// ---------------------------------------------------------------------------

/// Abstraction over tmux window checking so tests can avoid real subprocess calls.
///
/// Mirrors `crate::agent::tmux::list_live_tmux_windows()` but as a trait for testability.
pub trait TmuxChecker: Send + Sync {
    /// Return the set of currently-live tmux window names.
    fn live_windows(&self) -> HashSet<String>;
}

/// Real implementation that shells out to `tmux list-windows -a`.
pub struct RealTmuxChecker;

impl TmuxChecker for RealTmuxChecker {
    fn live_windows(&self) -> HashSet<String> {
        crate::agent::list_live_tmux_windows()
    }
}

// ---------------------------------------------------------------------------
// ConsistencyChecker
// ---------------------------------------------------------------------------

/// Detects desynchronization between SQLite state and the real world.
///
/// Follows the conductor manager pattern: takes `&Connection` + `&Config`.
pub struct ConsistencyChecker<'a> {
    conn: &'a Connection,
    #[allow(dead_code)]
    config: &'a Config,
    tmux: Box<dyn TmuxChecker>,
}

impl<'a> ConsistencyChecker<'a> {
    /// Create a checker using the real tmux implementation.
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self {
            conn,
            config,
            tmux: Box::new(RealTmuxChecker),
        }
    }

    /// Create a checker with a custom tmux implementation (for testing).
    pub fn with_tmux(conn: &'a Connection, config: &'a Config, tmux: Box<dyn TmuxChecker>) -> Self {
        Self { conn, config, tmux }
    }

    /// Run all consistency checks and return an aggregated report.
    pub fn check_all(&self) -> Result<DesyncReport> {
        let mut report = DesyncReport::default();
        report.findings.extend(self.check_worktrees()?.findings);
        report.findings.extend(self.check_agent_runs()?.findings);
        report.findings.extend(self.check_workflow_runs()?.findings);
        Ok(report)
    }

    /// Check active worktrees: verify their directories exist on disk.
    pub fn check_worktrees(&self) -> Result<DesyncReport> {
        let mut report = DesyncReport::default();

        let mut stmt = self
            .conn
            .prepare("SELECT id, slug, branch, path FROM worktrees WHERE status = 'active'")?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;

        for row in rows {
            let (id, slug, _branch, path) = row?;
            let dir = Path::new(&path);
            if !dir.exists() {
                report.findings.push(DesyncFinding {
                    entity_type: "worktree".to_string(),
                    entity_id: id.clone(),
                    declared: format!("status=active, path={path}"),
                    observed: "directory does not exist".to_string(),
                    flag: DesyncFlag::DesyncPhantom,
                    message: format!(
                        "Worktree '{slug}' is active in DB but its directory is missing: {path}"
                    ),
                });
            }
        }

        Ok(report)
    }

    /// Check running agent runs: verify their tmux windows still exist.
    pub fn check_agent_runs(&self) -> Result<DesyncReport> {
        let mut report = DesyncReport::default();
        let live_windows = self.tmux.live_windows();

        let mut stmt = self.conn.prepare(
            "SELECT id, tmux_window FROM agent_runs WHERE status IN ('running', 'waiting_for_feedback')",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;

        for row in rows {
            let (id, window) = row?;
            // If the run has a tmux_window set, check it is still live.
            // If tmux_window is NULL the run has no window to check (already suspect).
            let is_orphaned = match &window {
                Some(win) => !live_windows.contains(win.as_str()),
                None => true, // No window recorded — treat as orphaned
            };

            if is_orphaned {
                let win_display = window.as_deref().unwrap_or("<none>");
                report.findings.push(DesyncFinding {
                    entity_type: "agent_run".to_string(),
                    entity_id: id.clone(),
                    declared: format!("status=running, tmux_window={win_display}"),
                    observed: "tmux window does not exist".to_string(),
                    flag: DesyncFlag::DesyncAhead,
                    message: format!(
                        "Agent run '{id}' is running in DB but tmux window '{win_display}' is absent"
                    ),
                });
            }
        }

        Ok(report)
    }

    /// Check running workflow runs: detect stale workflows whose parent agent is no longer running.
    pub fn check_workflow_runs(&self) -> Result<DesyncReport> {
        let mut report = DesyncReport::default();

        // Find workflow runs that are "running" but have steps marked running with
        // no backing running agent.
        let mut stmt = self.conn.prepare(
            "SELECT wr.id, wr.workflow_name
             FROM workflow_runs wr
             WHERE wr.status = 'running'
               AND NOT EXISTS (
                   SELECT 1 FROM workflow_run_steps wrs
                   JOIN agent_runs ar ON ar.id = wrs.child_run_id
                   WHERE wrs.workflow_run_id = wr.id
                     AND wrs.status = 'running'
                     AND ar.status = 'running'
               )
               AND EXISTS (
                   SELECT 1 FROM workflow_run_steps wrs2
                   WHERE wrs2.workflow_run_id = wr.id
                     AND wrs2.status = 'running'
               )",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (id, name) = row?;
            report.findings.push(DesyncFinding {
                entity_type: "workflow_run".to_string(),
                entity_id: id.clone(),
                declared: "status=running".to_string(),
                observed: "no running agent backing running steps".to_string(),
                flag: DesyncFlag::DesyncAhead,
                message: format!(
                    "Workflow '{name}' (run {id}) is running but has steps with no active agent process"
                ),
            });
        }

        Ok(report)
    }

    /// Transition an orphaned agent run to failed status.
    ///
    /// This is the only auto-fix action the consistency checker performs.
    pub fn recover_orphaned_agent_run(&self, agent_run_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET status = 'failed', result_text = ?1, ended_at = ?2
             WHERE id = ?3 AND status = 'running'",
            params![
                "Process terminated externally (desync recovery)",
                chrono::Utc::now().to_rfc3339(),
                agent_run_id,
            ],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    /// Mock tmux checker that returns a configurable set of live windows.
    struct MockTmux {
        windows: HashSet<String>,
    }

    impl MockTmux {
        fn new(windows: Vec<&str>) -> Self {
            Self {
                windows: windows.into_iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl TmuxChecker for MockTmux {
        fn live_windows(&self) -> HashSet<String> {
            self.windows.clone()
        }
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn insert_repo(conn: &Connection) {
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo-1', 'test-repo', '/tmp/r', 'https://x.com/r.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
    }

    fn insert_worktree(conn: &Connection, id: &str, slug: &str, path: &str) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at)
             VALUES (?1, 'repo-1', ?2, 'main', ?3, 'active', '2024-01-01T00:00:00Z')",
            params![id, slug, path],
        )
        .unwrap();
    }

    fn insert_running_agent(conn: &Connection, id: &str, wt_id: &str, tmux_window: Option<&str>) {
        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, prompt, status, tmux_window, started_at)
             VALUES (?1, ?2, 'test', 'running', ?3, '2024-01-01T00:00:00Z')",
            params![id, wt_id, tmux_window],
        )
        .unwrap();
    }

    #[test]
    fn clean_state_no_desyncs() {
        let conn = setup_db();
        let config = Config::default();
        let checker =
            ConsistencyChecker::with_tmux(&conn, &config, Box::new(MockTmux::new(vec![])));
        let report = checker.check_all().unwrap();
        assert!(!report.has_desyncs());
        assert_eq!(report.desync_count(), 0);
    }

    #[test]
    fn phantom_worktree_detected() {
        let conn = setup_db();
        let config = Config::default();
        insert_repo(&conn);
        insert_worktree(
            &conn,
            "wt-1",
            "my-worktree",
            "/tmp/definitely-does-not-exist-12345",
        );

        let checker =
            ConsistencyChecker::with_tmux(&conn, &config, Box::new(MockTmux::new(vec![])));
        let report = checker.check_worktrees().unwrap();
        assert!(report.has_desyncs());
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].flag, DesyncFlag::DesyncPhantom);
        assert!(report.findings[0].message.contains("my-worktree"));
    }

    #[test]
    fn orphaned_agent_run_detected() {
        let conn = setup_db();
        let config = Config::default();
        insert_repo(&conn);
        insert_worktree(&conn, "wt-1", "wt", "/tmp/wt");
        insert_running_agent(&conn, "ar-1", "wt-1", Some("agent-1"));

        // Mock says no tmux windows exist
        let checker =
            ConsistencyChecker::with_tmux(&conn, &config, Box::new(MockTmux::new(vec![])));
        let report = checker.check_agent_runs().unwrap();
        assert!(report.has_desyncs());
        assert_eq!(report.findings[0].flag, DesyncFlag::DesyncAhead);
        assert!(report.findings[0].message.contains("ar-1"));
    }

    #[test]
    fn agent_run_with_existing_tmux_is_ok() {
        let conn = setup_db();
        let config = Config::default();
        insert_repo(&conn);
        insert_worktree(&conn, "wt-1", "wt", "/tmp/wt");
        insert_running_agent(&conn, "ar-1", "wt-1", Some("agent-1"));

        // Mock says tmux window DOES exist
        let checker =
            ConsistencyChecker::with_tmux(&conn, &config, Box::new(MockTmux::new(vec!["agent-1"])));
        let report = checker.check_agent_runs().unwrap();
        assert!(!report.has_desyncs());
    }

    #[test]
    fn recover_orphaned_agent_run() {
        let conn = setup_db();
        let config = Config::default();
        insert_repo(&conn);
        insert_worktree(&conn, "wt-1", "wt", "/tmp/wt");
        insert_running_agent(&conn, "ar-1", "wt-1", Some("agent-1"));

        let checker =
            ConsistencyChecker::with_tmux(&conn, &config, Box::new(MockTmux::new(vec![])));
        checker.recover_orphaned_agent_run("ar-1").unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM agent_runs WHERE id = 'ar-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed");

        let result: String = conn
            .query_row(
                "SELECT result_text FROM agent_runs WHERE id = 'ar-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(result.contains("desync recovery"));
    }

    #[test]
    fn check_all_aggregates_findings() {
        let conn = setup_db();
        let config = Config::default();
        insert_repo(&conn);
        insert_worktree(&conn, "wt-1", "phantom", "/tmp/no-such-dir-99999");
        insert_running_agent(&conn, "ar-1", "wt-1", Some("win-1"));

        let checker =
            ConsistencyChecker::with_tmux(&conn, &config, Box::new(MockTmux::new(vec![])));
        let report = checker.check_all().unwrap();
        assert!(report.has_desyncs());
        assert_eq!(report.desync_count(), 2);

        let types: Vec<&str> = report
            .findings
            .iter()
            .map(|f| f.entity_type.as_str())
            .collect();
        assert!(types.contains(&"worktree"));
        assert!(types.contains(&"agent_run"));
    }

    #[test]
    fn desync_report_default_is_empty() {
        let report = DesyncReport::default();
        assert!(!report.has_desyncs());
        assert_eq!(report.desync_count(), 0);
    }
}
