use super::*;
use crate::workflow::engine::record_step_skipped;
use crate::workflow::status::WorkflowStepStatus;
use crate::workflow_dsl::{AgentRef, CallNode, OnFail, ScriptNode, WorkflowNode};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// record_step_skipped
// ---------------------------------------------------------------------------

#[test]
fn test_record_step_skipped_does_not_set_all_succeeded_false() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);
    state.all_succeeded = true;

    record_step_skipped(&mut state, "my-step".to_string(), "my-step");

    assert!(
        state.all_succeeded,
        "all_succeeded must remain true after a skipped step"
    );
    let result = state.step_results.get("my-step").expect("result inserted");
    assert_eq!(result.status, WorkflowStepStatus::Skipped);
}

#[test]
fn test_record_step_skipped_inserts_step_result() {
    let conn = setup_db();
    let mut state = make_test_state(&conn);

    record_step_skipped(&mut state, "lint".to_string(), "lint");

    let result = state
        .step_results
        .get("lint")
        .expect("step result inserted");
    assert_eq!(result.step_name, "lint");
    assert_eq!(result.status, WorkflowStepStatus::Skipped);
    assert!(result.result_text.is_none());
    assert!(result.markers.is_empty());
}

// ---------------------------------------------------------------------------
// Parser: on_fail = continue produces OnFail::Continue
// ---------------------------------------------------------------------------

#[test]
fn test_parse_on_fail_continue_call_node() {
    use crate::workflow_dsl::parse_workflow_str;

    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call plan { on_fail = continue }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(c.on_fail, Some(OnFail::Continue));
        }
        _ => panic!("Expected Call node"),
    }
}

#[test]
fn test_parse_on_fail_continue_call_workflow_node() {
    use crate::workflow_dsl::parse_workflow_str;

    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call workflow sub-wf { on_fail = continue }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::CallWorkflow(n) => {
            assert_eq!(n.on_fail, Some(OnFail::Continue));
        }
        _ => panic!("Expected CallWorkflow node"),
    }
}

#[test]
fn test_parse_on_fail_continue_script_node() {
    use crate::workflow_dsl::parse_workflow_str;

    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    script lint { run = "lint.sh"  on_fail = continue }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Script(s) => {
            assert_eq!(s.on_fail, Some(OnFail::Continue));
        }
        _ => panic!("Expected Script node"),
    }
}

#[test]
fn test_parse_on_fail_agent_still_works() {
    use crate::workflow_dsl::parse_workflow_str;

    let input = r#"
workflow test {
    meta { targets = ["worktree"] }
    call build { on_fail = diagnose }
}
"#;
    let def = parse_workflow_str(input, "test.wf").unwrap();
    match &def.body[0] {
        WorkflowNode::Call(c) => {
            assert_eq!(
                c.on_fail,
                Some(OnFail::Agent(AgentRef::Name("diagnose".to_string())))
            );
        }
        _ => panic!("Expected Call node"),
    }
}

// ---------------------------------------------------------------------------
// collect_agent_names skips OnFail::Continue but includes OnFail::Agent
// ---------------------------------------------------------------------------

#[test]
fn test_collect_agent_names_skips_on_fail_continue() {
    use crate::workflow_dsl::collect_agent_names;

    let nodes = vec![WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("build".to_string()),
        retries: 1,
        on_fail: Some(OnFail::Continue),
        output: None,
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    })];

    let refs = collect_agent_names(&nodes);
    // Only the primary agent; "continue" is not an agent
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0], AgentRef::Name("build".to_string()));
}

#[test]
fn test_collect_agent_names_includes_on_fail_agent() {
    use crate::workflow_dsl::collect_agent_names;

    let nodes = vec![WorkflowNode::Call(CallNode {
        agent: AgentRef::Name("build".to_string()),
        retries: 1,
        on_fail: Some(OnFail::Agent(AgentRef::Name("diagnose".to_string()))),
        output: None,
        with: vec![],
        bot_name: None,
        plugin_dirs: vec![],
    })];

    let refs = collect_agent_names(&nodes);
    assert_eq!(refs.len(), 2);
    assert!(refs.contains(&AgentRef::Name("build".to_string())));
    assert!(refs.contains(&AgentRef::Name("diagnose".to_string())));
}

