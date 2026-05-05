use std::sync::Arc;

use runkon_flow::engine_error::EngineError;
use runkon_flow::traits::gate_resolver::{GateParams, GatePoll, GateResolver};
use runkon_flow::traits::persistence::{GateApprovalState, WorkflowPersistence};
use runkon_flow::traits::run_context::RunContext;

/// Distinguishes the two human gate types so a single struct can register
/// under both `"human_approval"` and `"human_review"`.
#[allow(dead_code)]
pub(in crate::workflow) enum HumanGateKind {
    HumanApproval,
    HumanReview,
}

#[allow(dead_code)]
pub(in crate::workflow) struct HumanApprovalGateResolver {
    persistence: Arc<dyn WorkflowPersistence>,
    kind: HumanGateKind,
}

#[allow(dead_code)]
impl HumanApprovalGateResolver {
    pub(in crate::workflow) fn new(
        persistence: Arc<dyn WorkflowPersistence>,
        kind: HumanGateKind,
    ) -> Self {
        Self { persistence, kind }
    }
}

impl GateResolver for HumanApprovalGateResolver {
    fn gate_type(&self) -> &str {
        match self.kind {
            HumanGateKind::HumanApproval => "human_approval",
            HumanGateKind::HumanReview => "human_review",
        }
    }

    fn poll(
        &self,
        _run_id: &str,
        params: &GateParams,
        _ctx: &dyn RunContext,
    ) -> Result<GatePoll, EngineError> {
        let state = self.persistence.get_gate_approval(&params.step_id)?;
        match state {
            GateApprovalState::Approved { feedback, .. } => Ok(GatePoll::Approved(feedback)),
            GateApprovalState::Rejected { feedback } => {
                Ok(GatePoll::Rejected(feedback.unwrap_or_else(|| {
                    format!("Gate '{}' rejected", params.gate_name)
                })))
            }
            GateApprovalState::Pending => Ok(GatePoll::Pending),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::persistence_sqlite::SqliteWorkflowPersistence;
    use runkon_flow::dsl::ApprovalMode;
    use runkon_flow::traits::gate_resolver::GateParams;
    use runkon_flow::traits::persistence::WorkflowPersistence;
    use rusqlite::Connection;
    use std::collections::HashMap;
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
            options: HashMap::new(),
            timeout_secs: 3600,
            bot_name: None,
            step_id: step_id.into(),
        }
    }

    struct NoopCtx;
    impl RunContext for NoopCtx {
        fn injected_variables(&self) -> HashMap<&'static str, String> {
            Default::default()
        }
        fn working_dir(&self) -> &std::path::Path {
            std::path::Path::new("/tmp")
        }
        fn run_id(&self) -> &str {
            "noop-run"
        }
        fn workflow_name(&self) -> &str {
            "noop-wf"
        }
    }

    fn make_persistence(db_path: &std::path::Path) -> Arc<dyn WorkflowPersistence> {
        Arc::new(
            SqliteWorkflowPersistence::open(db_path)
                .expect("failed to open test DB for HumanApprovalGateResolver"),
        )
    }

    #[test]
    fn test_human_approval_resolver_approved_when_gate_approved_at_set() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        insert_test_step(
            &conn,
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type, gate_approved_at) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'completed', 0, 'human_approval', '2025-01-01T00:00:00Z')",
        );
        drop(conn);

        let persistence = make_persistence(&db_path);
        let resolver = HumanApprovalGateResolver::new(persistence, HumanGateKind::HumanApproval);
        let params = make_test_params("step1");

        let result = resolver.poll("run1", &params, &NoopCtx).unwrap();
        assert!(
            matches!(result, GatePoll::Approved(_)),
            "expected Approved when gate_approved_at is set"
        );
    }

    #[test]
    fn test_human_approval_resolver_rejected_uses_fallback_when_no_feedback() {
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

        let persistence = make_persistence(&db_path);
        let resolver = HumanApprovalGateResolver::new(persistence, HumanGateKind::HumanApproval);
        let params = make_test_params("step1");

        let result = resolver.poll("run1", &params, &NoopCtx).unwrap();
        match result {
            GatePoll::Rejected(msg) => {
                assert_eq!(msg, "Gate 'test-gate' rejected");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn test_human_approval_resolver_rejected_surfaces_stored_feedback() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        insert_test_step(
            &conn,
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, position, status, iteration, gate_type, gate_feedback) \
             VALUES ('step1', 'run1', 'test-gate', 'gate', 0, 'failed', 0, 'human_approval', 'needs more work')",
        );
        drop(conn);

        let persistence = make_persistence(&db_path);
        let resolver = HumanApprovalGateResolver::new(persistence, HumanGateKind::HumanApproval);
        let params = make_test_params("step1");

        let result = resolver.poll("run1", &params, &NoopCtx).unwrap();
        match result {
            GatePoll::Rejected(msg) => {
                assert_eq!(msg, "needs more work");
            }
            other => panic!("expected Rejected with feedback, got {other:?}"),
        }
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

        let persistence = make_persistence(&db_path);
        let resolver = HumanApprovalGateResolver::new(persistence, HumanGateKind::HumanApproval);
        let params = make_test_params("step1");

        let result = resolver.poll("run1", &params, &NoopCtx).unwrap();
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

        let persistence = make_persistence(&db_path);
        let resolver = HumanApprovalGateResolver::new(persistence, HumanGateKind::HumanReview);
        assert_eq!(resolver.gate_type(), "human_review");
    }

    #[test]
    fn test_human_approval_resolver_pending_when_step_not_found() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();

        let conn = Connection::open(&db_path).unwrap();
        setup_test_db(&conn);
        drop(conn);

        let persistence = make_persistence(&db_path);
        let resolver = HumanApprovalGateResolver::new(persistence, HumanGateKind::HumanApproval);
        let params = make_test_params("nonexistent-step-id");

        let result = resolver.poll("run1", &params, &NoopCtx).unwrap();
        assert!(
            matches!(result, GatePoll::Pending),
            "expected Pending when step_id does not exist in DB"
        );
    }
}
