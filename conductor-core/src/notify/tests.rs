use runkon_notify::DedupStore;

use super::*;
use crate::config::{
    hooks_as_runkon, HookConfig, NotificationConfig, SlackConfig, WorkflowNotificationConfig,
};
use crate::notify::dedup::SqliteDedupStore;
use crate::notify::event::{build_synthetic_event, build_synthetic_for_pattern};
use crate::workflow::GateType;

fn config(enabled: bool, on_success: bool, on_failure: bool) -> NotificationConfig {
    NotificationConfig {
        enabled,
        workflows: Some(WorkflowNotificationConfig {
            on_success,
            on_failure,
            on_gate_human: true,
            on_gate_ci: false,
            on_gate_pr_review: true,
            on_stale: true,
        }),
        slack: SlackConfig::default(),
        web_url: None,
    }
}

fn open_test_db(path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE notification_log (
            entity_id  TEXT NOT NULL,
            event_type TEXT NOT NULL,
            fired_at   TEXT NOT NULL,
            PRIMARY KEY (entity_id, event_type)
        );",
    )
    .unwrap();
    conn
}

// --- should_notify: master enabled guard ---

#[test]
fn should_notify_disabled_suppresses_success() {
    assert!(!should_notify(&config(false, true, true), true));
}

#[test]
fn should_notify_disabled_suppresses_failure() {
    assert!(!should_notify(&config(false, true, true), false));
}

// --- should_notify: per-event guards ---

#[test]
fn should_notify_on_success_false_suppresses_success() {
    assert!(!should_notify(&config(true, false, true), true));
}

#[test]
fn should_notify_on_success_false_allows_failure() {
    assert!(should_notify(&config(true, false, true), false));
}

#[test]
fn should_notify_on_failure_false_suppresses_failure() {
    assert!(!should_notify(&config(true, true, false), false));
}

#[test]
fn should_notify_on_failure_false_allows_success() {
    assert!(should_notify(&config(true, true, false), true));
}

#[test]
fn should_notify_all_enabled_passes_both() {
    assert!(should_notify(&config(true, true, true), true));
    assert!(should_notify(&config(true, true, true), false));
}

// --- notification_body: body-formatting branches ---

#[test]
fn notification_body_with_label() {
    assert_eq!(
        notification_body("my-workflow", Some("main")),
        "my-workflow on main"
    );
}

#[test]
fn notification_body_without_label() {
    assert_eq!(notification_body("my-workflow", None), "my-workflow");
}

// --- SqliteDedupStore ---

#[test]
fn dedup_store_first_claim_wins() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    open_test_db(tmp.path()); // create schema
    let store = SqliteDedupStore::new(tmp.path().to_path_buf());
    assert!(store.try_claim("entity-1", "completed").unwrap());
}

#[test]
fn dedup_store_duplicate_returns_false() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    open_test_db(tmp.path());
    let store = SqliteDedupStore::new(tmp.path().to_path_buf());
    assert!(store.try_claim("entity-1", "completed").unwrap());
    assert!(!store.try_claim("entity-1", "completed").unwrap());
}

#[test]
fn dedup_store_different_event_types_independent() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    open_test_db(tmp.path());
    let store = SqliteDedupStore::new(tmp.path().to_path_buf());
    assert!(store.try_claim("entity-1", "completed").unwrap());
    assert!(store.try_claim("entity-1", "failed").unwrap());
}

#[test]
fn dedup_store_different_entities_independent() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    open_test_db(tmp.path());
    let store = SqliteDedupStore::new(tmp.path().to_path_buf());
    assert!(store.try_claim("entity-1", "completed").unwrap());
    assert!(store.try_claim("entity-2", "completed").unwrap());
}

// --- HookConfig.to_runkon_hook_config() translation ---

