//! Checkpoint persistence for workflow runs.
//!
//! Writes checkpoint files alongside SQLite records for resume-from-failure.
//! Checkpoint files are predictable from the run ID alone:
//! `~/.conductor/checkpoints/<workflow_run_id>.json`
//!
//! Part of: checkpoint-persistence-protocol@1.2.0

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SCHEMA_VERSION: u32 = 1;

/// Checkpoint data snapshot for a workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub workflow_run_id: String,
    pub workflow_name: String,
    pub captured_at: String,
    pub process_state: ProcessState,
    pub progress: Progress,
    pub completed_step_keys: Vec<(String, u32)>,
    pub last_action: String,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessState {
    pub status: String,
    pub position: u32,
    pub iteration: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub total_steps: u32,
    pub completed: u32,
    pub failed: u32,
    pub pending: u32,
    pub running: u32,
    pub skipped: u32,
}

/// Resolve the checkpoint directory path.
pub fn checkpoint_dir() -> PathBuf {
    crate::config::conductor_dir().join("checkpoints")
}

/// Resolve the checkpoint file path for a given run ID.
pub fn checkpoint_path(workflow_run_id: &str) -> PathBuf {
    checkpoint_dir().join(format!("{workflow_run_id}.json"))
}

/// Write a checkpoint to disk.
pub fn write_checkpoint(checkpoint: &Checkpoint) -> std::io::Result<()> {
    let dir = checkpoint_dir();
    std::fs::create_dir_all(&dir)?;
    let path = checkpoint_path(&checkpoint.workflow_run_id);
    let json = serde_json::to_string_pretty(checkpoint).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)?;
    tracing::debug!(
        run_id = %checkpoint.workflow_run_id,
        path = %path.display(),
        "checkpoint written"
    );
    Ok(())
}

/// Read a checkpoint from disk, if it exists.
pub fn read_checkpoint(workflow_run_id: &str) -> std::io::Result<Option<Checkpoint>> {
    let path = checkpoint_path(workflow_run_id);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)?;
    let checkpoint: Checkpoint = serde_json::from_str(&data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(checkpoint))
}

/// Validate a checkpoint against the current run state.
///
/// Returns Ok(()) if valid, Err with reason if invalid.
pub fn validate_checkpoint(checkpoint: &Checkpoint, workflow_run_id: &str) -> Result<(), String> {
    if checkpoint.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "checkpoint schema version {} != expected {}",
            checkpoint.schema_version, SCHEMA_VERSION
        ));
    }
    if checkpoint.workflow_run_id != workflow_run_id {
        return Err(format!(
            "checkpoint run ID {} != requested {}",
            checkpoint.workflow_run_id, workflow_run_id
        ));
    }
    Ok(())
}