#[test]
fn test_collect_agent_names_script_on_fail_continue_not_included() {
    use crate::workflow_dsl::collect_agent_names;

    let nodes = vec![WorkflowNode::Script(ScriptNode {
        name: "lint".to_string(),
        run: "lint.sh".to_string(),
        env: HashMap::new(),
        timeout: None,
        retries: 0,
        on_fail: Some(OnFail::Continue),
        bot_name: None,
    })];

    let refs = collect_agent_names(&nodes);
    assert!(
        refs.is_empty(),
        "script with on_fail=continue has no agent refs"
    );
}

// ---------------------------------------------------------------------------
// Executor: script on_fail=continue skips step and doesn't fail the workflow
// ---------------------------------------------------------------------------

#[test]
fn test_script_on_fail_continue_skips_step() {
    use crate::workflow::executors::execute_script;
    use std::os::unix::fs::PermissionsExt;

    let conn = setup_db();
    let config: &'static Config = Box::leak(Box::new(Config::default()));

    // Create a real temp dir and a failing script so path resolution succeeds
    // but execution exits non-zero, exercising the on_fail=continue branch.
    let tmp = std::env::temp_dir().join(format!(
        "conductor-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let script_path = tmp.join("fail.sh");
    std::fs::write(&script_path, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = ExecutionState {
        working_dir: tmp.to_str().unwrap().to_string(),
        repo_path: tmp.to_str().unwrap().to_string(),
        worktree_id: Some("w1".to_string()),
        workflow_run_id: run.id,
        ..crate::workflow::tests::common::make_loop_test_state(&conn, config)
    };
    // Disable fail_fast so on_fail=continue is the only thing stopping failure propagation
    state.exec_config.fail_fast = false;

    let node = ScriptNode {
        name: "failing-script".to_string(),
        run: script_path.to_str().unwrap().to_string(),
        env: HashMap::new(),
        timeout: None,
        retries: 0,
        on_fail: Some(OnFail::Continue),
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    std::fs::remove_dir_all(&tmp).ok();

    assert!(
        result.is_ok(),
        "execute_script must succeed when on_fail=continue: {result:?}"
    );
    assert!(
        state.all_succeeded,
        "all_succeeded must remain true when step is skipped via on_fail=continue"
    );
    let step = state
        .step_results
        .get("failing-script")
        .expect("step result must be recorded");
    assert_eq!(step.status, WorkflowStepStatus::Skipped);
}

#[test]
fn test_script_no_on_fail_still_fails_workflow() {
    use crate::workflow::executors::execute_script;
    use std::os::unix::fs::PermissionsExt;

    let conn = setup_db();
    let config: &'static Config = Box::leak(Box::new(Config::default()));

    let tmp = std::env::temp_dir().join(format!(
        "conductor-test-nofail-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let script_path = tmp.join("fail.sh");
    std::fs::write(&script_path, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr
        .create_run(Some("w1"), "workflow", None, None)
        .unwrap();
    let wf_mgr = WorkflowManager::new(&conn);
    let run = wf_mgr
        .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
        .unwrap();

    let mut state = ExecutionState {
        working_dir: tmp.to_str().unwrap().to_string(),
        repo_path: tmp.to_str().unwrap().to_string(),
        worktree_id: Some("w1".to_string()),
        workflow_run_id: run.id,
        ..crate::workflow::tests::common::make_loop_test_state(&conn, config)
    };
    // fail_fast off so the error is from all_succeeded, not the error return
    state.exec_config.fail_fast = false;

    let node = ScriptNode {
        name: "failing-script".to_string(),
        run: script_path.to_str().unwrap().to_string(),
        env: HashMap::new(),
        timeout: None,
        retries: 0,
        on_fail: None,
        bot_name: None,
    };

    let result = execute_script(&mut state, &node, 0);
    std::fs::remove_dir_all(&tmp).ok();

    assert!(
        result.is_ok(),
        "no fail_fast so result should be Ok: {result:?}"
    );
    assert!(
        !state.all_succeeded,
        "all_succeeded must be false when script fails without on_fail"
    );
    let step = state
        .step_results
        .get("failing-script")
        .expect("step result must be recorded");
    assert_eq!(step.status, WorkflowStepStatus::Failed);
}
