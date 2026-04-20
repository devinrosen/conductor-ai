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
    db_path: PathBuf,
    conn: Mutex<Option<Connection>>,
    kind: HumanGateKind,
}

impl HumanApprovalGateResolver {
    pub(in crate::workflow::executors) fn new(db_path: PathBuf, kind: HumanGateKind) -> Self {
        Self {
            db_path,
            conn: Mutex::new(None),
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
        let mut guard = self.conn.lock().map_err(|_| {
            ConductorError::Workflow("HumanApprovalGateResolver: mutex poisoned".into())
        })?;

        // Lazily open the connection on first use.
        if guard.is_none() {
            let conn = Connection::open(&self.db_path).map_err(|e| {
                ConductorError::Workflow(format!(
                    "HumanApprovalGateResolver: failed to open DB at {}: {e}",
                    self.db_path.display()
                ))
            })?;
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(|e| {
                    ConductorError::Workflow(format!(
                        "HumanApprovalGateResolver: failed to set journal_mode WAL: {e}"
                    ))
                })?;
            conn.pragma_update(None, "foreign_keys", true)
                .map_err(|e| {
                    ConductorError::Workflow(format!(
                        "HumanApprovalGateResolver: failed to enable foreign_keys: {e}"
                    ))
                })?;
            *guard = Some(conn);
        }

        let conn = guard.as_ref().expect("connection was just set");

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
        crate::db::migrations::run(conn).expect("migrations should run successfully");
    }

    /// Insert a workflow_run_steps row for testing.
    ///
    /// FK enforcement is temporarily disabled so the test does not need to
    /// create a full parent chain (repos → worktrees → agent_runs → workflow_runs).
    fn insert_test_step(conn: &Connection, sql: &str) {
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(sql, []).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
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
        insert_test_step(
            &conn,
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type, gate_approved_at) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'completed', 0, 'human_approval', '2025-01-01T00:00:00Z')",
        );
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
        insert_test_step(
            &conn,
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'failed', 0, 'human_approval')",
        );
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
        insert_test_step(
            &conn,
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'waiting', 0, 'human_approval')",
        );
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

    #[test]
    fn test_human_approval_resolver_pending_when_step_not_found() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        drop(conn);

        let resolver =
            HumanApprovalGateResolver::new(db_path.clone(), HumanGateKind::HumanApproval);
        let config = crate::config::Config::default();
        let params = make_test_params("nonexistent-step-id");
        let ctx = make_test_ctx(&config, &db_path);

        let result = resolver.poll("run1", &params, &ctx).unwrap();
        assert!(
            matches!(result, GatePoll::Pending),
            "expected Pending when step_id does not exist in DB"
        );
    }
}
