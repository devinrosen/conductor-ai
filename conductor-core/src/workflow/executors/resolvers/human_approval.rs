use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};

use crate::error::{ConductorError, Result};
use crate::workflow::executors::gate_resolver::{GateContext, GateParams, GatePoll, GateResolver};
use crate::workflow::status::WorkflowStepStatus;

/// Distinguishes the two human gate types so a single struct can register
/// under both `"human_approval"` and `"human_review"`.
pub(in crate::workflow::executors) enum HumanGateKind {
    HumanApproval,
    HumanReview,
}

pub(in crate::workflow::executors) struct HumanApprovalGateResolver {
    conn: Mutex<Connection>,
    kind: HumanGateKind,
}

impl HumanApprovalGateResolver {
    pub(in crate::workflow::executors) fn new(db_path: PathBuf, kind: HumanGateKind) -> Self {
        let conn = Connection::open(&db_path).unwrap_or_else(|e| {
            panic!(
                "HumanApprovalGateResolver: failed to open DB at {}: {e}",
                db_path.display()
            )
        });
        // Mirror the WAL mode and FK settings used by the main connection.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", true).ok();
        Self {
            conn: Mutex::new(conn),
            kind,
        }
    }
}

impl GateResolver for HumanApprovalGateResolver {
    fn gate_type(&self) -> &str {
        match self.kind {
            HumanGateKind::HumanApproval => "human_approval",
            HumanGateKind::HumanReview => "human_review",
        }
    }

    fn poll(&self, _run_id: &str, params: &GateParams, _ctx: &GateContext<'_>) -> Result<GatePoll> {
        let conn = self.conn.lock().map_err(|_| {
            ConductorError::Workflow("HumanApprovalGateResolver: mutex poisoned".into())
        })?;

        // Read the current state of the gate step directly by its ID.
        let row: Option<(Option<String>, String, Option<String>)> = conn
            .query_row(
                "SELECT gate_approved_at, status, gate_feedback \
                 FROM workflow_run_steps WHERE id = ?1",
                rusqlite::params![params.step_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(ConductorError::Database)?;

        if let Some((approved_at, status_str, feedback)) = row {
            let status = status_str
                .parse::<WorkflowStepStatus>()
                .unwrap_or(WorkflowStepStatus::Waiting);
            if approved_at.is_some() || status == WorkflowStepStatus::Completed {
                return Ok(GatePoll::Approved(feedback));
            }
            if status == WorkflowStepStatus::Failed {
                return Ok(GatePoll::Rejected(format!(
                    "Gate '{}' rejected",
                    params.gate_name
                )));
            }
        }

        Ok(GatePoll::Pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::executors::gate_resolver::{GateContext, GateParams, GitHubTokenCache};
    use crate::workflow_dsl::ApprovalMode;
    use rusqlite::Connection;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    fn setup_test_db(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_run_steps (
                id TEXT PRIMARY KEY,
                workflow_run_id TEXT NOT NULL,
                step_name TEXT NOT NULL,
                role TEXT NOT NULL,
                position INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'pending',
                iteration INTEGER NOT NULL DEFAULT 0,
                gate_type TEXT,
                gate_prompt TEXT,
                gate_timeout TEXT,
                gate_approved_at TEXT,
                gate_approved_by TEXT,
                gate_feedback TEXT,
                gate_options TEXT,
                gate_selections TEXT,
                started_at TEXT,
                ended_at TEXT,
                result_text TEXT,
                context_out TEXT,
                markers_out TEXT,
                condition_expr TEXT,
                condition_met INTEGER,
                can_commit INTEGER NOT NULL DEFAULT 0,
                child_run_id TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0,
                structured_output TEXT,
                output_file TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_input_tokens INTEGER,
                cache_creation_input_tokens INTEGER,
                fan_out_total INTEGER,
                fan_out_completed INTEGER NOT NULL DEFAULT 0,
                fan_out_failed INTEGER NOT NULL DEFAULT 0,
                fan_out_skipped INTEGER NOT NULL DEFAULT 0,
                step_error TEXT,
                parallel_group_id TEXT
            );",
        )
        .unwrap();
    }

    fn make_test_params(step_id: &str) -> GateParams {
        GateParams {
            gate_name: "test-gate".into(),
            prompt: None,
            min_approvals: 1,
            approval_mode: ApprovalMode::default(),
            options: vec![],
            timeout_secs: 3600,
            bot_name: None,
            step_id: step_id.into(),
        }
    }

    fn make_test_ctx<'a>(
        config: &'a crate::config::Config,
        db_path: &'a std::path::Path,
    ) -> GateContext<'a> {
        GateContext {
            working_dir: "/tmp",
            config,
            default_bot_name: None,
            token_cache: Arc::new(GitHubTokenCache::new(None)),
            db_path,
        }
    }

    #[test]
    fn test_human_approval_resolver_approved_when_gate_approved_at_set() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        // Set up the DB schema and insert an approved step.
        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type, gate_approved_at) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'completed', 0, 'human_approval', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();
        drop(conn);

        let resolver =
            HumanApprovalGateResolver::new(db_path.clone(), HumanGateKind::HumanApproval);
        let config = crate::config::Config::default();
        let params = make_test_params("step1");
        let ctx = make_test_ctx(&config, &db_path);

        let result = resolver.poll("run1", &params, &ctx).unwrap();
        assert!(
            matches!(result, GatePoll::Approved(_)),
            "expected Approved when gate_approved_at is set"
        );
    }

    #[test]
    fn test_human_approval_resolver_rejected_when_status_failed() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'failed', 0, 'human_approval')",
            [],
        ).unwrap();
        drop(conn);

        let resolver =
            HumanApprovalGateResolver::new(db_path.clone(), HumanGateKind::HumanApproval);
        let config = crate::config::Config::default();
        let params = make_test_params("step1");
        let ctx = make_test_ctx(&config, &db_path);

        let result = resolver.poll("run1", &params, &ctx).unwrap();
        assert!(
            matches!(result, GatePoll::Rejected(_)),
            "expected Rejected when status is failed"
        );
    }

    #[test]
    fn test_human_approval_resolver_pending_when_waiting() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'waiting', 0, 'human_approval')",
            [],
        ).unwrap();
        drop(conn);

        let resolver =
            HumanApprovalGateResolver::new(db_path.clone(), HumanGateKind::HumanApproval);
        let config = crate::config::Config::default();
        let params = make_test_params("step1");
        let ctx = make_test_ctx(&config, &db_path);

        let result = resolver.poll("run1", &params, &ctx).unwrap();
        assert!(
            matches!(result, GatePoll::Pending),
            "expected Pending when step is still waiting"
        );
    }

    #[test]
    fn test_human_review_resolver_gate_type() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();
        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        drop(conn);

        let resolver = HumanApprovalGateResolver::new(db_path, HumanGateKind::HumanReview);
        assert_eq!(resolver.gate_type(), "human_review");
    }
}
