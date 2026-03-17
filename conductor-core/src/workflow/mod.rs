//! Workflow engine: execute multi-step workflow definitions with conditional
//! branching, loops, parallel execution, gates, and actor/reviewer agent roles.
//!
//! Builds on top of the existing `AgentManager` and orchestrator infrastructure,
//! adding workflow-level tracking in `workflow_runs` / `workflow_run_steps`.

pub(crate) mod constants;
pub(crate) mod engine;
pub(crate) mod executors;
pub(crate) mod helpers;
pub(crate) mod manager;
pub(crate) mod output;
pub(crate) mod prompt_builder;
pub(crate) mod status;
pub(crate) mod types;

// Re-export DSL types so consumers go through `workflow::` instead of `workflow_dsl::` directly.
pub use crate::workflow_dsl::{
    collect_agent_names, collect_workflow_refs, detect_workflow_cycles, parse_workflow_str,
    validate_script_steps, validate_workflow_semantics, AgentRef, AlwaysNode, CallNode,
    CallWorkflowNode, Condition, DoNode, DoWhileNode, GateNode, GateType, IfNode, InputDecl,
    InputType, ParallelNode, UnlessNode, ValidationError, ValidationReport, WhileNode, WorkflowDef,
    WorkflowNode, WorkflowTrigger, WorkflowWarning, MAX_WORKFLOW_DEPTH,
};

// Re-export all public types and functions to preserve existing import paths.
pub use constants::CONDUCTOR_OUTPUT_INSTRUCTION;
pub use engine::ENGINE_INJECTED_KEYS;
pub use engine::{
    apply_workflow_input_defaults, execute_workflow, execute_workflow_standalone, resume_workflow,
    resume_workflow_standalone, validate_resume_preconditions,
};
pub use manager::WorkflowManager;
pub use output::{parse_conductor_output, ConductorOutput};
pub use status::{WorkflowRunStatus, WorkflowStepStatus};
pub use types::{
    ActiveWorkflowCounts, ContextEntry, MetadataEntry, RunIdSlot, StepResult, WorkflowExecConfig,
    WorkflowExecInput, WorkflowExecStandalone, WorkflowResult, WorkflowResumeInput,
    WorkflowResumeStandalone, WorkflowRun, WorkflowRunContext, WorkflowRunStep,
    WorkflowStepSummary,
};

use crate::agent_config::AgentSpec;