/// Remove a checkpoint file (e.g., after successful completion).
pub fn remove_checkpoint(workflow_run_id: &str) -> std::io::Result<()> {
    let path = checkpoint_path(workflow_run_id);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Build a checkpoint from the current workflow manager state.
///
/// Queries the DB to collect step statuses and progress.
/// This is `pub(crate)` to avoid leaking raw `&Connection` through the public API.
#[allow(dead_code)] // Will be called from engine.rs when checkpoint writes are wired in
pub(crate) fn build_checkpoint(
    conn: &rusqlite::Connection,
    workflow_run_id: &str,
    last_action: &str,
    next_action: Option<&str>,
) -> crate::error::Result<Checkpoint> {
    let (workflow_name, status, iteration): (String, String, u32) = conn.query_row(
        "SELECT workflow_name, status, COALESCE(iteration, 0) FROM workflow_runs WHERE id = ?1",
        rusqlite::params![workflow_run_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let mut stmt = conn.prepare(
        "SELECT step_name, status, COALESCE(iteration, 0) FROM workflow_run_steps WHERE workflow_run_id = ?1 ORDER BY position",
    )?;
    let steps: Vec<(String, String, u32)> = stmt
        .query_map(rusqlite::params![workflow_run_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let total = steps.len() as u32;
    let completed = steps.iter().filter(|(_, s, _)| s == "completed").count() as u32;
    let failed = steps.iter().filter(|(_, s, _)| s == "failed").count() as u32;
    let pending = steps.iter().filter(|(_, s, _)| s == "pending").count() as u32;
    let running = steps.iter().filter(|(_, s, _)| s == "running").count() as u32;
    let skipped = steps.iter().filter(|(_, s, _)| s == "skipped").count() as u32;
    let position = completed + failed + skipped;

    let completed_step_keys: Vec<(String, u32)> = steps
        .iter()
        .filter(|(_, s, _)| s == "completed")
        .map(|(name, _, iter)| (name.clone(), *iter))
        .collect();

    Ok(Checkpoint {
        schema_version: SCHEMA_VERSION,
        workflow_run_id: workflow_run_id.to_string(),
        workflow_name,
        captured_at: chrono::Utc::now().to_rfc3339(),
        process_state: ProcessState {
            status,
            position,
            iteration,
        },
        progress: Progress {
            total_steps: total,
            completed,
            failed,
            pending,
            running,
            skipped,
        },
        completed_step_keys,
        last_action: last_action.to_string(),
        next_action: next_action.map(String::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_roundtrip() {
        let cp = Checkpoint {
            schema_version: SCHEMA_VERSION,
            workflow_run_id: "test-run-1".to_string(),
            workflow_name: "test-wf".to_string(),
            captured_at: "2026-03-21T00:00:00Z".to_string(),
            process_state: ProcessState {
                status: "running".to_string(),
                position: 2,
                iteration: 0,
            },
            progress: Progress {
                total_steps: 5,
                completed: 2,
                failed: 0,
                pending: 3,
                running: 0,
                skipped: 0,
            },
            completed_step_keys: vec![("lint".to_string(), 0), ("test".to_string(), 0)],
            last_action: "Completed step 'test'".to_string(),
            next_action: Some("Execute step 'deploy'".to_string()),
        };

        let json = serde_json::to_string_pretty(&cp).unwrap();
        let parsed: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workflow_run_id, "test-run-1");
        assert_eq!(parsed.progress.completed, 2);
    }

    #[test]
    fn validate_checkpoint_correct() {
        let cp = Checkpoint {
            schema_version: SCHEMA_VERSION,
            workflow_run_id: "run-1".to_string(),
            workflow_name: "wf".to_string(),
            captured_at: "2026-03-21".to_string(),
            process_state: ProcessState {
                status: "completed".to_string(),
                position: 0,
                iteration: 0,
            },
            progress: Progress {
                total_steps: 0,
                completed: 0,
                failed: 0,
                pending: 0,
                running: 0,
                skipped: 0,
            },
            completed_step_keys: vec![],
            last_action: "done".to_string(),
            next_action: None,
        };
        assert!(validate_checkpoint(&cp, "run-1").is_ok());
    }

    #[test]
    fn validate_checkpoint_wrong_run_id() {
        let cp = Checkpoint {
            schema_version: SCHEMA_VERSION,
            workflow_run_id: "run-1".to_string(),
            workflow_name: "wf".to_string(),
            captured_at: "2026-03-21".to_string(),
            process_state: ProcessState {
                status: "completed".to_string(),
                position: 0,
                iteration: 0,
            },
            progress: Progress {
                total_steps: 0,
                completed: 0,
                failed: 0,
                pending: 0,
                running: 0,
                skipped: 0,
            },
            completed_step_keys: vec![],
            last_action: "done".to_string(),
            next_action: None,
        };
        assert!(validate_checkpoint(&cp, "run-2").is_err());
    }

    #[test]
    fn validate_checkpoint_wrong_schema_version() {
        let cp = Checkpoint {
            schema_version: 999,
            workflow_run_id: "run-1".to_string(),
            workflow_name: "wf".to_string(),
            captured_at: "2026-03-21".to_string(),
            process_state: ProcessState {
                status: "completed".to_string(),
                position: 0,
                iteration: 0,
            },
            progress: Progress {
                total_steps: 0,
                completed: 0,
                failed: 0,
                pending: 0,
                running: 0,
                skipped: 0,
            },
            completed_step_keys: vec![],
            last_action: "done".to_string(),
            next_action: None,
        };
        assert!(validate_checkpoint(&cp, "run-1").is_err());
    }

    #[test]
    fn checkpoint_path_predictable() {
        let path = checkpoint_path("01HXYZ");
        assert!(path.to_string_lossy().contains("01HXYZ.json"));
        assert!(path.to_string_lossy().contains("checkpoints"));
    }
}