#[test]
fn hook_config_translation_basic() {
    let hook = HookConfig {
        on: "workflow_run.*".into(),
        run: Some("echo hello".into()),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    assert_eq!(rk.on, "workflow_run.*");
    assert_eq!(rk.run.as_deref(), Some("echo hello"));
    assert!(rk.when_field_eq.is_none());
    assert!(rk.when_field_in.is_none());
    assert!(rk.when_field_glob.is_none());
    assert!(rk.when_field_gte.is_none());
    assert!(rk.when_field_lte.is_none());
}

#[test]
fn hook_config_translation_workflow_filter() {
    let hook = HookConfig {
        on: "*".into(),
        workflow: Some("my-workflow".into()),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    let eq = rk.when_field_eq.unwrap();
    assert_eq!(eq.get("workflow_name").map(String::as_str), Some("my-workflow"));
}

#[test]
fn hook_config_translation_repo_and_step_filters() {
    let hook = HookConfig {
        on: "*".into(),
        repo: Some("my-repo".into()),
        step: Some("deploy".into()),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    let in_map = rk.when_field_in.unwrap();
    assert_eq!(
        in_map.get("repo_slug").map(|v| v.as_slice()),
        Some(["my-repo".to_string()].as_slice())
    );
    assert_eq!(
        in_map.get("step_name").map(|v| v.as_slice()),
        Some(["deploy".to_string()].as_slice())
    );
}

#[test]
fn hook_config_translation_branch_glob() {
    let hook = HookConfig {
        on: "*".into(),
        branch: Some("release/*".into()),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    let glob = rk.when_field_glob.unwrap();
    assert_eq!(glob.get("branch").map(String::as_str), Some("release/*"));
}

#[test]
fn hook_config_translation_threshold_multiple() {
    let hook = HookConfig {
        on: "workflow_run.cost_spike".into(),
        threshold_multiple: Some(3.0),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    let gte = rk.when_field_gte.unwrap();
    assert_eq!(gte.get("multiple").copied(), Some(3.0));
}

#[test]
fn hook_config_translation_gate_pending_ms() {
    let hook = HookConfig {
        on: "gate.pending_too_long".into(),
        gate_pending_ms: Some(60_000),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    let gte = rk.when_field_gte.unwrap();
    assert_eq!(gte.get("pending_ms").copied(), Some(60_000.0));
}

#[test]
fn hook_config_translation_root_workflows_only_appends_root_suffix() {
    let hook = HookConfig {
        on: "workflow_run.completed,workflow_run.failed".into(),
        root_workflows_only: Some(true),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    assert_eq!(rk.on, "workflow_run.completed:root,workflow_run.failed:root");
}

#[test]
fn hook_config_translation_root_workflows_only_does_not_double_suffix() {
    let hook = HookConfig {
        on: "workflow_run.completed:root".into(),
        root_workflows_only: Some(true),
        ..Default::default()
    };
    let rk = hook.to_runkon_hook_config();
    assert_eq!(rk.on, "workflow_run.completed:root");
}

#[test]
fn hooks_as_runkon_translates_all() {
    let hooks = vec![
        HookConfig {
            on: "workflow_run.*".into(),
            run: Some("echo a".into()),
            ..Default::default()
        },
        HookConfig {
            on: "gate.waiting".into(),
            url: Some("https://example.com".into()),
            ..Default::default()
        },
    ];
    let rk = hooks_as_runkon(&hooks);
    assert_eq!(rk.len(), 2);
    assert_eq!(rk[0].on, "workflow_run.*");
    assert_eq!(rk[1].on, "gate.waiting");
}

// --- build_synthetic_event ---

#[test]
fn build_synthetic_event_all_valid_kinds() {
    let kinds = [
        "workflow_run.completed",
        "workflow_run.failed",
        "workflow_run.stale",
        "workflow_run.reaped",
        "workflow_run.orphan_resumed",
        "agent_run.completed",
        "agent_run.failed",
        "gate.waiting",
        "feedback.requested",
    ];
    for kind in kinds {
        let result = build_synthetic_event(kind, "2024-01-01T00:00:00Z");
        assert!(result.is_ok(), "expected Ok for '{kind}'");
        assert_eq!(result.unwrap().kind, kind);
    }
}

#[test]
fn build_synthetic_event_unknown_returns_err() {
    let result = build_synthetic_event("bad.event", "t");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("bad.event"));
}

#[test]
fn build_synthetic_for_pattern_wildcard_returns_workflow_completed() {
    let event = build_synthetic_for_pattern("*", "2024-01-01T00:00:00Z");
    assert_eq!(event.kind, "workflow_run.completed");
}

#[test]
fn build_synthetic_for_pattern_exact_match() {
    let event = build_synthetic_for_pattern("gate.waiting", "2024-01-01T00:00:00Z");
    assert_eq!(event.kind, "gate.waiting");
}

#[test]
fn build_synthetic_for_pattern_prefix_match() {
    let event = build_synthetic_for_pattern("agent_run.*", "2024-01-01T00:00:00Z");
    assert_eq!(event.kind, "agent_run.completed");
}

// --- is_root field in workflow events ---

#[test]
fn workflow_event_is_root_field_set_for_root_run() {
    let dummy = rusqlite::Connection::open_in_memory().unwrap();
    let cfg = config(true, true, true);
    let ctx = NotificationCtx {
        conn: &dummy,
        config: &cfg,
        hooks: &[],
    };
    // Just verify it doesn't panic — dedup writes to default_db, not dummy.
    fire_workflow_notification(
        &ctx,
        &WorkflowNotificationArgs {
            run_id: "root-run-test",
            workflow_name: "my-workflow",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None, // → is_root = true
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: None,
            worktree_id: None,
        },
    );
}

// --- fire_workflow_notification: early-return guards ---

#[test]
fn fire_workflow_notification_disabled_early_returns() {
    let dummy = rusqlite::Connection::open_in_memory().unwrap();
    let cfg = config(false, true, true);
    let ctx = NotificationCtx {
        conn: &dummy,
        config: &cfg,
        hooks: &[],
    };
    // disabled + no hooks → early return; must not panic
    fire_workflow_notification(
        &ctx,
        &WorkflowNotificationArgs {
            run_id: "run-1",
            workflow_name: "my-workflow",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: None,
            worktree_id: None,
        },
    );
}

#[test]
fn fire_workflow_notification_on_success_false_early_returns() {
    let dummy = rusqlite::Connection::open_in_memory().unwrap();
    let cfg = config(true, false, true);
    let ctx = NotificationCtx {
        conn: &dummy,
        config: &cfg,
        hooks: &[],
    };
    fire_workflow_notification(
        &ctx,
        &WorkflowNotificationArgs {
            run_id: "run-2",
            workflow_name: "my-workflow",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: None,
            worktree_id: None,
        },
    );
}

// --- deep link URL construction tests ---

#[test]
fn deep_link_all_some_produces_correct_url() {
    let url = build_workflow_deep_link(
        Some("https://conductor.example.ts.net"),
        Some("repo-abc"),
        Some("wt-xyz"),
        "run-dl-1",
    );
    assert_eq!(
        url,
        Some(
            "https://conductor.example.ts.net/repos/repo-abc/worktrees/wt-xyz/workflows/runs/run-dl-1"
                .to_string()
        )
    );
}

#[test]
fn deep_link_trailing_slash_trimmed() {
    let url = build_workflow_deep_link(
        Some("https://conductor.example.ts.net/"),
        Some("repo-abc"),
        Some("wt-xyz"),
        "run-dl-2",
    );
    assert_eq!(
        url,
        Some(
            "https://conductor.example.ts.net/repos/repo-abc/worktrees/wt-xyz/workflows/runs/run-dl-2"
                .to_string()
        )
    );
}

#[test]
fn deep_link_any_none_produces_no_url() {
    assert_eq!(
        build_workflow_deep_link(
            Some("https://conductor.example.ts.net"),
            Some("repo-abc"),
            None,
            "run-dl-3",
        ),
        None
    );
    assert_eq!(
        build_workflow_deep_link(
            Some("https://conductor.example.ts.net"),
            None,
            Some("wt-xyz"),
            "run-dl-3",
        ),
        None
    );
    assert_eq!(
        build_workflow_deep_link(None, Some("repo-abc"), Some("wt-xyz"), "run-dl-3"),
        None
    );
}

// --- gate_notification_text ---

#[test]
fn gate_text_human_approval_with_prompt() {
    let (title, body) = gate_notification_text(
        Some(&GateType::HumanApproval),
        "Deploy to prod",
        "release",
        None,
        Some("Ready to deploy?"),
    );
    assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
    assert_eq!(body, "release \u{2192} Deploy to prod: Ready to deploy?");
}

#[test]
fn gate_text_human_approval_without_prompt() {
    let (title, body) = gate_notification_text(
        Some(&GateType::HumanApproval),
        "Deploy to prod",
        "release",
        None,
        None,
    );
    assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
    assert_eq!(body, "release \u{2192} Deploy to prod");
}

#[test]
fn gate_text_human_review_with_prompt() {
    let (title, body) = gate_notification_text(
        Some(&GateType::HumanReview),
        "Code review",
        "ci-pipeline",
        None,
        Some("Please review the diff"),
    );
    assert_eq!(title, "Conductor \u{2014} Review Requested");
    assert_eq!(
        body,
        "ci-pipeline \u{2192} Code review: Please review the diff"
    );
}

#[test]
fn gate_text_human_review_without_prompt() {
    let (title, body) = gate_notification_text(
        Some(&GateType::HumanReview),
        "Code review",
        "ci-pipeline",
        None,
        None,
    );
    assert_eq!(title, "Conductor \u{2014} Review Requested");
    assert_eq!(body, "ci-pipeline \u{2192} Code review");
}

#[test]
fn gate_text_pr_approval() {
    let (title, body) = gate_notification_text(
        Some(&GateType::PrApproval),
        "wait-for-review",
        "release",
        None,
        None,
    );
    assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
    assert_eq!(body, "release: PR needs review");
}

#[test]
fn gate_text_pr_checks() {
    let (title, body) = gate_notification_text(
        Some(&GateType::PrChecks),
        "wait-for-ci",
        "release",
        None,
        None,
    );
    assert_eq!(title, "Conductor \u{2014} Waiting on CI");
    assert_eq!(body, "release: PR checks running");
}

#[test]
fn gate_text_none_fallback() {
    let (title, body) = gate_notification_text(None, "Deploy to prod", "release", None, None);
    assert_eq!(title, "Conductor \u{2014} Approval Required");
    assert_eq!(body, "release: Deploy to prod");
}

#[test]
fn gate_text_other_falls_back_to_default_title() {
    let other = GateType::Other("future_gate_type".to_string());
    let (title, body) = gate_notification_text(
        Some(&other),
        "Custom step",
        "experimental",
        None,
        Some("Take a look"),
    );
    assert_eq!(title, "Conductor \u{2014} Approval Required");
    assert_eq!(body, "experimental: Custom step");
}

#[test]
fn should_notify_gate_other_passes_when_workflows_enabled() {
    let cfg = config(true, false, false);
    let other = GateType::Other("future_gate_type".to_string());
    assert!(should_notify_gate(&cfg, Some(&other)));
}

#[test]
fn should_notify_gate_other_blocked_by_master_disabled() {
    let cfg = config(false, false, false);
    let other = GateType::Other("future_gate_type".to_string());
    assert!(!should_notify_gate(&cfg, Some(&other)));
}

#[test]
fn grouped_text_other_treated_as_lowest_priority() {
    let other = GateType::Other("future_gate_type".to_string());
    let gate_types = vec![Some(&other), Some(&GateType::PrApproval)];
    let (title, _) = grouped_gate_notification_text(&gate_types, "review", None, 2);
    assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
}

#[test]
fn grouped_text_only_other_uses_default_title() {
    let a = GateType::Other("custom_a".to_string());
    let b = GateType::Other("custom_b".to_string());
    let gate_types = vec![Some(&a), Some(&b)];
    let (title, _) = grouped_gate_notification_text(&gate_types, "review", None, 2);
    assert_eq!(title, "Conductor \u{2014} Approval Required");
}

#[test]
fn gate_text_with_target_label() {
    let (title, body) = gate_notification_text(
        Some(&GateType::HumanApproval),
        "Deploy",
        "release",
        Some("conductor-ai/feat-1095"),
        Some("Ship it?"),
    );
    assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
    assert_eq!(
        body,
        "release on conductor-ai/feat-1095 \u{2192} Deploy: Ship it?"
    );
}

#[test]
fn gate_text_pr_approval_with_target_label() {
    let (title, body) = gate_notification_text(
        Some(&GateType::PrApproval),
        "wait-for-review",
        "release",
        Some("main"),
        None,
    );
    assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
    assert_eq!(body, "release on main: PR needs review");
}

// --- fire_gate_notification: early-return guards ---

#[test]
fn fire_gate_notification_disabled_early_returns() {
    let dummy = rusqlite::Connection::open_in_memory().unwrap();
    let cfg = config(false, true, true);
    // disabled + no hooks → early return; must not panic
    fire_gate_notification(
        &dummy,
        &cfg,
        &[],
        &GateNotificationParams {
            step_id: "step-1",
            step_name: "Deploy to prod",
            workflow_name: "release",
            target_label: None,
            gate_type: None,
            gate_prompt: None,
            repo_slug: "",
            branch: "",
            ticket_url: None,
        },
    );
}

#[test]
fn fire_gate_notification_suppressed_by_gate_type() {
    let dummy = rusqlite::Connection::open_in_memory().unwrap();
    let cfg = config(true, true, true);
    // on_gate_ci is false by default → PrChecks must early-return; must not panic
    fire_gate_notification(
        &dummy,
        &cfg,
        &[],
        &GateNotificationParams {
            step_id: "step-ci-1",
            step_name: "wait-for-ci",
            workflow_name: "release",
            target_label: None,
            gate_type: Some(&GateType::PrChecks),
            gate_prompt: None,
            repo_slug: "",
            branch: "",
            ticket_url: None,
        },
    );
}

// --- should_notify_gate ---

#[test]
fn should_notify_gate_disabled_suppresses_all() {
    let cfg = config(false, true, true);
    assert!(!should_notify_gate(&cfg, None));
    assert!(!should_notify_gate(&cfg, Some(&GateType::HumanApproval)));
    assert!(!should_notify_gate(&cfg, Some(&GateType::PrChecks)));
}

#[test]
fn should_notify_gate_none_always_notifies() {
    let cfg = config(true, true, true);
    assert!(should_notify_gate(&cfg, None));
}

#[test]
fn should_notify_gate_human_approval() {
    let mut cfg = config(true, true, true);
    assert!(should_notify_gate(&cfg, Some(&GateType::HumanApproval)));
    cfg.workflows.as_mut().unwrap().on_gate_human = false;
    assert!(!should_notify_gate(&cfg, Some(&GateType::HumanApproval)));
}

#[test]
fn should_notify_gate_human_review() {
    let mut cfg = config(true, true, true);
    assert!(should_notify_gate(&cfg, Some(&GateType::HumanReview)));
    cfg.workflows.as_mut().unwrap().on_gate_human = false;
    assert!(!should_notify_gate(&cfg, Some(&GateType::HumanReview)));
}

#[test]
fn should_notify_gate_pr_checks_default_false() {
    let cfg = config(true, true, true);
    assert!(!should_notify_gate(&cfg, Some(&GateType::PrChecks)));
}

#[test]
fn should_notify_gate_pr_checks_enabled() {
    let mut cfg = config(true, true, true);
    cfg.workflows.as_mut().unwrap().on_gate_ci = true;
    assert!(should_notify_gate(&cfg, Some(&GateType::PrChecks)));
}

#[test]
fn should_notify_gate_pr_approval() {
    let mut cfg = config(true, true, true);
    assert!(should_notify_gate(&cfg, Some(&GateType::PrApproval)));
    cfg.workflows.as_mut().unwrap().on_gate_pr_review = false;
    assert!(!should_notify_gate(&cfg, Some(&GateType::PrApproval)));
}