/// Convert a DSL `AgentRef` to the `agent_config` layer's `AgentSpec`.
///
/// This is the boundary where the workflow DSL concern (`AgentRef`) maps to
/// the resolution concern (`AgentSpec`).
impl From<&AgentRef> for AgentSpec {
    fn from(r: &AgentRef) -> Self {
        match r {
            AgentRef::Name(s) => AgentSpec::Name(s.clone()),
            AgentRef::Path(s) => AgentSpec::Path(s.clone()),
        }
    }
}
#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use rusqlite::{params, Connection};

    use crate::agent::{AgentManager, AgentRunStatus};
    use crate::config::Config;
    use crate::schema_config::OutputSchema;
    use crate::workflow_dsl::{
        AgentRef, ApprovalMode, CallNode, DoNode, DoWhileNode, GateNode, GateType, IfNode,
        OnMaxIter, OnTimeout, ParallelNode, UnlessNode, WhileNode,
    };

    use std::time::Duration;

    use crate::agent_runtime;
    use crate::error::ConductorError;
    use crate::schema_config;
    use crate::workflow_dsl::WorkflowNode;

    use super::engine::{
        bubble_up_child_step_results, completed_keys_from_steps, fetch_child_final_output,
        resolve_child_inputs, restore_completed_step, ExecutionState, ResumeContext,
    };
    use super::executors::{
        execute_call, execute_do, execute_do_while, execute_unless, execute_while,
        handle_gate_timeout,
    };
    use super::helpers::find_max_completed_while_iteration;
    use super::manager::WorkflowManager;
    use super::output::{interpret_agent_output, parse_conductor_output};
    use super::prompt_builder::{build_variable_map, substitute_variables};
    use super::status::{WorkflowRunStatus, WorkflowStepStatus};
    use super::types::{
        ContextEntry, MetadataEntry, StepKey, StepResult, WorkflowExecConfig, WorkflowExecInput,
        WorkflowResumeInput, WorkflowRun, WorkflowRunStep,
    };
    use super::*;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    /// Set a step's status without touching any optional fields.
    fn set_step_status(mgr: &WorkflowManager, step_id: &str, status: WorkflowStepStatus) {
        mgr.update_step_status(step_id, status, None, None, None, None, None)
            .unwrap();
    }

    #[test]
    fn test_create_workflow_run() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run(
                "test-coverage",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                None,
            )
            .unwrap();

        assert_eq!(run.workflow_name, "test-coverage");
        assert_eq!(run.status, WorkflowRunStatus::Pending);
        assert!(!run.dry_run);
    }

    #[test]
    fn test_create_workflow_run_with_snapshot() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run(
                "test",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some(r#"{"name":"test"}"#),
            )
            .unwrap();

        let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.definition_snapshot.as_deref(),
            Some(r#"{"name":"test"}"#)
        );
    }

    #[test]
    fn test_create_workflow_run_with_repo_id_round_trip() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run_with_targets(
                "test-wf",
                Some("w1"),
                None,
                Some("r1"),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();

        // Verify the struct returned by create reflects the inputs.
        assert_eq!(run.repo_id.as_deref(), Some("r1"));
        assert_eq!(run.ticket_id, None);

        // Read back from DB and assert columns are persisted correctly.
        let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.repo_id.as_deref(), Some("r1"));
        assert_eq!(fetched.ticket_id, None);
    }

    #[test]
    fn test_active_run_counts_by_repo_empty() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let counts = mgr.active_run_counts_by_repo().unwrap();
        assert!(
            counts.is_empty(),
            "expected no counts with no workflow runs"
        );
    }

    #[test]
    fn test_active_run_counts_by_repo_with_runs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(&conn);

        // Create one pending and one running run for repo r1.
        let run1 = mgr
            .create_workflow_run_with_targets(
                "wf-a",
                Some("w1"),
                None,
                Some("r1"),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();
        // Advance run1 to running.
        conn.execute(
            "UPDATE workflow_runs SET status = 'running' WHERE id = ?1",
            [&run1.id],
        )
        .unwrap();
        let _run2 = mgr
            .create_workflow_run_with_targets(
                "wf-b",
                Some("w1"),
                None,
                Some("r1"),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();
        // run2 stays at pending (default).

        let counts = mgr.active_run_counts_by_repo().unwrap();
        let c = counts.get("r1").expect("r1 should be in map");
        assert_eq!(c.running, 1, "expected 1 running");
        assert_eq!(c.pending, 1, "expected 1 pending");
        assert_eq!(c.waiting, 0, "expected 0 waiting");
    }

    #[test]
    fn test_active_run_counts_by_repo_excludes_completed() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(&conn);

        let run = mgr
            .create_workflow_run_with_targets(
                "wf-done",
                Some("w1"),
                None,
                Some("r1"),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET status = 'completed' WHERE id = ?1",
            [&run.id],
        )
        .unwrap();

        let counts = mgr.active_run_counts_by_repo().unwrap();
        assert!(
            !counts.contains_key("r1"),
            "completed runs must not appear in active counts"
        );
    }

    #[test]
    fn test_create_workflow_run_with_ticket_id_round_trip() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        insert_test_ticket(&conn, "tkt-rt-1", "r1");

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run_with_targets(
                "test-wf",
                None,
                Some("tkt-rt-1"),
                None,
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();

        // Verify the struct returned by create reflects the inputs.
        assert_eq!(run.ticket_id.as_deref(), Some("tkt-rt-1"));
        assert_eq!(run.repo_id, None);

        // Read back from DB and assert columns are persisted correctly.
        let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.ticket_id.as_deref(), Some("tkt-rt-1"));
        assert_eq!(fetched.repo_id, None);
    }

    #[test]
    fn test_insert_step_with_iteration() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 2)
            .unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, step_id);
        assert_eq!(steps[0].step_name, "review");
        assert_eq!(steps[0].iteration, 2);
    }

    #[test]
    fn test_update_step_with_markers() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 0)
            .unwrap();

        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("Found issues"),
            Some("2 issues in lib.rs"),
            Some(r#"["has_review_issues"]"#),
            Some(0),
        )
        .unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].context_out.as_deref(), Some("2 issues in lib.rs"));
        assert_eq!(
            steps[0].markers_out.as_deref(),
            Some(r#"["has_review_issues"]"#)
        );
    }

    #[test]
    fn test_update_step_status_full_with_structured_output() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 0)
            .unwrap();

        let structured_json = r#"{"approved":true,"summary":"All good"}"#;
        mgr.update_step_status_full(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("result text"),
            Some("All good"),
            Some(r#"[]"#),
            Some(0),
            Some(structured_json),
        )
        .unwrap();

        let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
        assert_eq!(step.structured_output.as_deref(), Some(structured_json));
        assert_eq!(step.context_out.as_deref(), Some("All good"));
        assert_eq!(step.result_text.as_deref(), Some("result text"));
    }

    #[test]
    fn test_update_step_status_full_without_structured_output() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 0)
            .unwrap();

        mgr.update_step_status_full(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("result text"),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
        assert!(step.structured_output.is_none());
    }

    #[test]
    fn test_gate_approve() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "human_review", "reviewer", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human_review", Some("Review?"), "48h")
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        // Find waiting gate
        let waiting = mgr.find_waiting_gate(&run.id).unwrap();
        assert!(waiting.is_some());
        assert_eq!(waiting.unwrap().id, step_id);

        // Approve
        mgr.approve_gate(&step_id, "user", Some("Looks good!"))
            .unwrap();

        // Verify
        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
        assert!(steps[0].gate_approved_at.is_some());
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("user"));
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("Looks good!"));
    }

    #[test]
    fn test_gate_reject() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "human_approval", "reviewer", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human_approval", Some("Approve?"), "24h")
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        mgr.reject_gate(&step_id, "user", None).unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
    }

    fn make_gate_node(gate_type: GateType, on_timeout: OnTimeout) -> GateNode {
        GateNode {
            name: "test_gate".to_string(),
            gate_type,
            prompt: None,
            min_approvals: 1,
            approval_mode: ApprovalMode::default(),
            timeout_secs: 1,
            on_timeout,
            bot_name: None,
        }
    }

    fn make_state_with_run<'a>(
        conn: &'a Connection,
        config: &'static Config,
    ) -> (ExecutionState<'a>, String) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Waiting, None)
            .unwrap();
        let run_id = run.id.clone();
        let state = ExecutionState {
            conn,
            config,
            workflow_run_id: run_id.clone(),
            workflow_name: "test".to_string(),
            worktree_id: Some("w1".to_string()),
            working_dir: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            agent_mgr: AgentManager::new(conn),
            wf_mgr: WorkflowManager::new(conn),
            parent_run_id: parent.id,
            depth: 0,
            target_label: None,
            step_results: HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            last_output_file: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
        };
        (state, run_id)
    }

    #[test]
    fn test_gate_timeout_fail() {
        let conn = setup_db();
        let config: &'static Config = Box::leak(Box::new(Config::default()));
        let (mut state, run_id) = make_state_with_run(&conn, config);

        let wf_mgr = WorkflowManager::new(&conn);
        let step_id = wf_mgr
            .insert_step(&run_id, "test_gate", "gate", false, 0, 0)
            .unwrap();
        set_step_status(&wf_mgr, &step_id, WorkflowStepStatus::Waiting);

        let node = make_gate_node(GateType::HumanApproval, OnTimeout::Fail);
        let result = handle_gate_timeout(&mut state, &step_id, &node);

        assert!(result.is_err());
        let steps = wf_mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_gate_timeout_continue() {
        let conn = setup_db();
        let config: &'static Config = Box::leak(Box::new(Config::default()));
        let (mut state, run_id) = make_state_with_run(&conn, config);

        let wf_mgr = WorkflowManager::new(&conn);
        let step_id = wf_mgr
            .insert_step(&run_id, "test_gate", "gate", false, 0, 0)
            .unwrap();
        set_step_status(&wf_mgr, &step_id, WorkflowStepStatus::Waiting);

        let node = make_gate_node(GateType::HumanApproval, OnTimeout::Continue);
        let result = handle_gate_timeout(&mut state, &step_id, &node);

        assert!(result.is_ok(), "on_timeout=continue should return Ok");
        let steps = wf_mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::TimedOut);
        assert!(
            state.all_succeeded,
            "on_timeout=continue should not set all_succeeded=false"
        );
    }

    #[test]
    fn test_parse_conductor_output() {
        let text = r#"Here is my analysis...

<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_review_issues", "has_critical_issues"], "context": "Found 2 issues in src/lib.rs"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let output = parse_conductor_output(text).unwrap();
        assert_eq!(
            output.markers,
            vec!["has_review_issues", "has_critical_issues"]
        );
        assert_eq!(output.context, "Found 2 issues in src/lib.rs");
    }

    #[test]
    fn test_parse_conductor_output_missing() {
        assert!(parse_conductor_output("no output block here").is_none());
    }

    #[test]
    fn test_parse_conductor_output_no_markers() {
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [], \"context\": \"All good\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let output = parse_conductor_output(text).unwrap();
        assert!(output.markers.is_empty());
        assert_eq!(output.context, "All good");
    }

    #[test]
    fn test_parse_conductor_output_last_occurrence() {
        // Should find the LAST occurrence (the real one), not a false positive in a code block
        let text = r#"Here's an example of the output format:
```
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["fake"], "context": "This is a code example"}
<<<END_CONDUCTOR_OUTPUT>>>
```

And here is my actual output:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["real"], "context": "This is the real output"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let output = parse_conductor_output(text).unwrap();
        assert_eq!(output.markers, vec!["real"]);
        assert_eq!(output.context, "This is the real output");
    }

    #[test]
    fn test_substitute_variables() {
        let mut vars = HashMap::new();
        vars.insert("ticket_id", "FEAT-123".to_string());
        vars.insert("prior_context", "Created PLAN.md".to_string());

        let prompt = "Fix ticket {{ticket_id}}. Context: {{prior_context}}. Unknown: {{unknown}}.";
        let result = substitute_variables(prompt, &vars);
        assert_eq!(
            result,
            "Fix ticket FEAT-123. Context: Created PLAN.md. Unknown: {{unknown}}."
        );
    }

    #[test]
    fn test_workflow_run_status_roundtrip() {
        for status in [
            WorkflowRunStatus::Pending,
            WorkflowRunStatus::Running,
            WorkflowRunStatus::Completed,
            WorkflowRunStatus::Failed,
            WorkflowRunStatus::Cancelled,
            WorkflowRunStatus::Waiting,
        ] {
            let s = status.to_string();
            let parsed: WorkflowRunStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_workflow_step_status_roundtrip() {
        for status in [
            WorkflowStepStatus::Pending,
            WorkflowStepStatus::Running,
            WorkflowStepStatus::Completed,
            WorkflowStepStatus::Failed,
            WorkflowStepStatus::Skipped,
            WorkflowStepStatus::Waiting,
        ] {
            let s = status.to_string();
            let parsed: WorkflowStepStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_poll_child_completion_already_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        mgr.update_run_completed(
            &run.id,
            None,
            Some("done"),
            Some(0.05),
            Some(3),
            Some(5000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, AgentRunStatus::Completed);
    }

    #[test]
    fn test_poll_child_completion_timeout() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_millis(50),
            None,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_runtime::PollError::Timeout(_)
        ));
    }

    #[test]
    fn test_poll_child_completion_shutdown() {
        use std::sync::{atomic::AtomicBool, Arc};

        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        // run stays in Running; flag is already set
        let flag = Arc::new(AtomicBool::new(true));

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(5),
            Some(&flag),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_runtime::PollError::Shutdown
        ));
    }

    #[test]
    fn test_recover_stuck_steps_syncs_completed() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let wf_mgr = WorkflowManager::new(&conn);

        // Create a parent agent run and a workflow run
        let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let wf_run = wf_mgr
            .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert a step stuck in 'running' with a child_run_id
        let step_id = wf_mgr
            .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
            .unwrap();
        let child = agent_mgr
            .create_run(Some("w1"), "child-agent", None, None)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Running,
                Some(&child.id),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // Mark child run as completed
        agent_mgr
            .update_run_completed(
                &child.id,
                None,
                Some("great output"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let recovered = wf_mgr.recover_stuck_steps().unwrap();
        assert_eq!(recovered, 1);

        let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
        assert_eq!(steps[0].result_text.as_deref(), Some("great output"));
    }

    #[test]
    fn test_recover_stuck_steps_skips_still_running() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let wf_mgr = WorkflowManager::new(&conn);

        let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let wf_run = wf_mgr
            .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = wf_mgr
            .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
            .unwrap();
        let child = agent_mgr
            .create_run(Some("w1"), "child-agent", None, None)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Running,
                Some(&child.id),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        // child run stays in 'running' — should NOT be recovered

        let recovered = wf_mgr.recover_stuck_steps().unwrap();
        assert_eq!(recovered, 0);

        let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Running);
    }

    #[test]
    fn test_recover_stuck_steps_failed_child_marks_step_failed() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let wf_mgr = WorkflowManager::new(&conn);

        let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let wf_run = wf_mgr
            .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = wf_mgr
            .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
            .unwrap();
        let child = agent_mgr
            .create_run(Some("w1"), "child-agent", None, None)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Running,
                Some(&child.id),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        agent_mgr
            .update_run_failed(&child.id, "agent crashed")
            .unwrap();

        let recovered = wf_mgr.recover_stuck_steps().unwrap();
        assert_eq!(recovered, 1);

        let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
        assert_eq!(steps[0].result_text.as_deref(), Some("agent crashed"));
    }

    #[test]
    fn test_list_workflow_runs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w1"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("test-a", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("test-b", Some("w1"), &p2.id, true, "pr", None)
            .unwrap();

        let runs = mgr.list_workflow_runs("w1").unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn test_list_all_workflow_runs_cross_worktree() {
        let conn = setup_db();
        // Insert a second worktree so we can test cross-worktree aggregation.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("flow-a", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("flow-b", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        // list_all returns both runs regardless of worktree
        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 2);
        let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
        assert!(names.contains(&"flow-a"));
        assert!(names.contains(&"flow-b"));
    }

    #[test]
    fn test_list_all_workflow_runs_respects_limit() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);

        let mgr = WorkflowManager::new(&conn);
        for i in 0..5 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(
                &format!("flow-{i}"),
                Some("w1"),
                &p.id,
                false,
                "manual",
                None,
            )
            .unwrap();
        }

        let limited = mgr.list_all_workflow_runs(3).unwrap();
        assert_eq!(limited.len(), 3);
    }

    #[test]
    fn test_list_all_workflow_runs_empty() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let runs = mgr.list_all_workflow_runs(50).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn test_list_all_workflow_runs_includes_ephemeral() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Create a normal run (with worktree)
        let parent1 = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        mgr.create_workflow_run("normal-wf", Some("w1"), &parent1.id, false, "manual", None)
            .unwrap();

        // Create an ephemeral run (no worktree)
        let parent2 = agent_mgr
            .create_run(None, "ephemeral workflow", None, None)
            .unwrap();
        let ephemeral = mgr
            .create_workflow_run("ephemeral-wf", None, &parent2.id, false, "manual", None)
            .unwrap();

        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 2);

        // Verify the ephemeral run has None worktree_id
        let found = all.iter().find(|r| r.id == ephemeral.id).unwrap();
        assert!(found.worktree_id.is_none());
    }

    #[test]
    fn test_list_all_workflow_runs_excludes_merged_worktree() {
        let conn = setup_db();
        // Insert a second worktree with merged status
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-merged', 'feat/merged', '/tmp/ws/merged', 'merged', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("active-run", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("merged-run", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].workflow_name, "active-run");
    }

    #[test]
    fn test_list_all_workflow_runs_excludes_abandoned_worktree() {
        let conn = setup_db();
        // Insert a second worktree with abandoned status
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-abandoned', 'feat/abandoned', '/tmp/ws/abandoned', 'abandoned', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("active-run", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("abandoned-run", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].workflow_name, "active-run");
    }

    #[test]
    fn test_list_all_workflow_runs_includes_ephemeral_and_active() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Active worktree run
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        mgr.create_workflow_run("active-run", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();

        // Ephemeral run (no worktree)
        let p2 = agent_mgr.create_run(None, "wf2", None, None).unwrap();
        mgr.create_workflow_run("ephemeral-run", None, &p2.id, false, "manual", None)
            .unwrap();

        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 2);
        let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
        assert!(names.contains(&"active-run"));
        assert!(names.contains(&"ephemeral-run"));
    }

    #[test]
    fn test_list_all_workflow_runs_filtered_paginated_status_filter() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Create one run and leave it in Pending state.
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        mgr.create_workflow_run("pending-run", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();

        // Create a second run and advance it to Completed.
        let p2 = agent_mgr.create_run(Some("w1"), "wf2", None, None).unwrap();
        let r2 = mgr
            .create_workflow_run("done-run", Some("w1"), &p2.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&r2.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        let completed = mgr
            .list_all_workflow_runs_filtered_paginated(Some(WorkflowRunStatus::Completed), 100, 0)
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].workflow_name, "done-run");

        let pending = mgr
            .list_all_workflow_runs_filtered_paginated(Some(WorkflowRunStatus::Pending), 100, 0)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].workflow_name, "pending-run");
    }

    #[test]
    fn test_list_all_workflow_runs_filtered_paginated_offset() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        for i in 0..4 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(
                &format!("flow-{i}"),
                Some("w1"),
                &p.id,
                false,
                "manual",
                None,
            )
            .unwrap();
        }

        let page1 = mgr
            .list_all_workflow_runs_filtered_paginated(None, 2, 0)
            .unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = mgr
            .list_all_workflow_runs_filtered_paginated(None, 2, 2)
            .unwrap();
        assert_eq!(page2.len(), 2);

        // All 4 unique
        let all_ids: std::collections::HashSet<_> = page1
            .iter()
            .chain(page2.iter())
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(all_ids.len(), 4);
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_excludes_merged_worktree() {
        let conn = setup_db();
        // Insert a second worktree with merged status (same repo)
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-merged', 'feat/merged', '/tmp/ws/merged', 'merged', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        // Use create_workflow_run_with_targets to set repo_id so the query can filter by it
        mgr.create_workflow_run_with_targets(
            "active-run",
            Some("w1"),
            None,
            Some("r1"),
            &p1.id,
            false,
            "manual",
            None,
            None,
            None,
        )
        .unwrap();
        mgr.create_workflow_run_with_targets(
            "merged-run",
            Some("w2"),
            None,
            Some("r1"),
            &p2.id,
            false,
            "manual",
            None,
            None,
            None,
        )
        .unwrap();

        let runs = mgr.list_workflow_runs_by_repo_id("r1", 100, 0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].workflow_name, "active-run");
    }

    #[test]
    fn test_list_workflow_runs_for_scope_scoped() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("only-w1", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("only-w2", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        // Scoped: only w1's run
        let scoped = mgr.list_workflow_runs_for_scope(Some("w1"), 50).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].workflow_name, "only-w1");

        // Global: both runs
        let global = mgr.list_workflow_runs_for_scope(None, 50).unwrap();
        assert_eq!(global.len(), 2);
    }

    #[test]
    fn test_list_workflow_runs_for_scope_global_limit() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);
        for i in 0..5 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(
                &format!("flow-{i}"),
                Some("w1"),
                &p.id,
                false,
                "manual",
                None,
            )
            .unwrap();
        }
        let limited = mgr.list_workflow_runs_for_scope(None, 2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn test_get_workflow_run_not_found() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.get_workflow_run("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_step_by_id() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "build", "actor", false, 0, 0)
            .unwrap();

        let step = mgr.get_step_by_id(&step_id).unwrap();
        assert!(step.is_some());
        let step = step.unwrap();
        assert_eq!(step.id, step_id);
        assert_eq!(step.step_name, "build");
        assert_eq!(step.role, "actor");

        let missing = mgr.get_step_by_id("nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_metadata_fields_basic() {
        let step = WorkflowRunStep {
            id: "s1".into(),
            workflow_run_id: "r1".into(),
            step_name: "lint".into(),
            role: "reviewer".into(),
            can_commit: false,
            condition_expr: None,
            status: WorkflowStepStatus::Completed,
            child_run_id: None,
            position: 1,
            started_at: Some("2025-01-01T00:00:00Z".into()),
            ended_at: Some("2025-01-01T00:01:00Z".into()),
            result_text: None,
            condition_met: None,
            iteration: 1,
            parallel_group_id: None,
            context_out: None,
            markers_out: None,
            retry_count: 0,
            gate_type: None,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: None,
            output_file: None,
        };
        let entries = step.metadata_fields();
        assert_eq!(entries.len(), 6); // 4 always-present + Started + Ended
        assert_eq!(
            entries[0],
            MetadataEntry::Field {
                label: "Status",
                value: "completed".into()
            }
        );
        assert_eq!(
            entries[1],
            MetadataEntry::Field {
                label: "Role",
                value: "reviewer".into()
            }
        );
        assert_eq!(
            entries[2],
            MetadataEntry::Field {
                label: "Can commit",
                value: "false".into()
            }
        );
        assert_eq!(
            entries[3],
            MetadataEntry::Field {
                label: "Iteration",
                value: "1".into()
            }
        );
        assert_eq!(
            entries[4],
            MetadataEntry::Field {
                label: "Started",
                value: "2025-01-01T00:00:00Z".into()
            }
        );
        assert_eq!(
            entries[5],
            MetadataEntry::Field {
                label: "Ended",
                value: "2025-01-01T00:01:00Z".into()
            }
        );
        // No gate or section entries
        assert!(!entries
            .iter()
            .any(|e| matches!(e, MetadataEntry::Section { .. })));
    }

    #[test]
    fn test_metadata_fields_optional_sections() {
        let step = WorkflowRunStep {
            id: "s2".into(),
            workflow_run_id: "r1".into(),
            step_name: "review".into(),
            role: "reviewer".into(),
            can_commit: false,
            condition_expr: None,
            status: WorkflowStepStatus::Running,
            child_run_id: None,
            position: 2,
            started_at: None,
            ended_at: None,
            result_text: Some("All good".into()),
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: Some("ctx data".into()),
            markers_out: Some("marker1".into()),
            retry_count: 0,
            gate_type: Some("approval".into()),
            gate_prompt: Some("Please approve".into()),
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: Some("Looks good".into()),
            structured_output: None,
            output_file: None,
        };
        let entries = step.metadata_fields();
        assert!(entries.contains(&MetadataEntry::Field {
            label: "Gate type",
            value: "approval".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Gate Prompt",
            body: "Please approve".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Gate Feedback",
            body: "Looks good".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Result",
            body: "All good".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Context Out",
            body: "ctx data".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Markers Out",
            body: "marker1".into()
        }));
    }

    // -----------------------------------------------------------------------
    // fetch_child_final_output tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_fetch_child_final_output_returns_last_completed_step() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert two completed steps; the second (position=1) should be returned
        let step1_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step1_id,
            WorkflowStepStatus::Completed,
            None,
            Some("step-a done"),
            Some("context-a"),
            Some(r#"["marker_a"]"#),
            Some(0),
        )
        .unwrap();

        let step2_id = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        mgr.update_step_status(
            &step2_id,
            WorkflowStepStatus::Completed,
            None,
            Some("step-b done"),
            Some("context-b"),
            Some(r#"["marker_b1","marker_b2"]"#),
            Some(0),
        )
        .unwrap();

        let (markers, context) = fetch_child_final_output(&mgr, &run.id);
        assert_eq!(markers, vec!["marker_b1", "marker_b2"]);
        assert_eq!(context, "context-b");
    }

    #[test]
    fn test_fetch_child_final_output_no_completed_steps() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert a failed step only
        let step_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Failed,
            None,
            Some("failed"),
            None,
            None,
            Some(0),
        )
        .unwrap();

        let (markers, context) = fetch_child_final_output(&mgr, &run.id);
        assert!(markers.is_empty());
        assert!(context.is_empty());
    }

    #[test]
    fn test_fetch_child_final_output_malformed_markers_json() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("some context"),
            Some("not valid json {{{"),
            Some(0),
        )
        .unwrap();

        let (markers, context) = fetch_child_final_output(&mgr, &run.id);
        assert!(markers.is_empty()); // malformed JSON falls back to empty
        assert_eq!(context, "some context");
    }

    #[test]
    fn test_fetch_child_final_output_nonexistent_run() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let (markers, context) = fetch_child_final_output(&mgr, "nonexistent-run-id");
        assert!(markers.is_empty());
        assert!(context.is_empty());
    }

    // -----------------------------------------------------------------------
    // build_variable_map tests
    // -----------------------------------------------------------------------

    /// Helper to create a minimal ExecutionState for testing build_variable_map.
    fn make_test_state(conn: &Connection) -> ExecutionState<'_> {
        let config = Config::default();
        // We need a config that lives long enough — use a leaked Box for test simplicity.
        let config: &'static Config = Box::leak(Box::new(config));
        ExecutionState {
            conn,
            config,
            workflow_run_id: String::new(),
            workflow_name: String::new(),
            worktree_id: None,
            working_dir: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            agent_mgr: AgentManager::new(conn),
            wf_mgr: WorkflowManager::new(conn),
            parent_run_id: String::new(),
            depth: 0,
            target_label: None,
            step_results: HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            last_output_file: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
        }
    }

    #[test]
    fn test_build_variable_map_includes_inputs_and_prior_context() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);
        state
            .inputs
            .insert("branch".to_string(), "main".to_string());
        state.contexts.push(ContextEntry {
            step: "step-a".to_string(),
            iteration: 0,
            context: "previous output".to_string(),
            markers: vec![],
            structured_output: None,
        });

        let vars = build_variable_map(&state);
        assert_eq!(vars.get("branch").unwrap(), "main");
        assert_eq!(vars.get("prior_context").unwrap(), "previous output");
        assert!(vars.get("prior_contexts").unwrap().contains("step-a"));
    }

    #[test]
    fn test_parallel_contexts_included_in_prior_contexts() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Simulate multiple parallel agents completing and pushing contexts
        // (this is the pattern now used in execute_parallel's success branch)
        state.contexts.push(ContextEntry {
            step: "reviewer-a".to_string(),
            iteration: 0,
            context: "LGTM from reviewer A".to_string(),
            markers: vec![],
            structured_output: None,
        });
        state.contexts.push(ContextEntry {
            step: "reviewer-b".to_string(),
            iteration: 0,
            context: "Needs changes from reviewer B".to_string(),
            markers: vec!["has_review_issues".to_string()],
            structured_output: None,
        });

        let vars = build_variable_map(&state);

        // prior_context should be the last context pushed
        assert_eq!(
            vars.get("prior_context").unwrap(),
            "Needs changes from reviewer B"
        );

        // prior_contexts should contain both parallel agent entries
        let prior_contexts = vars.get("prior_contexts").unwrap();
        assert!(prior_contexts.contains("reviewer-a"));
        assert!(prior_contexts.contains("reviewer-b"));
        assert!(prior_contexts.contains("LGTM from reviewer A"));
        assert!(prior_contexts.contains("Needs changes from reviewer B"));
    }

    #[test]
    fn test_build_variable_map_includes_gate_feedback() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);
        state.last_gate_feedback = Some("looks good".to_string());

        let vars = build_variable_map(&state);
        assert_eq!(vars.get("gate_feedback").unwrap(), "looks good");
    }

    #[test]
    fn test_build_variable_map_no_gate_feedback() {
        let conn = setup_db();
        let state = make_test_state(&conn);
        let vars = build_variable_map(&state);
        assert!(!vars.contains_key("gate_feedback"));
        // prior_context should be empty string when no contexts
        assert_eq!(vars.get("prior_context").unwrap(), "");
        // prior_output should be absent when no structured output
        assert!(!vars.contains_key("prior_output"));
    }

    #[test]
    fn test_build_variable_map_includes_prior_output() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);
        let json = r#"{"approved":true,"summary":"All clear"}"#.to_string();
        state.contexts.push(crate::workflow::types::ContextEntry {
            step: "test_step".to_string(),
            iteration: 0,
            context: String::new(),
            markers: Vec::new(),
            structured_output: Some(json.clone()),
        });

        let vars = build_variable_map(&state);
        assert_eq!(vars.get("prior_output").unwrap(), &json);
    }

    #[test]
    fn test_build_variable_map_includes_dry_run() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Default exec_config has dry_run = false
        let vars = build_variable_map(&state);
        assert_eq!(vars.get("dry_run").unwrap(), "false");

        // Set dry_run = true
        state.exec_config.dry_run = true;
        let vars = build_variable_map(&state);
        assert_eq!(vars.get("dry_run").unwrap(), "true");
    }

    // -----------------------------------------------------------------------
    // resolve_child_inputs tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_child_inputs_substitutes_variables() {
        use crate::workflow_dsl::InputDecl;

        let mut raw = HashMap::new();
        raw.insert("msg".to_string(), "Hello {{name}}!".to_string());

        let mut vars: HashMap<&str, String> = HashMap::new();
        vars.insert("name", "World".to_string());

        let decls = vec![InputDecl {
            name: "msg".to_string(),
            required: true,
            default: None,
            description: None,
            input_type: Default::default(),
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("msg").unwrap(), "Hello World!");
    }

    #[test]
    fn test_resolve_child_inputs_applies_defaults() {
        use crate::workflow_dsl::InputDecl;

        let raw = HashMap::new(); // no inputs provided

        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "mode".to_string(),
            required: false,
            default: Some("fast".to_string()),
            description: None,
            input_type: Default::default(),
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("mode").unwrap(), "fast");
    }

    #[test]
    fn test_resolve_child_inputs_missing_required() {
        use crate::workflow_dsl::InputDecl;

        let raw = HashMap::new();
        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "pr_url".to_string(),
            required: true,
            default: None,
            description: None,
            input_type: Default::default(),
        }];

        let err = resolve_child_inputs(&raw, &vars, &decls).unwrap_err();
        assert_eq!(err, "pr_url");
    }

    #[test]
    fn test_resolve_child_inputs_provided_overrides_default() {
        use crate::workflow_dsl::InputDecl;

        let mut raw = HashMap::new();
        raw.insert("mode".to_string(), "slow".to_string());

        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "mode".to_string(),
            required: false,
            default: Some("fast".to_string()),
            description: None,
            input_type: Default::default(),
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("mode").unwrap(), "slow");
    }

    #[test]
    fn test_resolve_child_inputs_optional_without_default_omitted() {
        use crate::workflow_dsl::InputDecl;

        let raw = HashMap::new();
        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "optional_field".to_string(),
            required: false,
            default: None,
            description: None,
            input_type: Default::default(),
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert!(!result.contains_key("optional_field"));
    }

    #[test]
    fn test_resolve_child_inputs_boolean_defaults_to_false() {
        use crate::workflow_dsl::{InputDecl, InputType};

        let raw = HashMap::new(); // boolean input not explicitly passed
        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "flag".to_string(),
            required: false,
            default: None,
            description: None,
            input_type: InputType::Boolean,
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("flag").map(|s| s.as_str()), Some("false"));
    }

    #[test]
    fn test_resolve_child_inputs_boolean_provided_value_not_overwritten() {
        use crate::workflow_dsl::{InputDecl, InputType};

        let mut raw = HashMap::new();
        raw.insert("flag".to_string(), "true".to_string());

        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "flag".to_string(),
            required: false,
            default: None,
            description: None,
            input_type: InputType::Boolean,
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("flag").map(|s| s.as_str()), Some("true"));
    }

    // -----------------------------------------------------------------------
    // execute_unless tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_unless_marker_absent_runs_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Step "build" exists but does NOT have the "has_errors" marker
        state.step_results.insert(
            "build".to_string(),
            StepResult {
                step_name: "build".to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec!["build_ok".to_string()],
                context: String::new(),
                child_run_id: None,
                structured_output: None,
                output_file: None,
            },
        );

        let node = UnlessNode {
            condition: crate::workflow_dsl::Condition::StepMarker {
                step: "build".to_string(),
                marker: "has_errors".to_string(),
            },
            body: vec![], // empty body — just verify it enters the branch without error
        };

        // Should succeed (marker absent → body executes, empty body is fine)
        execute_unless(&mut state, &node).unwrap();
    }

    #[test]
    fn test_execute_unless_marker_present_skips_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Step "build" has the "has_errors" marker
        state.step_results.insert(
            "build".to_string(),
            StepResult {
                step_name: "build".to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec!["has_errors".to_string()],
                context: String::new(),
                child_run_id: None,
                structured_output: None,
                output_file: None,
            },
        );

        let node = UnlessNode {
            condition: crate::workflow_dsl::Condition::StepMarker {
                step: "build".to_string(),
                marker: "has_errors".to_string(),
            },
            body: vec![], // empty body
        };

        // Should succeed (marker present → body skipped)
        execute_unless(&mut state, &node).unwrap();
    }

    #[test]
    fn test_execute_unless_step_not_found_runs_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // No step results at all — step "build" not in step_results
        let node = UnlessNode {
            condition: crate::workflow_dsl::Condition::StepMarker {
                step: "build".to_string(),
                marker: "has_errors".to_string(),
            },
            body: vec![], // empty body
        };

        // Should succeed (step not found → unwrap_or(false) → !false → body runs)
        execute_unless(&mut state, &node).unwrap();
    }

    // -----------------------------------------------------------------------
    // interpret_agent_output tests
    // -----------------------------------------------------------------------

    fn make_test_schema() -> OutputSchema {
        schema_config::parse_schema_content(
            "fields:\n  approved: boolean\n  summary: string\n",
            "test",
        )
        .unwrap()
    }

    #[test]
    fn test_interpret_agent_output_schema_valid() {
        let schema = make_test_schema();
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"approved\": true, \"summary\": \"all good\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let (markers, context, json) =
            interpret_agent_output(Some(text), Some(&schema), true).unwrap();
        assert_eq!(context, "all good");
        assert!(json.is_some());
        // approved=true → no not_approved marker
        assert!(!markers.contains(&"not_approved".to_string()));
    }

    #[test]
    fn test_interpret_agent_output_schema_validation_fails_succeeded() {
        let schema = make_test_schema();
        // Missing required field "approved"
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"summary\": \"oops\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = interpret_agent_output(Some(text), Some(&schema), true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("structured output validation"));
    }

    #[test]
    fn test_interpret_agent_output_schema_validation_fails_not_succeeded_falls_back() {
        let schema = make_test_schema();
        // Missing required field — but succeeded=false so it falls back
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"summary\": \"oops\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let (markers, context, json) =
            interpret_agent_output(Some(text), Some(&schema), false).unwrap();
        // Falls back to generic parse_conductor_output which doesn't find markers/context
        assert!(json.is_none());
        assert!(markers.is_empty());
        assert!(context.is_empty());
    }

    #[test]
    fn test_interpret_agent_output_no_schema_generic_parsing() {
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [\"done\"], \"context\": \"finished\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let (markers, context, json) = interpret_agent_output(Some(text), None, true).unwrap();
        assert_eq!(markers, vec!["done"]);
        assert_eq!(context, "finished");
        assert!(json.is_none());
    }

    #[test]
    fn test_interpret_agent_output_no_text() {
        let schema = make_test_schema();
        // result_text is None with schema — falls back
        let (markers, context, json) = interpret_agent_output(None, Some(&schema), false).unwrap();
        assert!(markers.is_empty());
        assert!(context.is_empty());
        assert!(json.is_none());
    }

    // -----------------------------------------------------------------------
    // execute_do_while tests
    // -----------------------------------------------------------------------

    fn make_step_result(step_name: &str, markers: Vec<&str>) -> StepResult {
        StepResult {
            step_name: step_name.into(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: markers.into_iter().map(String::from).collect(),
            context: String::new(),
            child_run_id: None,
            structured_output: None,
            output_file: None,
        }
    }

    /// Helper to build an `ExecutionState` suitable for testing loop functions
    /// (no real agents or worktrees needed).
    fn make_loop_test_state<'a>(conn: &'a Connection, config: &'a Config) -> ExecutionState<'a> {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        ExecutionState {
            conn,
            config,
            workflow_run_id: run.id,
            workflow_name: "test".into(),
            worktree_id: Some("w1".into()),
            working_dir: "/tmp/test".into(),
            worktree_slug: "test".into(),
            repo_path: "/tmp/repo".into(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            agent_mgr: AgentManager::new(conn),
            wf_mgr: WorkflowManager::new(conn),
            parent_run_id: parent.id,
            depth: 0,
            target_label: None,
            step_results: HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            last_output_file: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
        }
    }

    #[test]
    fn test_do_while_body_runs_once_when_condition_absent() {
        // The defining semantic: body executes before condition check,
        // so even with no marker set the body runs once.
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 3,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![], // empty body — still runs the loop once
        };

        // No step_results set → marker absent → loop exits after 1 iteration
        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(state.all_succeeded);
    }

    #[test]
    fn test_do_while_max_iterations_fail() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        // Pre-set a marker that stays true forever (body is empty so nothing clears it)
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 2,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_iterations"));
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_do_while_max_iterations_continue() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 2,
            stuck_after: None,
            on_max_iter: OnMaxIter::Continue,
            body: vec![],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(state.all_succeeded);
    }

    #[test]
    fn test_do_while_stuck_detection() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        // Marker stays the same every iteration → stuck after 2
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 10,
            stuck_after: Some(2),
            on_max_iter: OnMaxIter::Fail,
            body: vec![],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("stuck"));
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_do_while_iterates_body_multiple_times() {
        // Verify the body actually executes on each iteration by tracking
        // state.position, which Gate nodes increment in dry_run mode.
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Marker present → loop keeps iterating until max_iterations
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let initial_position = state.position;

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 3,
            stuck_after: None,
            on_max_iter: OnMaxIter::Continue,
            body: vec![WorkflowNode::Gate(GateNode {
                name: "counter".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
                bot_name: None,
            })],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        // Gate node increments position once per iteration; 3 iterations expected
        assert_eq!(state.position - initial_position, 3);
    }

    // NOTE: Testing the natural-exit path (marker transitions from true→false
    // mid-loop) is not feasible in a unit test because no WorkflowNode type
    // modifies step_results without running a real agent. The `!has_marker → break`
    // branch after body execution IS covered when the marker is absent from the
    // start (see test_do_while_body_runs_once_when_condition_absent). The
    // transition case (marker present → body clears marker → loop exits) requires
    // integration testing with actual agent execution.

    #[test]
    fn test_do_while_fail_fast_exits_early() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.fail_fast = true;

        // Marker is set so the loop would keep iterating if not for fail_fast
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        // Simulate a prior failure — all_succeeded is already false
        state.all_succeeded = false;

        // Body has a no-op If node (condition never true → body skipped, returns Ok)
        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 10,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![WorkflowNode::If(IfNode {
                condition: crate::workflow_dsl::Condition::StepMarker {
                    step: "nonexistent".into(),
                    marker: "nope".into(),
                },
                body: vec![],
            })],
        };

        // fail_fast should cause early exit with Ok(()) instead of looping to max_iterations
        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_while_fail_fast_exits_early() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.fail_fast = true;

        // Marker is set so the loop would keep iterating if not for fail_fast
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        // Simulate a prior failure — all_succeeded is already false
        state.all_succeeded = false;

        // Body has a no-op If node (condition never true → body skipped, returns Ok)
        let node = WhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 10,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![WorkflowNode::If(IfNode {
                condition: crate::workflow_dsl::Condition::StepMarker {
                    step: "nonexistent".into(),
                    marker: "nope".into(),
                },
                body: vec![],
            })],
        };

        // fail_fast should cause early exit with Ok(()) instead of looping to max_iterations
        let result = execute_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_get_active_run_for_worktree_none_when_empty() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_get_active_run_for_worktree_returns_active() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("my-flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        // Set status to running
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().workflow_name, "my-flow");
    }

    #[test]
    fn test_get_active_run_for_worktree_none_after_completion() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("my-flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_get_active_run_for_worktree_ignores_other_worktree() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w2"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("other-flow", Some("w2"), &parent.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        // w1 should see no active runs
        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_none());
    }

    // -----------------------------------------------------------------------
    // execute_workflow guard tests (depth == 0 only)
    // -----------------------------------------------------------------------

    /// Minimal workflow with no agents or steps — used to exercise the
    /// execute_workflow guard without touching real agent infrastructure.
    fn make_empty_workflow() -> WorkflowDef {
        WorkflowDef {
            name: "test-wf".into(),
            description: "test".into(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: "test.wf".into(),
        }
    }

    #[test]
    fn test_cannot_start_workflow_run_when_active() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("running-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let workflow = make_empty_workflow();
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"),
            working_dir: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let err = execute_workflow(&input).unwrap_err();
        assert!(
            matches!(err, ConductorError::WorkflowRunAlreadyActive { .. }),
            "expected WorkflowRunAlreadyActive, got: {err}"
        );
    }

    #[test]
    fn test_can_start_workflow_run_after_completion() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("done-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let workflow = make_empty_workflow();
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"),
            working_dir: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        // Guard should pass; empty workflow completes successfully.
        let result = execute_workflow(&input);
        assert!(
            !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
            "should not be blocked by completed run"
        );
    }

    #[test]
    fn test_child_workflow_not_blocked_by_parent() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("parent-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let workflow = make_empty_workflow();
        // depth = 1 means this is a child workflow — guard must be skipped.
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"),
            working_dir: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 1,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result = execute_workflow(&input);
        assert!(
            !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
            "child workflow should not be blocked by active parent run"
        );
    }

    #[test]
    fn test_run_id_notify_slot_is_populated() {
        // Verify that execute_workflow writes the newly-created run ID into
        // run_id_notify before any steps execute. This is the mechanism used
        // by the MCP tool_run_workflow handler to return a run_id immediately.
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();

        let workflow = make_empty_workflow();

        let slot: RunIdSlot =
            std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: Some(std::sync::Arc::clone(&slot)),
        };

        execute_workflow(&input).expect("workflow should complete");

        let notified_id = slot
            .0
            .lock()
            .expect("mutex not poisoned")
            .clone()
            .expect("run_id_notify slot should have been written");

        // The written ID must match the run that was actually created.
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .get_workflow_run(&notified_id)
            .expect("db query ok")
            .expect("run should exist");
        assert_eq!(run.workflow_name, "test-wf");
    }

    // -----------------------------------------------------------------------
    // Regression tests: fallback-to-repo-root when worktree path missing (#816)
    // -----------------------------------------------------------------------

    /// setup_db() creates worktree `w1` with path `/tmp/ws/feat-test` which does not
    /// exist on disk. Prior to #816 this would propagate a path-not-found error; after
    /// the fix the engine must silently fall back to the repo root and succeed.
    #[test]
    fn test_execute_workflow_falls_back_to_repo_root_when_worktree_path_missing() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"), // path /tmp/ws/feat-test — does not exist on disk
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };

        let result = execute_workflow(&input).expect(
            "execute_workflow must succeed when worktree path is missing (fallback to repo root)",
        );
        assert!(
            result.all_succeeded,
            "empty workflow should complete with all_succeeded=true"
        );
    }

    // -----------------------------------------------------------------------
    // execute_do tests (plain do {} block)
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_do_empty_body() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        let node = DoNode {
            output: None,
            with: vec![],
            body: vec![],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        assert!(state.all_succeeded);
    }

    #[test]
    fn test_execute_do_sets_and_restores_block_state() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Set some outer block state that should be saved and restored
        state.block_output = Some("outer-schema".into());
        state.block_with = vec!["outer-snippet".into()];

        let node = DoNode {
            output: Some("inner-schema".into()),
            with: vec!["inner-snippet".into()],
            // Use a Gate in dry_run mode as a no-op body node
            body: vec![WorkflowNode::Gate(GateNode {
                name: "noop".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                approval_mode: ApprovalMode::default(),
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
                bot_name: None,
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());

        // After execute_do, outer state must be restored
        assert_eq!(state.block_output.as_deref(), Some("outer-schema"));
        assert_eq!(state.block_with, vec!["outer-snippet".to_string()]);
    }

    #[test]
    fn test_execute_do_restores_state_on_error() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.block_output = Some("outer-schema".into());
        state.block_with = vec!["outer-snippet".into()];

        // A call node without dry_run and no real agent will error
        let node = DoNode {
            output: Some("inner-schema".into()),
            with: vec!["inner-snippet".into()],
            body: vec![WorkflowNode::Call(CallNode {
                agent: AgentRef::Name("nonexistent-agent".into()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
                bot_name: None,
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_err());

        // Block state must be restored even after error
        assert_eq!(state.block_output.as_deref(), Some("outer-schema"));
        assert_eq!(state.block_with, vec!["outer-snippet".to_string()]);
    }

    #[test]
    fn test_execute_do_fail_fast_exits_early() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.fail_fast = true;
        state.exec_config.dry_run = true;
        state.all_succeeded = false; // simulate prior failure

        let initial_position = state.position;

        let node = DoNode {
            output: None,
            with: vec![],
            body: vec![
                WorkflowNode::Gate(GateNode {
                    name: "g1".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    approval_mode: ApprovalMode::default(),
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                    bot_name: None,
                }),
                WorkflowNode::Gate(GateNode {
                    name: "g2".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    approval_mode: ApprovalMode::default(),
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                    bot_name: None,
                }),
            ],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        // fail_fast should skip after first node — only 1 position increment
        assert_eq!(state.position - initial_position, 1);
    }

    #[test]
    fn test_execute_do_nested_with_combination() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Outer do sets with=["a"], inner do sets with=["b"].
        // After inner do runs, inner block_with should have been ["b", "a"].
        // After both do blocks complete, state should be fully restored.
        let node = DoNode {
            output: Some("outer-schema".into()),
            with: vec!["a".into()],
            body: vec![WorkflowNode::Do(DoNode {
                output: None,
                with: vec!["b".into()],
                body: vec![WorkflowNode::Gate(GateNode {
                    name: "noop".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    approval_mode: ApprovalMode::default(),
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                    bot_name: None,
                })],
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        // Outer state fully restored
        assert!(state.block_output.is_none());
        assert!(state.block_with.is_empty());
    }

    #[test]
    fn test_execute_do_nested_inner_output_overrides_outer() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Outer do sets output="outer", inner do sets output="inner".
        // Inner body should see block_output="inner".
        // Verify state restoration after nested execution.
        let node = DoNode {
            output: Some("outer".into()),
            with: vec![],
            body: vec![WorkflowNode::Do(DoNode {
                output: Some("inner".into()),
                with: vec![],
                body: vec![WorkflowNode::Gate(GateNode {
                    name: "noop".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    approval_mode: ApprovalMode::default(),
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                    bot_name: None,
                })],
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        // Outer state fully restored
        assert!(state.block_output.is_none());
        assert!(state.block_with.is_empty());
    }

    #[test]
    fn test_execute_call_merges_block_state() {
        // Verify execute_call picks up block_output and block_with from state.
        // The call will fail (no agent file on disk) but it should attempt to
        // load with the effective values rather than panicking.
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.block_output = Some("block-schema".into());
        state.block_with = vec!["block-snippet".into()];

        let node = CallNode {
            agent: AgentRef::Name("nonexistent".into()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec!["call-snippet".into()],
            bot_name: None,
        };

        // Call will error on load_agent, but the merging logic should execute
        // without panics and the error should be from agent loading, not from
        // the effective_output/effective_with computation.
        let result = execute_call(&mut state, &node, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("agent") || err.contains("nonexistent"),
            "expected agent load error, got: {err}"
        );
    }

    #[test]
    fn test_execute_call_node_output_overrides_block_output() {
        // When a CallNode has its own output, it should take precedence
        // over block_output. Verify the call attempts to use "call-schema".
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.block_output = Some("block-schema".into());

        let node = CallNode {
            agent: AgentRef::Name("nonexistent".into()),
            retries: 0,
            on_fail: None,
            output: Some("call-schema".into()),
            with: vec![],
            bot_name: None,
        };

        let result = execute_call(&mut state, &node, 0);
        assert!(result.is_err());
        // The error is from agent loading, not from the merging logic
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("agent") || err.contains("nonexistent"),
            "expected agent load error, got: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // bubble_up_child_step_results tests
    // ---------------------------------------------------------------------------

    fn create_child_run(conn: &Connection) -> (WorkflowManager<'_>, String) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        (wf_mgr, run.id)
    }

    #[test]
    fn test_bubble_up_child_step_results_basic() {
        let conn = setup_db();
        let (wf_mgr, run_id) = create_child_run(&conn);

        // Insert two completed steps with markers
        let step1 = wf_mgr
            .insert_step(&run_id, "review-aggregator", "reviewer", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step1,
                WorkflowStepStatus::Completed,
                None,
                Some("done"),
                Some("some context"),
                Some(r#"["has_review_issues"]"#),
                None,
            )
            .unwrap();

        let step2 = wf_mgr
            .insert_step(&run_id, "lint-checker", "reviewer", false, 1, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step2,
                WorkflowStepStatus::Completed,
                None,
                Some("done"),
                Some("lint ok"),
                Some(r#"["lint_passed"]"#),
                None,
            )
            .unwrap();

        let result = bubble_up_child_step_results(&wf_mgr, &run_id);

        assert_eq!(result.len(), 2);
        let agg = result.get("review-aggregator").unwrap();
        assert!(agg.markers.contains(&"has_review_issues".to_string()));
        let lint = result.get("lint-checker").unwrap();
        assert!(lint.markers.contains(&"lint_passed".to_string()));
    }

    #[test]
    fn test_bubble_up_child_step_results_parent_wins() {
        let conn = setup_db();
        let config: &'static Config = Box::leak(Box::new(Config::default()));
        let (mut state, _run_id) = make_state_with_run(&conn, config);

        // Parent already has a step result for "review-aggregator"
        state.step_results.insert(
            "review-aggregator".to_string(),
            StepResult {
                step_name: "review-aggregator".to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec!["parent_marker".to_string()],
                context: "parent context".to_string(),
                child_run_id: None,
                structured_output: None,
                output_file: None,
            },
        );

        // Child run with same step name but different marker
        let (child_wf_mgr, child_run_id) = create_child_run(&conn);
        let step1 = child_wf_mgr
            .insert_step(&child_run_id, "review-aggregator", "reviewer", false, 0, 0)
            .unwrap();
        child_wf_mgr
            .update_step_status(
                &step1,
                WorkflowStepStatus::Completed,
                None,
                Some("done"),
                Some("child context"),
                Some(r#"["child_marker"]"#),
                None,
            )
            .unwrap();

        let child_steps = bubble_up_child_step_results(&child_wf_mgr, &child_run_id);
        for (key, value) in child_steps {
            state.step_results.entry(key).or_insert(value);
        }

        // Parent's value should win
        let result = state.step_results.get("review-aggregator").unwrap();
        assert!(result.markers.contains(&"parent_marker".to_string()));
        assert!(!result.markers.contains(&"child_marker".to_string()));
    }

    #[test]
    fn test_bubble_up_child_step_results_no_completed_steps() {
        let conn = setup_db();
        let (wf_mgr, run_id) = create_child_run(&conn);

        // Insert a failed step — should not be bubbled up
        let step1 = wf_mgr
            .insert_step(&run_id, "some-step", "reviewer", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step1,
                WorkflowStepStatus::Failed,
                None,
                Some("failed"),
                None,
                None,
                None,
            )
            .unwrap();

        let result = bubble_up_child_step_results(&wf_mgr, &run_id);
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // Resume-related tests
    // -----------------------------------------------------------------------

    /// Helper: create a workflow run with steps in various statuses.
    fn setup_run_with_steps(conn: &Connection) -> (String, WorkflowManager<'_>) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(conn);
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Step 0: completed
        let s0 = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &s0,
            WorkflowStepStatus::Completed,
            None,
            Some("result-a"),
            Some("ctx-a"),
            Some(r#"["marker_a"]"#),
            Some(0),
        )
        .unwrap();

        // Step 1: failed
        let s1 = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        mgr.update_step_status(
            &s1,
            WorkflowStepStatus::Failed,
            None,
            Some("error"),
            None,
            None,
            Some(0),
        )
        .unwrap();

        // Step 2: running (stalled)
        let s2 = mgr
            .insert_step(&run.id, "step-c", "actor", false, 2, 0)
            .unwrap();
        set_step_status(&mgr, &s2, WorkflowStepStatus::Running);

        (run.id, mgr)
    }

    #[test]
    fn test_reset_failed_steps() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        let count = mgr.reset_failed_steps(&run_id).unwrap();
        // Should reset both 'failed' and 'running' steps
        assert_eq!(count, 2);

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed); // unchanged
        assert_eq!(steps[1].status, WorkflowStepStatus::Pending); // was failed
        assert!(steps[1].result_text.is_none()); // cleared
        assert_eq!(steps[2].status, WorkflowStepStatus::Pending); // was running
    }

    #[test]
    fn test_reset_completed_steps() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        let count = mgr.reset_completed_steps(&run_id).unwrap();
        assert_eq!(count, 1);

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Pending); // was completed
        assert!(steps[0].result_text.is_none()); // cleared
        assert!(steps[0].context_out.is_none()); // cleared
    }

    #[test]
    fn test_reset_steps_from_position() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        // Reset from position 1 onwards
        let count = mgr.reset_steps_from_position(&run_id, 1).unwrap();
        assert_eq!(count, 2); // positions 1 and 2

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed); // position 0 unchanged
        assert_eq!(steps[1].status, WorkflowStepStatus::Pending);
        assert_eq!(steps[2].status, WorkflowStepStatus::Pending);
    }

    #[test]
    fn test_get_completed_step_keys() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        let keys = mgr.get_completed_step_keys(&run_id).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&("step-a".to_string(), 0)));
        // Failed/running steps should not be in the set
        assert!(!keys.contains(&("step-b".to_string(), 0)));
        assert!(!keys.contains(&("step-c".to_string(), 0)));
    }

    // -----------------------------------------------------------------------
    // find_max_completed_while_iteration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_max_completed_while_iteration_none_completed() {
        let conn = setup_db();
        let state = make_test_state(&conn);

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
                bot_name: None,
            })],
        };

        // No resume context → returns 0
        assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
    }

    #[test]
    fn test_find_max_completed_while_iteration_two_completed() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let skip: HashSet<StepKey> = [("step-a".to_string(), 0), ("step-a".to_string(), 1)]
            .into_iter()
            .collect();
        state.resume_ctx = Some(ResumeContext {
            skip_completed: skip,
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
                bot_name: None,
            })],
        };

        // Iterations 0 and 1 completed → start from 2
        assert_eq!(find_max_completed_while_iteration(&state, &node), 2);
    }

    #[test]
    fn test_find_max_completed_while_iteration_empty_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        state.resume_ctx = Some(ResumeContext {
            skip_completed: HashSet::new(),
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![], // no call nodes
        };

        // Empty body → returns 0
        assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
    }

    #[test]
    fn test_find_max_completed_while_iteration_partial_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Two body nodes, but only one completed for iteration 0
        let skip: HashSet<StepKey> = [("step-a".to_string(), 0)].into_iter().collect();
        state.resume_ctx = Some(ResumeContext {
            skip_completed: skip,
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });
        // step-b:0 is NOT in skip_completed

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![
                WorkflowNode::Call(CallNode {
                    agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                    retries: 0,
                    on_fail: None,
                    output: None,
                    with: vec![],
                    bot_name: None,
                }),
                WorkflowNode::Call(CallNode {
                    agent: crate::workflow_dsl::AgentRef::Name("step-b".to_string()),
                    retries: 0,
                    on_fail: None,
                    output: None,
                    with: vec![],
                    bot_name: None,
                }),
            ],
        };

        // Only partial completion → start from 0
        assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
    }

    #[test]
    fn test_find_max_completed_while_iteration_with_parallel_and_gate() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let skip: HashSet<StepKey> = [
            ("agent-a".to_string(), 0),
            ("agent-b".to_string(), 0),
            ("approval".to_string(), 0),
        ]
        .into_iter()
        .collect();
        state.resume_ctx = Some(ResumeContext {
            skip_completed: skip,
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![
                WorkflowNode::Parallel(ParallelNode {
                    fail_fast: true,
                    min_success: None,
                    calls: vec![
                        crate::workflow_dsl::AgentRef::Name("agent-a".to_string()),
                        crate::workflow_dsl::AgentRef::Name("agent-b".to_string()),
                    ],
                    output: None,
                    call_outputs: HashMap::new(),
                    with: vec![],
                    call_with: HashMap::new(),
                    call_if: HashMap::new(),
                }),
                WorkflowNode::Gate(GateNode {
                    name: "approval".to_string(),
                    gate_type: crate::workflow_dsl::GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    approval_mode: ApprovalMode::default(),
                    timeout_secs: 300,
                    on_timeout: crate::workflow_dsl::OnTimeout::Fail,
                    bot_name: None,
                }),
            ],
        };

        // Iteration 0 fully completed → start from 1
        assert_eq!(find_max_completed_while_iteration(&state, &node), 1);
    }

    // -----------------------------------------------------------------------
    // restore_completed_step tests
    // -----------------------------------------------------------------------

    /// Helper to build a WorkflowRunStep for testing without listing every field.
    fn make_test_step(
        step_name: &str,
        status: WorkflowStepStatus,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        child_run_id: Option<&str>,
        structured_output: Option<&str>,
    ) -> WorkflowRunStep {
        WorkflowRunStep {
            id: "s1".to_string(),
            workflow_run_id: "run1".to_string(),
            step_name: step_name.to_string(),
            role: "actor".to_string(),
            can_commit: false,
            condition_expr: None,
            status,
            child_run_id: child_run_id.map(String::from),
            position: 0,
            started_at: None,
            ended_at: None,
            result_text: result_text.map(String::from),
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: context_out.map(String::from),
            markers_out: markers_out.map(String::from),
            retry_count: 0,
            gate_type: None,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: structured_output.map(String::from),
            output_file: None,
        }
    }

    /// Helper to build a ResumeContext from a step map.
    fn make_resume_ctx(
        step_map: HashMap<StepKey, WorkflowRunStep>,
        child_runs: HashMap<String, crate::agent::AgentRun>,
    ) -> ResumeContext {
        let skip_completed = step_map.keys().cloned().collect();
        ResumeContext {
            skip_completed,
            step_map,
            child_runs,
        }
    }

    #[test]
    fn test_restore_completed_step_basic() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let step = make_test_step(
            "review",
            WorkflowStepStatus::Completed,
            Some("looks good"),
            Some("reviewed code"),
            Some(r#"["approved"]"#),
            None,
            Some(r#"{"verdict":"approve"}"#),
        );
        let ctx = make_resume_ctx(
            [(("review".to_string(), 0), step)].into_iter().collect(),
            HashMap::new(),
        );

        restore_completed_step(&mut state, &ctx, "review", 0);

        // Verify step_results populated
        let result = state.step_results.get("review").unwrap();
        assert_eq!(result.status, WorkflowStepStatus::Completed);
        assert_eq!(result.result_text.as_deref(), Some("looks good"));
        assert_eq!(result.markers, vec!["approved"]);
        assert_eq!(result.context, "reviewed code");
        assert_eq!(
            result.structured_output.as_deref(),
            Some(r#"{"verdict":"approve"}"#)
        );

        // Verify contexts populated
        assert_eq!(state.contexts.len(), 1);
        assert_eq!(state.contexts[0].step, "review");
        assert_eq!(state.contexts[0].context, "reviewed code");
        assert_eq!(
            state.contexts[0].structured_output.as_deref(),
            Some(r#"{"verdict":"approve"}"#)
        );

        // Verify structured output is accessible via contexts
        assert_eq!(
            state
                .contexts
                .iter()
                .rev()
                .find_map(|c| c.structured_output.as_deref()),
            Some(r#"{"verdict":"approve"}"#)
        );
    }

    #[test]
    fn test_restore_completed_step_not_found() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let ctx = make_resume_ctx(HashMap::new(), HashMap::new());
        restore_completed_step(&mut state, &ctx, "nonexistent", 0);

        // Should be a no-op (with warning logged)
        assert!(state.step_results.is_empty());
        assert!(state.contexts.is_empty());
    }

    #[test]
    fn test_restore_completed_step_accumulates_costs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);

        // Create a child agent run with cost data
        let child_run = agent_mgr
            .create_run(Some("w1"), "test agent", None, None)
            .unwrap();
        agent_mgr
            .update_run_completed(
                &child_run.id,
                None,
                Some("done"),
                Some(0.05),
                Some(3),
                Some(5000),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let mut state = make_test_state(&conn);
        state.total_cost = 0.10;
        state.total_turns = 5;
        state.total_duration_ms = 10000;

        // Re-fetch the child run so we have the full AgentRun with costs
        let loaded_run = agent_mgr.get_run(&child_run.id).unwrap().unwrap();

        let step = make_test_step(
            "build",
            WorkflowStepStatus::Completed,
            Some("built"),
            Some("build output"),
            None,
            Some(&child_run.id),
            None,
        );
        let ctx = make_resume_ctx(
            [(("build".to_string(), 0), step)].into_iter().collect(),
            [(child_run.id.clone(), loaded_run)].into_iter().collect(),
        );

        restore_completed_step(&mut state, &ctx, "build", 0);

        // Costs should be accumulated from the child run
        assert!((state.total_cost - 0.15).abs() < 0.001);
        assert_eq!(state.total_turns, 8);
        assert_eq!(state.total_duration_ms, 15000);
    }

    #[test]
    fn test_restore_completed_step_restores_gate_feedback() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let mut step = make_test_step(
            "approval-gate",
            WorkflowStepStatus::Completed,
            Some("approved"),
            None,
            None,
            None,
            None,
        );
        step.gate_feedback = Some("LGTM, ship it".to_string());

        let ctx = make_resume_ctx(
            [(("approval-gate".to_string(), 0), step)]
                .into_iter()
                .collect(),
            HashMap::new(),
        );

        restore_completed_step(&mut state, &ctx, "approval-gate", 0);

        // Gate feedback should be restored for downstream steps
        assert_eq!(state.last_gate_feedback.as_deref(), Some("LGTM, ship it"));
    }

    // -----------------------------------------------------------------------
    // resume_workflow validation tests
    // -----------------------------------------------------------------------

    /// Helper: create a Config suitable for resume tests.
    fn make_resume_config() -> &'static Config {
        Box::leak(Box::new(Config::default()))
    }

    #[test]
    fn test_resume_rejects_completed_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("Cannot resume a completed"),
            "Expected completed-run error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_cancelled_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Cancelled, None)
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("Cannot resume a cancelled"),
            "Expected cancelled-run error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_running_run() {
        let err =
            validate_resume_preconditions(&WorkflowRunStatus::Running, false, None).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "Expected running-run error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_restart_and_from_step_together() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: Some("step-one"),
            restart: true,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string()
                .contains("--restart and --from-step together"),
            "Expected conflict error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_missing_snapshot() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        // Create run with no definition_snapshot
        let run = wf_mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("no definition snapshot"),
            "Expected missing-snapshot error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_nonexistent_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: "nonexistent-id",
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "Expected not-found error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_nonexistent_from_step() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"))
            .unwrap();

        // Add a step so the run has steps to search through
        let s0 = wf_mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &s0,
                WorkflowStepStatus::Completed,
                None,
                Some("ok"),
                None,
                None,
                Some(0),
            )
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: Some("nonexistent-step"),
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("not found in workflow run"),
            "Expected step-not-found error, got: {err}"
        );
    }

    /// Regression test for #816: resume_workflow must fall back to the repo root when
    /// the worktree path recorded in the DB no longer exists on disk.
    /// setup_db() creates worktree `w1` with path `/tmp/ws/feat-test` — absent on disk.
    #[test]
    fn test_resume_workflow_falls_back_to_repo_root_when_worktree_path_missing() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);

        // Serialize a valid empty WorkflowDef as the snapshot so resume can deserialize it.
        let snapshot = serde_json::to_string(&make_empty_workflow()).unwrap();
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some(&snapshot),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("prior error"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };

        let result = resume_workflow(&input).expect(
            "resume_workflow must succeed when worktree path is missing (fallback to repo root)",
        );
        assert!(
            result.all_succeeded,
            "empty resumed workflow should complete with all_succeeded=true"
        );
    }

    #[test]
    fn test_set_workflow_run_inputs_round_trip() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();

        // Initially inputs should be empty (no inputs set yet)
        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert!(fetched.inputs.is_empty(), "Expected no inputs initially");

        // Write inputs and read back
        let mut inputs = HashMap::new();
        inputs.insert("key1".to_string(), "value1".to_string());
        inputs.insert("key2".to_string(), "value2".to_string());
        wf_mgr.set_workflow_run_inputs(&run.id, &inputs).unwrap();

        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.inputs.get("key1").map(String::as_str),
            Some("value1")
        );
        assert_eq!(
            fetched.inputs.get("key2").map(String::as_str),
            Some("value2")
        );
        assert_eq!(fetched.inputs.len(), 2);
    }

    #[test]
    fn test_set_workflow_run_default_bot_name_round_trip() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();

        // Initially default_bot_name should be None
        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert!(
            fetched.default_bot_name.is_none(),
            "Expected no default_bot_name initially"
        );

        // Write a bot name and read it back
        wf_mgr
            .set_workflow_run_default_bot_name(&run.id, "reviewer-bot")
            .unwrap();

        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.default_bot_name.as_deref(),
            Some("reviewer-bot"),
            "default_bot_name should persist after set"
        );
    }

    #[test]
    fn test_default_bot_name_persists_through_suspend_and_resume() {
        // Verify that default_bot_name written by set_workflow_run_default_bot_name is
        // correctly loaded back when resume_workflow reads the run from the DB — this
        // exercises the full store → retrieve invariant for multi-stage bot identity.
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();

        // Simulate what execute_workflow does when a default_bot_name is set
        wf_mgr
            .set_workflow_run_default_bot_name(&run.id, "deploy-bot")
            .unwrap();

        // Simulate a suspend by marking the run as waiting
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Waiting, None)
            .unwrap();

        // Load the run as resume_workflow would — the bot name must survive the round-trip
        let restored = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(
            restored.default_bot_name.as_deref(),
            Some("deploy-bot"),
            "default_bot_name must survive a suspend/resume DB round-trip"
        );
        assert_eq!(restored.status, WorkflowRunStatus::Waiting);
    }

    #[test]
    fn test_row_to_workflow_run_malformed_inputs_json_returns_empty() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();

        // Directly write invalid JSON into the inputs column to simulate corruption
        conn.execute(
            "UPDATE workflow_runs SET inputs = ?1 WHERE id = ?2",
            rusqlite::params!["not-valid-json", &run.id],
        )
        .unwrap();

        // Reading back should return an empty HashMap (not panic), matching the
        // unwrap_or_else + warn fallback in row_to_workflow_run.
        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert!(
            fetched.inputs.is_empty(),
            "Expected empty inputs on malformed JSON, got: {:?}",
            fetched.inputs
        );
    }

    #[test]
    fn test_restart_resets_all_steps() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        // Verify initial state: 1 completed, 1 failed, 1 running
        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
        assert_eq!(steps[1].status, WorkflowStepStatus::Failed);
        assert_eq!(steps[2].status, WorkflowStepStatus::Running);

        // Restart resets both failed+running and completed steps
        mgr.reset_failed_steps(&run_id).unwrap();
        mgr.reset_completed_steps(&run_id).unwrap();

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(
            steps[0].status,
            WorkflowStepStatus::Pending,
            "completed step should be reset"
        );
        assert!(steps[0].result_text.is_none(), "result should be cleared");
        assert!(steps[0].context_out.is_none(), "context should be cleared");
        assert!(steps[0].markers_out.is_none(), "markers should be cleared");
        assert_eq!(
            steps[1].status,
            WorkflowStepStatus::Pending,
            "failed step should be reset"
        );
        assert_eq!(
            steps[2].status,
            WorkflowStepStatus::Pending,
            "running step should be reset"
        );

        // skip set should be empty after restart
        let keys = mgr.get_completed_step_keys(&run_id).unwrap();
        assert!(
            keys.is_empty(),
            "no completed steps should remain after restart"
        );
    }

    /// Exercises the full --from-step DB orchestration path:
    /// - skip-set pruning (keys at/after pos removed)
    /// - step_map filtered to only surviving skip keys
    /// - DB reset: steps at/after the target step become Pending
    /// - steps before the target step remain Completed
    #[test]
    fn test_from_step_skip_set_and_step_map() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert 3 completed steps at positions 0, 1, 2
        let s0 = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &s0,
            WorkflowStepStatus::Completed,
            None,
            Some("result-a"),
            Some("ctx-a"),
            None,
            Some(0),
        )
        .unwrap();

        let s1 = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        mgr.update_step_status(
            &s1,
            WorkflowStepStatus::Completed,
            None,
            Some("result-b"),
            Some("ctx-b"),
            None,
            Some(0),
        )
        .unwrap();

        let s2 = mgr
            .insert_step(&run.id, "step-c", "actor", false, 2, 0)
            .unwrap();
        mgr.update_step_status(
            &s2,
            WorkflowStepStatus::Completed,
            None,
            Some("result-c"),
            Some("ctx-c"),
            None,
            Some(0),
        )
        .unwrap();

        // Snapshot all_steps before any resets (mirrors resume_workflow: load once upfront)
        let all_steps = mgr.get_workflow_steps(&run.id).unwrap();

        // Simulate the --from-step "step-b" (position 1) branch of resume_workflow
        let mut keys = completed_keys_from_steps(&all_steps);
        assert_eq!(
            keys.len(),
            3,
            "all three steps should be in completed keys initially"
        );

        let pos = all_steps
            .iter()
            .find(|s| s.step_name == "step-b")
            .unwrap()
            .position;
        assert_eq!(pos, 1);

        // Prune keys at/after the from-step position (mirrors resume_workflow)
        let to_remove: Vec<StepKey> = all_steps
            .iter()
            .filter(|s| s.position >= pos && s.status == WorkflowStepStatus::Completed)
            .map(|s| (s.step_name.clone(), s.iteration as u32))
            .collect();
        for key in to_remove {
            keys.remove(&key);
        }

        // Reset DB state for steps at/after pos, then reset any failed/running steps
        mgr.reset_steps_from_position(&run.id, pos).unwrap();
        mgr.reset_failed_steps(&run.id).unwrap();

        // skip_completed should contain only ("step-a", 0)
        assert_eq!(keys.len(), 1, "only step-a:0 should survive pruning");
        assert!(
            keys.contains(&("step-a".to_string(), 0)),
            "step-a:0 must be in skip set"
        );
        assert!(
            !keys.contains(&("step-b".to_string(), 0)),
            "step-b:0 must be pruned from skip set"
        );
        assert!(
            !keys.contains(&("step-c".to_string(), 0)),
            "step-c:0 must be pruned from skip set"
        );

        // DB state: step-a stays Completed, step-b and step-c are reset to Pending
        let updated = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(
            updated[0].status,
            WorkflowStepStatus::Completed,
            "step-a (pos 0) must remain Completed"
        );
        assert_eq!(
            updated[1].status,
            WorkflowStepStatus::Pending,
            "step-b (pos 1, the from-step) must be reset to Pending"
        );
        assert_eq!(
            updated[2].status,
            WorkflowStepStatus::Pending,
            "step-c (pos 2) must be reset to Pending"
        );

        // step_map built from all_steps filtered by surviving skip keys
        // (mirrors resume_workflow)
        let step_map: HashMap<StepKey, WorkflowRunStep> = all_steps
            .into_iter()
            .filter(|s| s.status == WorkflowStepStatus::Completed)
            .map(|s| {
                let key = (s.step_name.clone(), s.iteration as u32);
                (key, s)
            })
            .filter(|(key, _)| keys.contains(key))
            .collect();

        assert!(
            step_map.contains_key(&("step-a".to_string(), 0)),
            "step_map must include step-a:0 (will be skipped on resume)"
        );
        assert!(
            !step_map.contains_key(&("step-b".to_string(), 0)),
            "step_map must not include step-b:0 (will be re-executed)"
        );
        assert!(
            !step_map.contains_key(&("step-c".to_string(), 0)),
            "step_map must not include step-c:0 (will be re-executed)"
        );
    }

    #[test]
    fn test_resume_allows_restart_on_completed_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        // Without restart, completed run should be rejected
        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        assert!(resume_workflow(&input).is_err());

        // With restart, completed run should pass the status check
        // (will fail later due to missing worktree, but the status check should pass)
        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: true,
        };
        let err = resume_workflow(&input).unwrap_err();
        // Should fail on worktree resolution, NOT on "Cannot resume a completed"
        assert!(
            !err.to_string().contains("Cannot resume a completed"),
            "restart=true should bypass the completed-run check, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // parallel min_success with skipped-on-resume agents
    // -----------------------------------------------------------------------

    /// Validates that the min_success calculation in execute_parallel correctly
    /// counts skipped-on-resume agents as successes for both the warning logic
    /// and the synthetic step status.
    ///
    /// This is a logic-level regression test: execute_parallel uses
    /// `effective_successes = successes + skipped_count` and
    /// `total_agents = children.len() + skipped_count`, and the synthetic step
    /// status must use `effective_successes >= min_required` (not raw `successes`).
    #[test]
    fn test_parallel_min_success_with_skipped_resume_agents() {
        // Scenario: 3 agents in a parallel block, min_success = 3.
        // On resume, 2 agents were already completed (skipped), 1 new agent succeeds.
        let successes: u32 = 1; // newly succeeded
        let skipped_count: u32 = 2; // completed on previous run
        let children_len: u32 = 1; // only the non-skipped agent was spawned

        let effective_successes = successes + skipped_count; // 3
        let total_agents = children_len + skipped_count; // 3
        let min_required: u32 = 3; // all must succeed

        // The synthetic step should be Completed, not Failed
        let status = if effective_successes >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        };
        assert_eq!(
            status,
            WorkflowStepStatus::Completed,
            "skipped agents must count toward min_success"
        );

        // Verify the all_succeeded flag would NOT be set to false
        let all_succeeded = effective_successes >= min_required;
        assert!(
            all_succeeded,
            "effective_successes ({effective_successes}) should meet min_required ({min_required})"
        );

        // Verify default min_success (None → total_agents) also works
        let default_min = total_agents;
        assert!(
            effective_successes >= default_min,
            "default min_success should be met when all agents (including skipped) succeed"
        );

        // Edge case: one new agent fails, only skipped agents succeeded
        let successes_fail: u32 = 0;
        let effective_fail = successes_fail + skipped_count; // 2
        let status_fail = if effective_fail >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        };
        assert_eq!(
            status_fail,
            WorkflowStepStatus::Failed,
            "should fail when effective successes don't meet min_required"
        );
    }

    // ---------------------------------------------------------------------------
    // per-call `if` condition logic tests
    // ---------------------------------------------------------------------------

    /// When `if` condition IS met (marker present), the agent is not skipped.
    /// This tests the pure marker-lookup logic used by execute_parallel.
    #[test]
    fn test_if_condition_met_does_not_skip() {
        // Simulate: detect-db-migrations emitted has_db_migrations → review-db-migrations runs
        let cond_step = "detect-db-migrations";
        let cond_marker = "has_db_migrations";

        let mut step_results: HashMap<String, StepResult> = HashMap::new();
        step_results.insert(
            cond_step.to_string(),
            StepResult {
                step_name: cond_step.to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec![cond_marker.to_string()],
                context: "Found 2 migration files".to_string(),
                child_run_id: None,
                structured_output: None,
                output_file: None,
            },
        );

        let has_marker = step_results
            .get(cond_step)
            .map(|r| r.markers.iter().any(|m| m == cond_marker))
            .unwrap_or(false);

        assert!(has_marker, "marker present → agent should NOT be skipped");
    }

    /// When `if` condition is NOT met (marker absent), the agent is skipped.
    #[test]
    fn test_if_condition_not_met_skips() {
        // Simulate: detect-db-migrations ran but did NOT emit has_db_migrations
        let cond_step = "detect-db-migrations";
        let cond_marker = "has_db_migrations";

        let mut step_results: HashMap<String, StepResult> = HashMap::new();
        step_results.insert(
            cond_step.to_string(),
            StepResult {
                step_name: cond_step.to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec![], // no markers emitted
                context: "No migration files in diff".to_string(),
                child_run_id: None,
                structured_output: None,
                output_file: None,
            },
        );

        let has_marker = step_results
            .get(cond_step)
            .map(|r| r.markers.iter().any(|m| m == cond_marker))
            .unwrap_or(false);

        assert!(!has_marker, "marker absent → agent SHOULD be skipped");
    }

    /// When the cond_step is not in step_results at all, `if` skips the agent.
    #[test]
    fn test_if_step_not_found_skips() {
        let cond_step = "detect-db-migrations";
        let cond_marker = "has_db_migrations";
        let step_results: HashMap<String, StepResult> = HashMap::new();

        let has_marker = step_results
            .get(cond_step)
            .map(|r| r.markers.iter().any(|m| m == cond_marker))
            .unwrap_or(false);

        assert!(
            !has_marker,
            "step not found → should skip (unwrap_or(false))"
        );
    }

    /// Condition-skipped steps (status=Skipped) must NOT appear in completed_keys_from_steps,
    /// so they re-evaluate on resume rather than being treated as done.
    #[test]
    fn test_condition_skipped_steps_not_in_completed_keys() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert a Completed step and a Skipped step
        let step_completed = wf_mgr
            .insert_step(&run.id, "detect-db-migrations", "reviewer", false, 0, 0)
            .unwrap();
        set_step_status(&wf_mgr, &step_completed, WorkflowStepStatus::Completed);

        let step_skipped = wf_mgr
            .insert_step(&run.id, "review-db-migrations", "reviewer", false, 1, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_skipped,
                WorkflowStepStatus::Skipped,
                None,
                Some("skipped: detect-db-migrations.has_db_migrations not emitted"),
                None,
                None,
                None,
            )
            .unwrap();

        let steps = wf_mgr.get_workflow_steps(&run.id).unwrap();
        let keys = completed_keys_from_steps(&steps);

        assert!(
            keys.contains(&("detect-db-migrations".to_string(), 0)),
            "Completed step must be in completed_keys"
        );
        assert!(
            !keys.contains(&("review-db-migrations".to_string(), 0)),
            "Skipped step must NOT be in completed_keys (re-evaluates on resume)"
        );
    }

    /// `if`-skipped agents count toward skipped_count (and thus effective_successes),
    /// so the parallel block succeeds even if some calls were condition-skipped.
    #[test]
    fn test_parallel_if_counts_toward_skipped_count() {
        // Scenario: 2 agents. 1 ran and succeeded, 1 was condition-skipped.
        let successes: u32 = 1;
        let skipped_count: u32 = 1; // condition-skipped
        let children_len: u32 = 1; // only the non-skipped agent was spawned

        let effective_successes = successes + skipped_count; // 2
        let total_agents = children_len + skipped_count; // 2
        let min_required: u32 = total_agents; // default: all

        let status = if effective_successes >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        };
        assert_eq!(
            status,
            WorkflowStepStatus::Completed,
            "condition-skipped agents must count toward min_success so parallel block succeeds"
        );
    }

    // ---------------------------------------------------------------------------
    // apply_workflow_input_defaults tests
    // ---------------------------------------------------------------------------

    fn make_workflow_def_with_inputs(
        inputs: Vec<crate::workflow_dsl::InputDecl>,
    ) -> crate::workflow_dsl::WorkflowDef {
        crate::workflow_dsl::WorkflowDef {
            name: "test-wf".to_string(),
            description: String::new(),
            trigger: crate::workflow_dsl::WorkflowTrigger::Manual,
            targets: vec![],
            inputs,
            body: vec![],
            always: vec![],
            source_path: String::new(),
        }
    }

    #[test]
    fn test_apply_workflow_input_defaults_fills_missing_default() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "skip_tests".to_string(),
            required: false,
            default: Some("false".to_string()),
            description: None,
            input_type: Default::default(),
        }]);

        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("false"));
    }

    #[test]
    fn test_apply_workflow_input_defaults_does_not_overwrite_provided_value() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "skip_tests".to_string(),
            required: false,
            default: Some("false".to_string()),
            description: None,
            input_type: Default::default(),
        }]);

        let mut inputs = HashMap::new();
        inputs.insert("skip_tests".to_string(), "true".to_string());
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        // Provided value must not be replaced by the default.
        assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("true"));
    }

    #[test]
    fn test_apply_workflow_input_defaults_errors_on_missing_required() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "ticket_id".to_string(),
            required: true,
            default: None,
            description: None,
            input_type: Default::default(),
        }]);

        let mut inputs = HashMap::new();
        let result = apply_workflow_input_defaults(&workflow, &mut inputs);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("ticket_id"),
            "error message should name the missing input, got: {msg}"
        );
    }

    #[test]
    fn test_apply_workflow_input_defaults_required_input_provided_succeeds() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "ticket_id".to_string(),
            required: true,
            default: None,
            description: None,
            input_type: Default::default(),
        }]);

        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "TKT-1".to_string());
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert_eq!(inputs.get("ticket_id").map(String::as_str), Some("TKT-1"));
    }

    #[test]
    fn test_apply_workflow_input_defaults_no_inputs_is_noop() {
        let workflow = make_workflow_def_with_inputs(vec![]);
        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert!(inputs.is_empty());
    }

    #[test]
    fn test_execute_workflow_ephemeral_skips_concurrent_guard() {
        // Verify that when worktree_id is None (ephemeral run), a second concurrent
        // call at depth==0 does NOT return WorkflowRunAlreadyActive — the guard is
        // intentionally skipped for ephemeral runs which have no registered worktree.
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();

        let workflow = make_empty_workflow();

        // First ephemeral call — must succeed (empty workflow, no agents to spawn).
        let input1 = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result1 = execute_workflow(&input1);
        assert!(
            !matches!(
                result1,
                Err(ConductorError::WorkflowRunAlreadyActive { .. })
            ),
            "first ephemeral call should not be blocked by the concurrent guard"
        );

        // Second ephemeral call — must also not be blocked by the guard, even though
        // the first run's record now exists in the DB (it has no worktree_id, so the
        // guard is skipped entirely for ephemeral runs).
        let input2 = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result2 = execute_workflow(&input2);
        assert!(
            !matches!(
                result2,
                Err(ConductorError::WorkflowRunAlreadyActive { .. })
            ),
            "second ephemeral call should not be blocked by the concurrent guard"
        );
    }

    // ---------------------------------------------------------------------------
    // purge / purge_count tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_purge_all_terminal_statuses() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a3 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a4 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let r_completed = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let r_failed = mgr
            .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();
        let r_cancelled = mgr
            .create_workflow_run("t", Some("w1"), &a3.id, false, "manual", None)
            .unwrap();
        let r_running = mgr
            .create_workflow_run("t", Some("w1"), &a4.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&r_completed.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&r_failed.id, WorkflowRunStatus::Failed, None)
            .unwrap();
        mgr.update_workflow_status(&r_cancelled.id, WorkflowRunStatus::Cancelled, None)
            .unwrap();
        mgr.update_workflow_status(&r_running.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let deleted = mgr
            .purge(None, &["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(deleted, 3);

        // running run must still exist
        assert!(mgr.get_workflow_run(&r_running.id).unwrap().is_some());
        // terminal runs must be gone
        assert!(mgr.get_workflow_run(&r_completed.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&r_failed.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&r_cancelled.id).unwrap().is_none());
    }

    #[test]
    fn test_purge_single_status_filter() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let r_completed = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let r_failed = mgr
            .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&r_completed.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&r_failed.id, WorkflowRunStatus::Failed, None)
            .unwrap();

        // only purge completed
        let deleted = mgr.purge(None, &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        assert!(mgr.get_workflow_run(&r_completed.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&r_failed.id).unwrap().is_some());
    }

    #[test]
    fn test_purge_repo_scoped() {
        let conn = setup_db();
        // Add a second repo + worktree
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/r2', '', 'main', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/feat-other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w2"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run_r1 = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let run_r2 = mgr
            .create_workflow_run("t", Some("w2"), &a2.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&run_r1.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&run_r2.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        // scope to r1 only
        let deleted = mgr.purge(Some("r1"), &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        assert!(mgr.get_workflow_run(&run_r1.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&run_r2.id).unwrap().is_some());
    }

    #[test]
    fn test_purge_cascade_deletes_steps() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        mgr.insert_step(&run.id, "step1", "actor", true, 0, 0)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        let deleted = mgr.purge(None, &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        // steps must be gone (cascade)
        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn test_purge_count_matches_purge() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let r1 = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let r2 = mgr
            .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&r1.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&r2.id, WorkflowRunStatus::Failed, None)
            .unwrap();

        let statuses = &["completed", "failed", "cancelled"];
        let count = mgr.purge_count(None, statuses).unwrap();
        assert_eq!(count, 2);

        let deleted = mgr.purge(None, statuses).unwrap();
        assert_eq!(deleted, count);
    }

    #[test]
    fn test_purge_noop_when_no_matches() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let count = mgr
            .purge_count(None, &["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(count, 0);

        let deleted = mgr
            .purge(None, &["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(deleted, 0);

        assert!(mgr.get_workflow_run(&run.id).unwrap().is_some());
    }

    #[test]
    fn test_purge_empty_statuses_is_noop() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        assert_eq!(mgr.purge(None, &[]).unwrap(), 0);
        assert_eq!(mgr.purge_count(None, &[]).unwrap(), 0);
    }

    /// Repo-scoped purge must NOT delete global workflow runs (worktree_id IS NULL).
    #[test]
    fn test_purge_repo_scoped_does_not_delete_global_runs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);

        // Create a global run (no worktree) and a run scoped to w1.
        let a_global = agent_mgr.create_run(None, "wf", None, None).unwrap();
        let a_w1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run_global = mgr
            .create_workflow_run("t", None, &a_global.id, false, "manual", None)
            .unwrap();
        let run_w1 = mgr
            .create_workflow_run("t", Some("w1"), &a_w1.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&run_global.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&run_w1.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        // Scope purge to r1 — must only delete the worktree-bound run.
        assert_eq!(mgr.purge_count(Some("r1"), &["completed"]).unwrap(), 1);
        let deleted = mgr.purge(Some("r1"), &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        // Global run must survive.
        assert!(mgr.get_workflow_run(&run_global.id).unwrap().is_some());
        // w1 run must be gone.
        assert!(mgr.get_workflow_run(&run_w1.id).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Implicit variable injection tests
    // -----------------------------------------------------------------------

    /// Insert a minimal ticket row into the test DB and return its id.
    fn insert_test_ticket(conn: &Connection, id: &str, repo_id: &str) {
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, \
             labels, url, synced_at, raw_json) \
             VALUES (?1, ?2, 'github', ?3, 'Test ticket title', '', 'open', '[]', \
             'https://github.com/test/repo/issues/1', '2024-01-01T00:00:00Z', '{}')",
            rusqlite::params![id, repo_id, id],
        )
        .unwrap();
    }

    #[test]
    fn test_execute_workflow_injects_repo_variables() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        // repo `r1` with local_path `/tmp/repo` is inserted by setup_db()
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: Some("r1"),
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(&result.workflow_run_id)
            .unwrap()
            .unwrap();

        assert_eq!(run.inputs.get("repo_id").map(String::as_str), Some("r1"));
        assert_eq!(
            run.inputs.get("repo_path").map(String::as_str),
            Some("/tmp/repo")
        );
        assert_eq!(
            run.inputs.get("repo_name").map(String::as_str),
            Some("test-repo")
        );
        // Assert the repo_id column is persisted on the WorkflowRun record itself.
        assert_eq!(run.repo_id.as_deref(), Some("r1"));
        assert_eq!(run.ticket_id, None);
    }

    #[test]
    fn test_execute_workflow_injects_ticket_variables() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        insert_test_ticket(&conn, "tkt-1", "r1");

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: Some("tkt-1"),
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(&result.workflow_run_id)
            .unwrap()
            .unwrap();

        assert_eq!(
            run.inputs.get("ticket_id").map(String::as_str),
            Some("tkt-1")
        );
        assert_eq!(
            run.inputs.get("ticket_title").map(String::as_str),
            Some("Test ticket title")
        );
        assert!(
            run.inputs.contains_key("ticket_url"),
            "ticket_url should be injected"
        );
        // Assert the ticket_id column is persisted on the WorkflowRun record itself.
        assert_eq!(run.ticket_id.as_deref(), Some("tkt-1"));
        assert_eq!(run.repo_id, None);
    }

    #[test]
    fn test_execute_workflow_existing_input_not_overwritten_by_injection() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let mut explicit_inputs = HashMap::new();
        explicit_inputs.insert("repo_name".to_string(), "my-override".to_string());

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: Some("r1"),
            model: None,
            exec_config: &exec_config,
            inputs: explicit_inputs,
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(&result.workflow_run_id)
            .unwrap()
            .unwrap();

        // Caller-supplied repo_name must not be overwritten by the injected value.
        assert_eq!(
            run.inputs.get("repo_name").map(String::as_str),
            Some("my-override")
        );
    }

    #[test]
    fn test_execute_workflow_unknown_ticket_id_returns_error() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: Some("nonexistent-ticket"),
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        assert!(
            execute_workflow(&input).is_err(),
            "referencing a nonexistent ticket_id must return an error"
        );
    }

    #[test]
    fn test_execute_workflow_unknown_repo_id_returns_error() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: None,
            repo_id: Some("nonexistent-repo"),
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        assert!(
            execute_workflow(&input).is_err(),
            "referencing a nonexistent repo_id must return an error"
        );
    }

    #[test]
    fn test_resume_workflow_ephemeral_run_rejected() {
        let conn = setup_db();
        let config = Config::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let snapshot = serde_json::to_string(&make_empty_workflow()).unwrap();
        let run = wf_mgr
            .create_workflow_run(
                "ephemeral-wf",
                None,
                &parent.id,
                false,
                "manual",
                Some(&snapshot),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("step failed"))
            .unwrap();

        let result = resume_workflow(&WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        });
        assert!(result.is_err(), "ephemeral run resume should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ephemeral PR run"),
            "error should mention ephemeral PR run, got: {err}"
        );
    }

    #[test]
    fn test_resume_workflow_repo_target() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: Some("r1"),
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        wf_mgr
            .update_workflow_status(
                &result.workflow_run_id,
                WorkflowRunStatus::Failed,
                Some("step failed"),
            )
            .unwrap();

        let resume_result = resume_workflow(&WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &result.workflow_run_id,
            model: None,
            from_step: None,
            restart: false,
        });
        assert!(
            resume_result.is_ok(),
            "resume of repo-targeted run should succeed: {:?}",
            resume_result.err()
        );
    }

    #[test]
    fn test_resume_workflow_ticket_target() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        insert_test_ticket(&conn, "tkt-1", "r1");

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: Some("tkt-1"),
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            run_id_notify: None,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        wf_mgr
            .update_workflow_status(
                &result.workflow_run_id,
                WorkflowRunStatus::Failed,
                Some("step failed"),
            )
            .unwrap();

        let resume_result = resume_workflow(&WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &result.workflow_run_id,
            model: None,
            from_step: None,
            restart: false,
        });
        assert!(
            resume_result.is_ok(),
            "resume of ticket-targeted run should succeed: {:?}",
            resume_result.err()
        );
    }

    // ── helpers shared by chain/step-summary tests ───────────────────────────

    /// Insert a minimal workflow_run directly into the DB for testing chain walks.
    /// Creates a throwaway agent_run to satisfy the `parent_run_id` FK constraint.
    fn insert_workflow_run(
        conn: &Connection,
        id: &str,
        name: &str,
        status: &str,
        parent_workflow_run_id: Option<&str>,
    ) {
        // Create a dummy agent_run so the FK on parent_run_id is satisfied.
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
              parent_workflow_run_id) \
             VALUES (?1, ?2, NULL, ?3, ?4, 0, 'manual', '2025-01-01T00:00:00Z', ?5)",
            params![id, name, parent.id, status, parent_workflow_run_id],
        )
        .unwrap();
    }

    /// Insert a workflow_run_step in 'running' status for the given run.
    fn insert_running_step(conn: &Connection, step_id: &str, run_id: &str, step_name: &str) {
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration) \
             VALUES (?1, ?2, ?3, 'actor', 0, 'running', 1)",
            params![step_id, run_id, step_name],
        )
        .unwrap();
    }

    // ── list_root_workflow_runs ───────────────────────────────────────────────

    #[test]
    fn test_list_root_workflow_runs_excludes_children() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));

        let mgr = WorkflowManager::new(&conn);
        let roots = mgr.list_root_workflow_runs(100).unwrap();
        let ids: Vec<&str> = roots.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"root1"), "root run should appear");
        assert!(!ids.contains(&"child1"), "child run must not appear");
    }

    #[test]
    fn test_list_root_workflow_runs_empty() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let roots = mgr.list_root_workflow_runs(100).unwrap();
        assert!(roots.is_empty());
    }

    // ── get_active_chain_for_run ──────────────────────────────────────────────

    #[test]
    fn test_get_active_chain_no_children() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);

        let mgr = WorkflowManager::new(&conn);
        let chain = mgr.get_active_chain_for_run("root1").unwrap();
        assert!(chain.is_empty(), "no children → empty chain");
    }

    #[test]
    fn test_get_active_chain_single_child() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));

        let mgr = WorkflowManager::new(&conn);
        let chain = mgr.get_active_chain_for_run("root1").unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].0, "child1");
        assert_eq!(chain[0].1, "child-wf");
    }

    #[test]
    fn test_get_active_chain_two_deep() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
        insert_workflow_run(&conn, "grand1", "grand-wf", "running", Some("child1"));

        let mgr = WorkflowManager::new(&conn);
        let chain = mgr.get_active_chain_for_run("root1").unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0], ("child1".to_string(), "child-wf".to_string()));
        assert_eq!(chain[1], ("grand1".to_string(), "grand-wf".to_string()));
    }

    #[test]
    fn test_get_active_chain_ignores_terminal_children() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        // completed child — must not appear in active chain
        insert_workflow_run(&conn, "child1", "child-wf", "completed", Some("root1"));

        let mgr = WorkflowManager::new(&conn);
        let chain = mgr.get_active_chain_for_run("root1").unwrap();
        assert!(chain.is_empty(), "completed child must not appear in chain");
    }

    // ── get_step_summaries_for_runs ───────────────────────────────────────────

    #[test]
    fn test_get_step_summaries_no_children() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        insert_running_step(&conn, "step1", "root1", "my-step");

        let mgr = WorkflowManager::new(&conn);
        let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
        let s = summaries.get("root1").expect("summary should exist");
        assert_eq!(s.step_name, "my-step");
        assert_eq!(s.iteration, 1);
        // single-level: chain is empty
        assert!(s.workflow_chain.is_empty());
    }

    #[test]
    fn test_get_step_summaries_with_child_chain() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
        // running step is on the child (leaf)
        insert_running_step(&conn, "step1", "child1", "leaf-step");

        let mgr = WorkflowManager::new(&conn);
        let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
        let s = summaries.get("root1").expect("summary should exist");
        assert_eq!(s.step_name, "leaf-step");
        // workflow_chain is [root_name] because child is the leaf (excluded)
        assert_eq!(s.workflow_chain, vec!["root-wf"]);
    }

    #[test]
    fn test_get_step_summaries_two_deep_chain() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        insert_workflow_run(&conn, "child1", "child-wf", "running", Some("root1"));
        insert_workflow_run(&conn, "grand1", "grand-wf", "running", Some("child1"));
        insert_running_step(&conn, "step1", "grand1", "grand-step");

        let mgr = WorkflowManager::new(&conn);
        let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
        let s = summaries.get("root1").expect("summary should exist");
        assert_eq!(s.step_name, "grand-step");
        // root + first child (grand is leaf, excluded)
        assert_eq!(s.workflow_chain, vec!["root-wf", "child-wf"]);
    }

    #[test]
    fn test_get_step_summaries_empty_run_ids() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let summaries = mgr.get_step_summaries_for_runs(&[]).unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_get_step_summaries_no_running_step() {
        let conn = setup_db();
        insert_workflow_run(&conn, "root1", "root-wf", "running", None);
        // no steps inserted

        let mgr = WorkflowManager::new(&conn);
        let summaries = mgr.get_step_summaries_for_runs(&["root1"]).unwrap();
        assert!(
            !summaries.contains_key("root1"),
            "no running step → no entry in map"
        );
    }

    // --- resolve_run_context tests ---

    /// Helper: create a minimal workflow_run row with explicit worktree_id / repo_id.
    fn insert_workflow_run_with_targets(
        conn: &Connection,
        worktree_id: Option<&str>,
        repo_id: Option<&str>,
    ) -> String {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
        let mgr = WorkflowManager::new(conn);
        let run = mgr
            .create_workflow_run_with_targets(
                "test-wf",
                worktree_id,
                None,
                repo_id,
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();
        run.id
    }

    #[test]
    fn test_resolve_run_context_run_not_found() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = WorkflowManager::new(&conn);
        let err = mgr
            .resolve_run_context("nonexistent-id", &config)
            .unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {err}"
        );
    }

    #[test]
    fn test_resolve_run_context_worktree_path_exists() {
        let conn = setup_db();
        let config = Config::default();

        // Create a real temp directory so the disk-existence guard passes.
        let tmp = std::env::temp_dir().join("conductor_test_wt_path_exists");
        std::fs::create_dir_all(&tmp).unwrap();
        let wt_path = tmp.to_string_lossy().to_string();

        // Insert a worktree pointing at the real temp dir.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt-exists', 'r1', 'test-wt', 'feat/test', ?1, 'active', '2024-01-01T00:00:00Z')",
            params![wt_path],
        )
        .unwrap();

        let run_id = insert_workflow_run_with_targets(&conn, Some("wt-exists"), None);
        let mgr = WorkflowManager::new(&conn);
        let ctx = mgr.resolve_run_context(&run_id, &config).unwrap();

        assert_eq!(ctx.working_dir, wt_path);
        assert_eq!(ctx.repo_path, "/tmp/repo"); // repo r1 from setup_db
        assert_eq!(ctx.worktree_id.as_deref(), Some("wt-exists"));
        assert_eq!(ctx.repo_id.as_deref(), Some("r1"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_resolve_run_context_worktree_path_missing() {
        let conn = setup_db();
        let config = Config::default();

        // setup_db inserts worktree w1 at /tmp/ws/feat-test which does not exist.
        // Verify the guard rejects it.
        let run_id = insert_workflow_run_with_targets(&conn, Some("w1"), None);
        let mgr = WorkflowManager::new(&conn);
        let err = mgr.resolve_run_context(&run_id, &config).unwrap_err();
        assert!(
            err.to_string().contains("no longer exists on disk"),
            "expected disk-existence error, got: {err}"
        );
    }

    #[test]
    fn test_resolve_run_context_repo_only() {
        let conn = setup_db();
        let config = Config::default();

        // Run with only repo_id (no worktree).
        let run_id = insert_workflow_run_with_targets(&conn, None, Some("r1"));
        let mgr = WorkflowManager::new(&conn);
        let ctx = mgr.resolve_run_context(&run_id, &config).unwrap();

        assert_eq!(ctx.working_dir, "/tmp/repo");
        assert_eq!(ctx.repo_path, "/tmp/repo");
        assert!(ctx.worktree_id.is_none());
        assert_eq!(ctx.repo_id.as_deref(), Some("r1"));
    }

    #[test]
    fn test_resolve_run_context_no_worktree_no_repo() {
        let conn = setup_db();
        let config = Config::default();

        // Run with neither worktree nor repo.
        let run_id = insert_workflow_run_with_targets(&conn, None, None);
        let mgr = WorkflowManager::new(&conn);
        let err = mgr.resolve_run_context(&run_id, &config).unwrap_err();
        assert!(
            err.to_string()
                .contains("has no associated worktree or repo"),
            "expected missing-targets error, got: {err}"
        );
    }

    // ── reap_orphaned_workflow_runs ───────────────────────────────────────────

    /// Insert a workflow run in 'waiting' status with a waiting gate step.
    /// The parent agent run is created with the given `parent_status`.
    /// Returns `(run_id, step_id)`.
    fn insert_waiting_run_with_gate(
        conn: &Connection,
        run_id: &str,
        parent_status: &str,
        gate_timeout: Option<&str>,
        step_started_at: Option<&str>,
    ) -> String {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();

        // Set the parent agent run to the requested status directly.
        conn.execute(
            "UPDATE agent_runs SET status = ?1 WHERE id = ?2",
            params![parent_status, parent.id],
        )
        .unwrap();

        // Create the workflow run in 'waiting' status.
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, parent_workflow_run_id) \
             VALUES (?1, 'test-wf', NULL, ?2, 'waiting', 0, 'manual', \
                     '2025-01-01T00:00:00Z', NULL)",
            params![run_id, parent.id],
        )
        .unwrap();

        // Insert a waiting gate step.
        let step_id = crate::new_id();
        let started = step_started_at.unwrap_or("2025-01-01T00:00:00Z");
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration, \
              gate_type, gate_timeout, started_at) \
             VALUES (?1, ?2, 'approval-gate', 'gate', 0, 'waiting', 1, \
                     'human_approval', ?3, ?4)",
            params![step_id, run_id, gate_timeout, started],
        )
        .unwrap();

        step_id
    }

    #[test]
    fn test_reap_orphaned_workflow_runs_dead_parent() {
        let conn = setup_db();
        let run_id = "run-dead-parent";
        insert_waiting_run_with_gate(&conn, run_id, "failed", Some("86400s"), None);

        let mgr = WorkflowManager::new(&conn);
        let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
        assert_eq!(reaped, 1);

        // Run should be cancelled.
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled");

        // Gate step should be timed_out.
        let step_status: String = conn
            .query_row(
                "SELECT status FROM workflow_run_steps WHERE workflow_run_id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(step_status, "timed_out");
    }

    #[test]
    fn test_reap_orphaned_workflow_runs_gate_timeout_elapsed() {
        let conn = setup_db();
        let run_id = "run-gate-timeout";
        // Parent is still running but gate started long ago with a 1s timeout.
        insert_waiting_run_with_gate(
            &conn,
            run_id,
            "running",
            Some("1s"),
            Some("2020-01-01T00:00:00Z"), // well in the past
        );

        let mgr = WorkflowManager::new(&conn);
        let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
        assert_eq!(reaped, 1);

        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[test]
    fn test_reap_orphaned_workflow_runs_skips_active_parent() {
        let conn = setup_db();
        let run_id = "run-active-parent";
        // Parent is running, gate timeout is huge — not orphaned.
        // Use a future started_at to ensure the timeout check also passes.
        insert_waiting_run_with_gate(
            &conn,
            run_id,
            "running",
            Some("999999999s"),
            Some("2099-01-01T00:00:00Z"),
        );

        let mgr = WorkflowManager::new(&conn);
        let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
        assert_eq!(reaped, 0);

        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "waiting", "run must remain waiting");
    }

    #[test]
    fn test_reap_orphaned_workflow_runs_skips_terminal() {
        let conn = setup_db();
        // Insert a completed run — must not be touched.
        insert_workflow_run(&conn, "run-completed", "test-wf", "completed", None);
        // Insert a cancelled run — must not be touched.
        insert_workflow_run(&conn, "run-cancelled", "test-wf", "cancelled", None);

        let mgr = WorkflowManager::new(&conn);
        let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
        assert_eq!(reaped, 0);
    }

    #[test]
    fn test_reap_orphaned_workflow_runs_purged_parent() {
        // A workflow run whose parent agent run no longer exists in the DB
        // must still be reaped (parent_status == None → treat as dead).
        // We insert the workflow run with FK checks disabled so we can
        // reference a non-existent agent_run ID, simulating a purged parent.
        let conn = setup_db();
        let run_id = "run-purged-parent";
        let ghost_parent_id = "ghost-agent-run-does-not-exist";

        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
              started_at, parent_workflow_run_id) \
             VALUES (?1, 'test-wf', NULL, ?2, 'waiting', 0, 'manual', \
                     '2025-01-01T00:00:00Z', NULL)",
            params![run_id, ghost_parent_id],
        )
        .unwrap();

        let step_id = crate::new_id();
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, iteration, \
              gate_type, gate_timeout, started_at) \
             VALUES (?1, ?2, 'approval-gate', 'gate', 0, 'waiting', 1, \
                     'human_approval', '999999999s', '2099-01-01T00:00:00Z')",
            params![step_id, run_id],
        )
        .unwrap();

        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        let mgr = WorkflowManager::new(&conn);
        let reaped = mgr.reap_orphaned_workflow_runs().unwrap();
        assert_eq!(
            reaped, 1,
            "purged parent should cause the workflow run to be reaped"
        );

        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[test]
    fn test_list_workflow_runs_paginated_limit_and_offset() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Create 5 runs for worktree w1
        for i in 0..5 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf-paginated-{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(
                &format!("paginated-flow-{i}"),
                Some("w1"),
                &p.id,
                false,
                "manual",
                None,
            )
            .unwrap();
        }

        // First page: limit=2, offset=0
        let page1 = mgr.list_workflow_runs_paginated("w1", 2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        // Second page: limit=2, offset=2
        let page2 = mgr.list_workflow_runs_paginated("w1", 2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        // Third page: limit=2, offset=4 — only 1 remaining
        let page3 = mgr.list_workflow_runs_paginated("w1", 2, 4).unwrap();
        assert_eq!(page3.len(), 1);

        // Pages must not overlap
        let ids1: Vec<_> = page1.iter().map(|r| r.id.clone()).collect();
        let ids2: Vec<_> = page2.iter().map(|r| r.id.clone()).collect();
        assert!(
            ids1.iter().all(|id| !ids2.contains(id)),
            "page1 and page2 must not share runs"
        );

        // All 5 runs returned when limit exceeds count
        let all = mgr.list_workflow_runs_paginated("w1", 100, 0).unwrap();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn test_list_workflow_runs_paginated_filters_by_worktree() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        let p1 = agent_mgr
            .create_run(Some("w1"), "wf-w1", None, None)
            .unwrap();
        let p2 = agent_mgr
            .create_run(Some("w2"), "wf-w2", None, None)
            .unwrap();
        mgr.create_workflow_run("run-w1", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("run-w2", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        let w1_runs = mgr.list_workflow_runs_paginated("w1", 100, 0).unwrap();
        assert_eq!(w1_runs.len(), 1);
        assert_eq!(w1_runs[0].workflow_name, "run-w1");

        let w2_runs = mgr.list_workflow_runs_paginated("w2", 100, 0).unwrap();
        assert_eq!(w2_runs.len(), 1);
        assert_eq!(w2_runs[0].workflow_name, "run-w2");
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_offset_pagination() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Create 4 runs for repo r1 (all on active worktree w1)
        for i in 0..4 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf-repo-{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run_with_targets(
                &format!("repo-flow-{i}"),
                Some("w1"),
                None,
                Some("r1"),
                &p.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .unwrap();
        }

        // First page
        let page1 = mgr.list_workflow_runs_by_repo_id("r1", 2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        // Second page
        let page2 = mgr.list_workflow_runs_by_repo_id("r1", 2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        // Pages must not overlap
        let ids1: Vec<_> = page1.iter().map(|r| r.id.clone()).collect();
        let ids2: Vec<_> = page2.iter().map(|r| r.id.clone()).collect();
        assert!(
            ids1.iter().all(|id| !ids2.contains(id)),
            "page1 and page2 must not share runs"
        );

        // Beyond end returns empty
        let beyond = mgr.list_workflow_runs_by_repo_id("r1", 2, 10).unwrap();
        assert!(beyond.is_empty());
    }

    // ---------------------------------------------------------------------------
    // cancel_run tests
    // ---------------------------------------------------------------------------

    fn make_workflow_run(
        conn: &Connection,
    ) -> (WorkflowManager<'_>, crate::agent::AgentRun, WorkflowRun) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(conn);
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        (mgr, parent, run)
    }

    #[test]
    fn test_cancel_run_pending() {
        let conn = setup_db();
        let (mgr, _parent, run) = make_workflow_run(&conn);
        assert_eq!(run.status, WorkflowRunStatus::Pending);

        mgr.cancel_run(&run.id, "user requested").unwrap();

        let updated = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, WorkflowRunStatus::Cancelled);
    }

    #[test]
    fn test_cancel_run_running_with_active_steps() {
        let conn = setup_db();
        let (mgr, _parent, run) = make_workflow_run(&conn);

        // Advance run to Running
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        // Insert a Running step with a child agent run
        let child_agent_mgr = AgentManager::new(&conn);
        let child = child_agent_mgr
            .create_run(Some("w1"), "child-step", None, None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "do-work", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child.id),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // Cancel the run — should cancel step and child agent run
        mgr.cancel_run(&run.id, "abort").unwrap();

        let updated_run = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(updated_run.status, WorkflowRunStatus::Cancelled);

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);

        let agent_run: String = conn
            .query_row(
                "SELECT status FROM agent_runs WHERE id = ?1",
                params![child.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(agent_run, "cancelled");
    }

    #[test]
    fn test_cancel_run_waiting_status() {
        let conn = setup_db();
        let (mgr, _parent, run) = make_workflow_run(&conn);

        // Advance run to Waiting (e.g. at a gate)
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Waiting, None)
            .unwrap();

        // Insert a Waiting step (no child run)
        let step_id = mgr
            .insert_step(&run.id, "human-gate", "gate", false, 0, 0)
            .unwrap();
        set_step_status(&mgr, &step_id, WorkflowStepStatus::Waiting);

        mgr.cancel_run(&run.id, "timed out").unwrap();

        let updated = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, WorkflowRunStatus::Cancelled);

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
    }

    #[test]
    fn test_cancel_run_skips_terminal_steps() {
        let conn = setup_db();
        let (mgr, _parent, run) = make_workflow_run(&conn);

        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        // A completed step — must not be touched
        let done_step = mgr
            .insert_step(&run.id, "already-done", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &done_step,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            None,
            None,
            None,
        )
        .unwrap();

        // An active step — must be cancelled
        let active_step = mgr
            .insert_step(&run.id, "in-progress", "actor", false, 1, 0)
            .unwrap();
        set_step_status(&mgr, &active_step, WorkflowStepStatus::Running);

        mgr.cancel_run(&run.id, "stop").unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        let done = steps.iter().find(|s| s.id == done_step).unwrap();
        let active = steps.iter().find(|s| s.id == active_step).unwrap();

        assert_eq!(
            done.status,
            WorkflowStepStatus::Completed,
            "completed step must not be modified"
        );
        assert_eq!(
            active.status,
            WorkflowStepStatus::Failed,
            "active step must be marked failed"
        );
    }

    #[test]
    fn test_cancel_run_already_terminal_returns_error() {
        let conn = setup_db();
        let (mgr, _parent, run) = make_workflow_run(&conn);

        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        let err = mgr.cancel_run(&run.id, "too late").unwrap_err();
        assert!(
            err.to_string().contains("terminal state"),
            "expected terminal state error, got: {err}"
        );
    }

    #[test]
    fn test_cancel_run_not_found_returns_error() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);

        let err = mgr.cancel_run("nonexistent-id", "reason").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected not-found error, got: {err}"
        );
    }

    // ── find_resumable_child_run ──────────────────────────────────────────────

    #[test]
    fn test_find_resumable_child_run_returns_failed() {
        let conn = setup_db();
        insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
        insert_workflow_run(&conn, "child1", "child-wf", "failed", Some("parent1"));

        let mgr = WorkflowManager::new(&conn);
        let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
        assert!(result.is_some(), "failed child run should be found");
        assert_eq!(result.unwrap().id, "child1");
    }

    #[test]
    fn test_find_resumable_child_run_ignores_completed() {
        let conn = setup_db();
        insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
        insert_workflow_run(&conn, "child1", "child-wf", "completed", Some("parent1"));

        let mgr = WorkflowManager::new(&conn);
        let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
        assert!(result.is_none(), "completed child run must not be returned");
    }

    #[test]
    fn test_find_resumable_child_run_ignores_running() {
        let conn = setup_db();
        insert_workflow_run(&conn, "parent1", "parent-wf", "running", None);
        insert_workflow_run(&conn, "child1", "child-wf", "running", Some("parent1"));

        let mgr = WorkflowManager::new(&conn);
        let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
        assert!(result.is_none(), "running child run must not be returned");
    }

    #[test]
    fn test_find_resumable_child_run_ignores_cancelled() {
        let conn = setup_db();
        insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);
        insert_workflow_run(&conn, "child1", "child-wf", "cancelled", Some("parent1"));

        let mgr = WorkflowManager::new(&conn);
        let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
        assert!(result.is_none(), "cancelled child run must not be returned");
    }

    #[test]
    fn test_find_resumable_child_run_picks_most_recent() {
        let conn = setup_db();
        insert_workflow_run(&conn, "parent1", "parent-wf", "failed", None);

        // Insert two failed child runs with distinct timestamps
        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(None, "workflow", None, None).unwrap();
        let p2 = agent_mgr.create_run(None, "workflow", None, None).unwrap();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
              parent_workflow_run_id) \
             VALUES ('older-child', 'child-wf', NULL, ?1, 'failed', 0, 'manual', \
                     '2025-01-01T00:00:00Z', 'parent1')",
            params![p1.id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at, \
              parent_workflow_run_id) \
             VALUES ('newer-child', 'child-wf', NULL, ?1, 'failed', 0, 'manual', \
                     '2025-06-01T00:00:00Z', 'parent1')",
            params![p2.id],
        )
        .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let result = mgr.find_resumable_child_run("parent1", "child-wf").unwrap();
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().id,
            "newer-child",
            "most recently started child must be returned"
        );
    }
}
