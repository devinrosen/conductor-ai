//! Emergency recovery protocol: graduated escalation for stuck states.
//!
//! Provides detection and recovery for common stuck states in conductor-ai:
//! - Partially created worktrees (dir exists, no DB record)
//! - Stuck workflow runs (running/waiting status with stale timestamps)
//!
//! Recovery follows a graduated escalation ladder:
//! Tier 1: Targeted fix (cancel/cleanup specific resource)
//! Tier 2: Full repair (rebuild from known state)
//!
//! Part of: emergency-recovery-protocol@1.0.0

use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::Path;

use crate::config::Config;
use crate::error::{ConductorError, Result};

/// Configuration for recovery behavior.
pub struct RecoveryConfig {
    /// Hours after which a running/waiting workflow is considered stale.
    pub stale_workflow_threshold_hours: i64,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            stale_workflow_threshold_hours: 4,
        }
    }
}

/// A detected stuck state requiring recovery.
#[derive(Debug, Clone)]
pub struct StuckState {
    pub entity_type: String,
    pub entity_id: String,
    pub description: String,
    pub recommended_action: RecoveryAction,
}

/// Recovery actions ordered by escalation tier.
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// Tier 1: Cancel a stale workflow run.
    CancelStaleRun { run_id: String },
    /// Tier 1: Clean up an orphaned worktree directory.
    CleanupOrphanedWorktree { path: String },
}

/// Manager for recovery operations, following conductor's Manager pattern.
///
/// Part of: emergency-recovery-protocol@1.0.0
pub struct RecoveryManager<'a> {
    conn: &'a Connection,
    _config: &'a Config,
}

impl<'a> RecoveryManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self {
            conn,
            _config: config,
        }
    }

    /// Scan for stuck workflow runs (status=running/waiting with stale updated_at).
    pub fn find_stale_workflow_runs(
        &self,
        recovery_config: &RecoveryConfig,
    ) -> Result<Vec<StuckState>> {
        let threshold =
            Utc::now() - chrono::Duration::hours(recovery_config.stale_workflow_threshold_hours);
        let threshold_str = threshold.to_rfc3339();

        let mut stmt = self.conn.prepare(
            "SELECT id, workflow_name, status, updated_at FROM workflow_runs
             WHERE status IN ('running', 'waiting')
             AND updated_at < ?1",
        )?;

        let mut stale = Vec::new();
        let rows = stmt.query_map(params![threshold_str], |row| {
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let status: String = row.get(2)?;
            let updated: String = row.get(3)?;
            Ok((id, name, status, updated))
        })?;

        for row in rows {
            let (id, name, status, updated) = row.map_err(ConductorError::Database)?;
            stale.push(StuckState {
                entity_type: "workflow_run".to_string(),
                entity_id: id.clone(),
                description: format!(
                    "Workflow '{}' has been {} since {} (>{} hours)",
                    name, status, updated, recovery_config.stale_workflow_threshold_hours
                ),
                recommended_action: RecoveryAction::CancelStaleRun { run_id: id },
            });
        }

        Ok(stale)
    }

    /// Find orphaned worktree directories (exist on disk but have no DB record).
    ///
    /// Uses a single query to fetch all known worktree slugs per repo, avoiding N+1.
    pub fn find_orphaned_worktree_dirs(&self) -> Result<Vec<StuckState>> {
        let mut results = Vec::new();

        // Get all registered repo workspace dirs
        let mut repo_stmt = self
            .conn
            .prepare("SELECT id, slug, workspace_dir FROM repos WHERE workspace_dir IS NOT NULL")?;
        let repos: Vec<(String, String, String)> = {
            let rows =
                repo_stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row.map_err(ConductorError::Database)?);
            }
            v
        };

        // Prepare the worktree slug query once, reused per repo
        let mut wt_stmt = self
            .conn
            .prepare("SELECT slug FROM worktrees WHERE repo_id = ?1")?;

        for (repo_id, _repo_slug, workspace_dir) in repos {
            let ws_path = Path::new(&workspace_dir);
            if !ws_path.exists() {
                continue;
            }

            // Single query per repo: fetch all known worktree slugs
            let known_slugs: HashSet<String> = {
                let rows = wt_stmt.query_map(params![repo_id], |row| row.get(0))?;
                let mut set = HashSet::new();
                for row in rows {
                    set.insert(row.map_err(ConductorError::Database)?);
                }
                set
            };

            // List subdirs in the workspace dir
            let entries = match std::fs::read_dir(ws_path) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                if !known_slugs.contains(&dir_name) {
                    results.push(StuckState {
                        entity_type: "worktree_dir".to_string(),
                        entity_id: dir_name.clone(),
                        description: format!(
                            "Directory '{}' exists in {}/{} but has no DB record",
                            dir_name, workspace_dir, dir_name
                        ),
                        recommended_action: RecoveryAction::CleanupOrphanedWorktree {
                            path: path.to_string_lossy().to_string(),
                        },
                    });
                }
            }
        }

        Ok(results)
    }

    /// Execute a recovery action (Tier 1).
    pub fn execute(&self, action: &RecoveryAction) -> Result<String> {
        match action {
            RecoveryAction::CancelStaleRun { run_id } => {
                let wf_mgr = crate::workflow::WorkflowManager::new(self.conn);
                wf_mgr.cancel_run(run_id, "emergency recovery: stale run detected")?;
                Ok(format!("Cancelled stale workflow run {run_id}"))
            }
            RecoveryAction::CleanupOrphanedWorktree { path } => {
                // Safety: only remove if the directory is inside ~/.conductor/workspaces/
                let conductor_dir = crate::config::conductor_dir();
                let workspaces = conductor_dir.join("workspaces");
                if !Path::new(path).starts_with(&workspaces) {
                    return Err(ConductorError::InvalidInput(format!(
                        "refusing to delete path outside workspaces dir: {path}"
                    )));
                }
                std::fs::remove_dir_all(path)?;
                Ok(format!("Removed orphaned worktree directory: {path}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_config_defaults() {
        let config = RecoveryConfig::default();
        assert_eq!(config.stale_workflow_threshold_hours, 4);
    }

    #[test]
    fn stuck_state_display() {
        let state = StuckState {
            entity_type: "workflow_run".to_string(),
            entity_id: "run-1".to_string(),
            description: "Stuck for 5 hours".to_string(),
            recommended_action: RecoveryAction::CancelStaleRun {
                run_id: "run-1".to_string(),
            },
        };
        assert_eq!(state.entity_type, "workflow_run");
    }

    #[test]
    fn cleanup_rejects_path_outside_workspaces() {
        let conn = crate::test_helpers::setup_db();
        let config = Config::default();
        let mgr = RecoveryManager::new(&conn, &config);
        let result = mgr.execute(&RecoveryAction::CleanupOrphanedWorktree {
            path: "/tmp/evil/path".to_string(),
        });
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("refusing to delete"));
    }
}
