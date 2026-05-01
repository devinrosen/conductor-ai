#![allow(unused_imports)]

use super::*;
use crate::workflow::{
    BlockedOn, ContextEntry, GateType, WorkflowExecConfig, WorkflowRunStep, WorkflowStepSummary,
};

// ---------------------------------------------------------------------------
// BlockedOn serde roundtrips
// ---------------------------------------------------------------------------

#[test]
fn test_blocked_on_human_approval_roundtrip() {
    let val = BlockedOn::HumanApproval {
        gate_name: "review-gate".into(),
        prompt: Some("Please review".into()),
        options: vec![],
    };
    let json = serde_json::to_string(&val).unwrap();
    assert!(json.contains(r#""type":"human_approval""#));
    let deser: BlockedOn = serde_json::from_str(&json).unwrap();
    match deser {
        BlockedOn::HumanApproval {
            gate_name, prompt, ..
        } => {
            assert_eq!(gate_name, "review-gate");
            assert_eq!(prompt.as_deref(), Some("Please review"));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_blocked_on_human_approval_no_prompt() {
    let val = BlockedOn::HumanApproval {
        gate_name: "g".into(),
        prompt: None,
        options: vec![],
    };
    let json = serde_json::to_string(&val).unwrap();
    let deser: BlockedOn = serde_json::from_str(&json).unwrap();
    match deser {
        BlockedOn::HumanApproval { prompt, .. } => assert!(prompt.is_none()),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_blocked_on_human_review_roundtrip() {
    let val = BlockedOn::HumanReview {
        gate_name: "code-review".into(),
        prompt: Some("Check tests".into()),
        options: vec![],
    };
    let json = serde_json::to_string(&val).unwrap();
    assert!(json.contains(r#""type":"human_review""#));
    let deser: BlockedOn = serde_json::from_str(&json).unwrap();
    match deser {
        BlockedOn::HumanReview {
            gate_name, prompt, ..
        } => {
            assert_eq!(gate_name, "code-review");
            assert_eq!(prompt.as_deref(), Some("Check tests"));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_blocked_on_pr_approval_roundtrip() {
    let val = BlockedOn::PrApproval {
        gate_name: "pr-gate".into(),
        approvals_needed: 2,
    };
    let json = serde_json::to_string(&val).unwrap();
    assert!(json.contains(r#""type":"pr_approval""#));
    let deser: BlockedOn = serde_json::from_str(&json).unwrap();
    match deser {
        BlockedOn::PrApproval {
            gate_name,
            approvals_needed,
        } => {
            assert_eq!(gate_name, "pr-gate");
            assert_eq!(approvals_needed, 2);
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_blocked_on_pr_checks_roundtrip() {
    let val = BlockedOn::PrChecks {
        gate_name: "ci-gate".into(),
    };
    let json = serde_json::to_string(&val).unwrap();
    assert!(json.contains(r#""type":"pr_checks""#));
    let deser: BlockedOn = serde_json::from_str(&json).unwrap();
    match deser {
        BlockedOn::PrChecks { gate_name } => {
            assert_eq!(gate_name, "ci-gate");
        }
        _ => panic!("wrong variant"),
    }
}

// ---------------------------------------------------------------------------
// WorkflowRunStatus serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_workflow_run_status_serde_roundtrip() {
    let statuses = vec![
        WorkflowRunStatus::Pending,
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Completed,
        WorkflowRunStatus::Failed,
        WorkflowRunStatus::Cancelled,
        WorkflowRunStatus::Waiting,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let deser: WorkflowRunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, deser);
    }
}

// ---------------------------------------------------------------------------
// WorkflowStepStatus serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_workflow_step_status_serde_roundtrip() {
    let statuses = vec![
        WorkflowStepStatus::Pending,
        WorkflowStepStatus::Running,
        WorkflowStepStatus::Completed,
        WorkflowStepStatus::Failed,
        WorkflowStepStatus::Skipped,
        WorkflowStepStatus::TimedOut,
        WorkflowStepStatus::Waiting,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let deser: WorkflowStepStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, deser);
    }
}

// ---------------------------------------------------------------------------
// ContextEntry serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_context_entry_roundtrip_full() {
    let entry = ContextEntry {
        step: "build".into(),
        iteration: 3,
        context: "built OK".into(),
        markers: vec!["success".into()],
        structured_output: Some(r#"{"ok":true}"#.into()),
        output_file: Some("/tmp/out.txt".into()),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deser: ContextEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.step, "build");
    assert_eq!(deser.iteration, 3);
    assert_eq!(deser.context, "built OK");
    assert_eq!(deser.markers, vec!["success"]);
    assert_eq!(deser.structured_output.as_deref(), Some(r#"{"ok":true}"#));
    assert_eq!(deser.output_file.as_deref(), Some("/tmp/out.txt"));
}

#[test]
fn test_context_entry_roundtrip_defaults() {
    // markers, structured_output, output_file should all default when absent
    let json = r#"{"step":"s","iteration":0,"context":"c"}"#;
    let deser: ContextEntry = serde_json::from_str(json).unwrap();
    assert!(deser.markers.is_empty());
    assert!(deser.structured_output.is_none());
    assert!(deser.output_file.is_none());
}

// ---------------------------------------------------------------------------
// WorkflowStepSummary serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_workflow_step_summary_roundtrip() {
    let summary = WorkflowStepSummary {
        step_name: "deploy".into(),
        iteration: 1,
        workflow_chain: vec!["parent-wf".into(), "child-wf".into()],
    };
    let json = serde_json::to_string(&summary).unwrap();
    let deser: WorkflowStepSummary = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.step_name, "deploy");
    assert_eq!(deser.iteration, 1);
    assert_eq!(deser.workflow_chain, vec!["parent-wf", "child-wf"]);
}

#[test]
fn test_workflow_step_summary_empty_chain() {
    let summary = WorkflowStepSummary {
        step_name: "build".into(),
        iteration: 0,
        workflow_chain: vec![],
    };
    let json = serde_json::to_string(&summary).unwrap();
    let deser: WorkflowStepSummary = serde_json::from_str(&json).unwrap();
    assert!(deser.workflow_chain.is_empty());
}

// ---------------------------------------------------------------------------
// WorkflowRun::is_triggered_by_hook()
// ---------------------------------------------------------------------------

#[test]
fn test_is_triggered_by_hook_true() {
    let conn = setup_db();
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let mut run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "hook",
        None,
    )
    .unwrap();
    run.trigger = "hook".into();
    assert!(run.is_triggered_by_hook());
}

#[test]
fn test_is_triggered_by_hook_false() {
    let conn = setup_db();
    let agent_mgr = crate::agent::AgentManager::new(&conn);
    let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
    let run = crate::workflow::create_workflow_run(
        &conn,
        "test",
        Some("w1"),
        &parent.id,
        false,
        "manual",
        None,
    )
    .unwrap();
    assert!(!run.is_triggered_by_hook());
}

// ---------------------------------------------------------------------------
// WorkflowExecConfig::default()
// ---------------------------------------------------------------------------

#[test]
fn test_workflow_exec_config_default() {
    let cfg = WorkflowExecConfig::default();
    assert_eq!(cfg.poll_interval, std::time::Duration::from_secs(5));
    assert_eq!(
        cfg.step_timeout,
        std::time::Duration::from_secs(12 * 60 * 60)
    );
    assert!(cfg.fail_fast);
    assert!(!cfg.dry_run);
    assert!(cfg.shutdown.is_none());
}

// ---------------------------------------------------------------------------
// resolve_conductor_bin_dir()
// ---------------------------------------------------------------------------

#[test]
fn test_resolve_conductor_bin_dir_returns_some() {
    // In a test binary current_exe() should always succeed
    let dir = crate::workflow::types::resolve_conductor_bin_dir();
    assert!(dir.is_some());
    assert!(dir.unwrap().is_dir());
}
