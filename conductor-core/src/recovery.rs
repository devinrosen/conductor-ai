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
use std::path::Path;

use crate::config::Config;
use crate::error::Result;

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
    /// Tier 1: Adopt an orphaned worktree directory into the DB.
    AdoptOrphanedWorktree {
        repo_slug: String,
        dir_name: String,
        path: String,
    },
    /// Tier 1: Clean up an orphaned worktree directory.
    CleanupOrphanedWorktree { path: String },
}

/// Scan for stuck workflow runs (status=running/waiting with stale updated_at).
pub fn find_stale_workflow_runs(
    conn: &Connection,
    recovery_config: &RecoveryConfig,
) -> Result<Vec<StuckState>> {
    let threshold =
        Utc::now() - chrono::Duration::hours(recovery_config.stale_workflow_threshold_hours);
    let threshold_str = threshold.to_rfc3339();

    let mut stmt = conn.prepare(
        "SELECT id, workflow_name, status, updated_at FROM workflow_runs
         WHERE status IN ('running', 'waiting')
         AND updated_at < ?1",
    )?;

    let stale: Vec<StuckState> = stmt
        .query_map(params![threshold_str], |row| {
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let status: String = row.get(2)?;
            let updated: String = row.get(3)?;
            Ok(StuckState {
                entity_type: "workflow_run".to_string(),
                entity_id: id.clone(),
                description: format!(
                    "Workflow '{}' has been {} since {} (>{} hours)",
                    name, status, updated, recovery_config.stale_workflow_threshold_hours
                ),
                recommended_action: RecoveryAction::CancelStaleRun { run_id: id },
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(stale)
}

/// Find orphaned worktree directories (exist on disk but have no DB record).
pub fn find_orphaned_worktree_dirs(conn: &Connection, _config: &Config) -> Result<Vec<StuckState>> {
    let mut results = Vec::new();

    // Get all registered repo workspace dirs
    let mut stmt =
        conn.prepare("SELECT slug, workspace_dir FROM repos WHERE workspace_dir IS NOT NULL")?;
    let repos: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (repo_slug, workspace_dir) in repos {
        let ws_path = Path::new(&workspace_dir);
        if !ws_path.exists() {
            continue;
        }

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

            // Check if this directory has a matching worktree record
            let has_record: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM worktrees WHERE repo_id = (SELECT id FROM repos WHERE slug = ?1) AND slug = ?2",
                    params![repo_slug, dir_name],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);

            if !has_record {
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
pub fn execute_recovery(conn: &Connection, action: &RecoveryAction) -> Result<String> {
    match action {
        RecoveryAction::CancelStaleRun { run_id } => {
            let wf_mgr = crate::workflow::WorkflowManager::new(conn);
            wf_mgr.cancel_run(run_id, "emergency recovery: stale run detected")?;
            Ok(format!("Cancelled stale workflow run {run_id}"))
        }
        RecoveryAction::CleanupOrphanedWorktree { path } => {
            // Safety: only remove if the directory is inside ~/.conductor/workspaces/
            let conductor_dir = crate::config::conductor_dir();
            let workspaces = conductor_dir.join("workspaces");
            if !Path::new(path).starts_with(&workspaces) {
                return Err(crate::error::ConductorError::InvalidInput(format!(
                    "refusing to delete path outside workspaces dir: {path}"
                )));
            }
            std::fs::remove_dir_all(path)?;
            Ok(format!("Removed orphaned worktree directory: {path}"))
        }
        RecoveryAction::AdoptOrphanedWorktree { .. } => {
            // Adoption requires more context; deferred to interactive recovery
            Ok(
                "Adoption is not yet implemented — use `conductor worktree create` instead"
                    .to_string(),
            )
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
        let result = execute_recovery(
            &conn,
            &RecoveryAction::CleanupOrphanedWorktree {
                path: "/tmp/evil/path".to_string(),
            },
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("refusing to delete"));
    }
}
