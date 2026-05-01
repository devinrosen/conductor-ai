//! Backwards-compat re-export of the canonical implementation that now lives in
//! `runkon_flow::persistence_sqlite` (Phase 4 step 4.3).
//!
//! Existing call sites import [`SqliteWorkflowPersistence`] from
//! `crate::workflow` — keeping the path stable here lets the move land without
//! ripple changes through the engine, CLI, TUI, and web layers. The integration
//! tests below verify the runkon-flow implementation against conductor's actual
//! schema (migrations + helper tables) rather than re-testing trait semantics.

pub use runkon_flow::persistence_sqlite::SqliteWorkflowPersistence;

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use runkon_flow::traits::persistence::{
        GateApprovalState, NewRun, NewStep, WorkflowPersistence,
    };

    use crate::agent::AgentManager;
    use crate::workflow::WorkflowRunStatus;

    use super::SqliteWorkflowPersistence;

    fn make_persistence() -> (SqliteWorkflowPersistence, String) {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
        let shared = Arc::new(Mutex::new(conn));
        (
            SqliteWorkflowPersistence::from_shared_connection(shared),
            parent.id,
        )
    }

    fn make_new_run(parent_run_id: String) -> NewRun {
        NewRun {
            workflow_name: "test-wf".to_string(),
            worktree_id: Some("w1".to_string()),
            ticket_id: None,
            repo_id: None,
            parent_run_id,
            dry_run: false,
            trigger: "manual".to_string(),
            definition_snapshot: None,
            parent_workflow_run_id: None,
            target_label: None,
        }
    }

    #[test]
    fn get_gate_approval_returns_pending_for_unknown_step() {
        let (p, _) = make_persistence();
        let result = p.get_gate_approval("nonexistent-step");
        assert!(matches!(result, Ok(GateApprovalState::Pending)));
    }

    #[test]
    fn create_run_and_get_run_roundtrip() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        assert_eq!(run.workflow_name, "test-wf");
        let fetched = p.get_run(&run.id).unwrap();
        assert_eq!(fetched.map(|r| r.id), Some(run.id));
    }

    #[test]
    fn approve_gate_then_get_approval_returns_approved() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id,
                step_name: "approval-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();
        p.approve_gate(&step_id, "human", Some("looks good"), None)
            .unwrap();
        let state = p.get_gate_approval(&step_id).unwrap();
        assert!(matches!(state, GateApprovalState::Approved { .. }));
    }

    #[test]
    fn reject_gate_then_get_approval_returns_rejected() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id,
                step_name: "review-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();
        p.reject_gate(&step_id, "human", Some("needs work"))
            .unwrap();
        let state = p.get_gate_approval(&step_id).unwrap();
        assert!(matches!(state, GateApprovalState::Rejected { .. }));
    }

    #[test]
    fn update_run_status_roundtrip() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Running, None, None)
            .unwrap();
        let active = p.list_active_runs(&[WorkflowRunStatus::Running]).unwrap();
        assert!(active.iter().any(|r| r.id == run.id));
    }

    #[test]
    fn from_shared_connection_creates_working_persistence() {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

        let shared = Arc::new(Mutex::new(conn));
        let p = SqliteWorkflowPersistence::from_shared_connection(Arc::clone(&shared));

        let run = p.create_run(make_new_run(parent.id)).unwrap();
        assert_eq!(run.workflow_name, "test-wf");

        let fetched = p.get_run(&run.id).unwrap();
        assert!(
            fetched.is_some(),
            "run should be retrievable after creation"
        );
    }

    #[test]
    fn from_shared_connection_shares_state_with_raw_connection() {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

        let shared = Arc::new(Mutex::new(conn));
        let p = SqliteWorkflowPersistence::from_shared_connection(Arc::clone(&shared));

        let run = p.create_run(make_new_run(parent.id)).unwrap();

        // Verify state is visible through the shared connection handle too.
        let guard = shared.lock().unwrap();
        let mgr = crate::workflow::manager::WorkflowManager::new(&guard);
        let found = crate::workflow::get_workflow_run(&guard, &run.id).unwrap();
        assert!(
            found.is_some(),
            "run written via persistence should be visible through shared conn"
        );
    }

    #[test]
    fn is_run_cancelled_returns_true_for_cancelled_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Cancelled, None, None)
            .unwrap();
        assert!(p.is_run_cancelled(&run.id).unwrap());
    }

    #[test]
    fn is_run_cancelled_returns_true_for_cancelling_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Cancelling, None, None)
            .unwrap();
        assert!(p.is_run_cancelled(&run.id).unwrap());
    }

    #[test]
    fn is_run_cancelled_returns_false_for_running_status() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        p.update_run_status(&run.id, WorkflowRunStatus::Running, None, None)
            .unwrap();
        assert!(!p.is_run_cancelled(&run.id).unwrap());
    }

    #[test]
    fn is_run_cancelled_returns_false_for_nonexistent_run() {
        let (p, _) = make_persistence();
        assert!(!p.is_run_cancelled("nonexistent-run-id").unwrap());
    }

    /// `persist_metrics` must land cost_usd in `total_cost_usd` and num_turns in
    /// `total_turns`. Guards against future signature drift between trait and SQL.
    #[test]
    fn persist_metrics_maps_cost_and_turns_to_correct_columns() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();

        let cost_usd = 42.5_f64;
        let num_turns = 7_i64;

        p.persist_metrics(&run.id, 0, 0, 0, 0, cost_usd, num_turns, 1000)
            .unwrap();

        let fetched = p.get_run(&run.id).unwrap().expect("run should exist");
        assert_eq!(
            fetched.total_cost_usd,
            Some(cost_usd),
            "total_cost_usd should match the cost_usd argument"
        );
        assert_eq!(
            fetched.total_turns,
            Some(num_turns),
            "total_turns should match the num_turns argument"
        );
    }

    /// `approve_gate` with non-empty `selections` must write `context_out` to the step row.
    #[test]
    fn approve_gate_with_selections_sets_context_out() {
        let conn = crate::test_helpers::setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();

        let shared = Arc::new(Mutex::new(conn));
        let p = SqliteWorkflowPersistence::from_shared_connection(Arc::clone(&shared));

        let run = p.create_run(make_new_run(parent.id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id.clone(),
                step_name: "review-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();

        // Configure gate_options so validation passes for the selections.
        // The validation expects an array of objects with a "value" key.
        {
            let conn = shared.lock().unwrap();
            let mgr = crate::workflow::manager::WorkflowManager::new(&conn);
            mgr.set_step_gate_options(
                &step_id,
                r#"[{"value":"item-a"},{"value":"item-b"},{"value":"item-c"}]"#,
            )
            .unwrap();
        }

        let selections = vec!["item-a".to_string(), "item-b".to_string()];
        p.approve_gate(&step_id, "human", None, Some(&selections))
            .unwrap();

        let steps = p.get_steps(&run.id).unwrap();
        let step = steps.iter().find(|s| s.id == step_id).unwrap();
        let context_out = step
            .context_out
            .as_deref()
            .expect("context_out should be set when selections are provided");
        assert!(
            context_out.contains("item-a"),
            "context_out should contain the first selection; got: {context_out:?}"
        );
        assert!(
            context_out.contains("item-b"),
            "context_out should contain the second selection; got: {context_out:?}"
        );
    }

    /// `approve_gate` with `selections = Some(&[])` (empty slice) must NOT set
    /// `context_out` — the persistence layer filters empty selections out.
    #[test]
    fn approve_gate_with_empty_selections_sets_no_context_out() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id.clone(),
                step_name: "review-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();

        p.approve_gate(&step_id, "human", None, Some(&[])).unwrap();

        let steps = p.get_steps(&run.id).unwrap();
        let step = steps.iter().find(|s| s.id == step_id).unwrap();
        assert!(
            step.context_out.is_none(),
            "context_out should be None for empty selections; got: {:?}",
            step.context_out
        );
    }

    /// `get_gate_approval` must preserve `feedback` on the `Approved` variant.
    #[test]
    fn get_gate_approval_approved_preserves_feedback() {
        let (p, parent_id) = make_persistence();
        let run = p.create_run(make_new_run(parent_id)).unwrap();
        let step_id = p
            .insert_step(NewStep {
                workflow_run_id: run.id,
                step_name: "approval-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();

        p.approve_gate(&step_id, "human", Some("lgtm"), None)
            .unwrap();

        let state = p.get_gate_approval(&step_id).unwrap();
        match state {
            GateApprovalState::Approved {
                feedback,
                selections,
            } => {
                assert_eq!(
                    feedback,
                    Some("lgtm".to_string()),
                    "feedback must survive the approve_gate/get_gate_approval roundtrip"
                );
                assert!(
                    selections.is_none(),
                    "selections should be None when not provided"
                );
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn open_creates_usable_connection_at_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.db");
        // Apply conductor migrations so the workflow tables exist.
        crate::db::open_database(&path).expect("migrations should apply");
        // open() must succeed and produce a functional persistence instance.
        let p = SqliteWorkflowPersistence::open(&path).expect("open() should succeed");
        // A get on a nonexistent run returns Ok(None), proving the connection is live.
        assert!(
            p.get_run("nonexistent-run-id").unwrap().is_none(),
            "get_run on a fresh DB should return None for unknown IDs"
        );
    }
}
