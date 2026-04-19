use super::*;
use crate::agent::{AgentRun, AgentRunStatus};
use crate::config::{HookConfig, NotificationConfig, SlackConfig, WorkflowNotificationConfig};
use crate::workflow::{WorkflowRun, WorkflowRunStatus};
use crate::workflow_dsl::GateType;
#[allow(unused_imports)]
use rusqlite::Connection;

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

fn config_with_web_url(
    enabled: bool,
    on_success: bool,
    on_failure: bool,
    web_url: &str,
) -> NotificationConfig {
    NotificationConfig {
        enabled,
        workflows: Some(WorkflowNotificationConfig {
            on_success,
            on_failure,
            on_stale: true,
            on_gate_human: true,
            on_gate_ci: false,
            on_gate_pr_review: true,
        }),
        slack: SlackConfig::default(),
        web_url: Some(web_url.to_string()),
    }
}

fn in_memory_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
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

// --- try_claim_notification ---

#[test]
fn try_claim_notification_first_call_wins() {
    let conn = in_memory_db();
    assert!(
        try_claim_notification(&conn, "entity-1", "completed"),
        "first claim must succeed"
    );
}

#[test]
fn try_claim_notification_duplicate_returns_false() {
    let conn = in_memory_db();
    assert!(try_claim_notification(&conn, "entity-1", "completed"));
    assert!(
        !try_claim_notification(&conn, "entity-1", "completed"),
        "duplicate claim must return false"
    );
}

#[test]
fn try_claim_notification_different_event_types_independent() {
    let conn = in_memory_db();
    assert!(try_claim_notification(&conn, "entity-1", "completed"));
    assert!(
        try_claim_notification(&conn, "entity-1", "failed"),
        "different event_type for same entity_id must be independent"
    );
}

#[test]
fn error_path_deterministic_key_deduplicates() {
    // Simulate two concurrent web instances both observing the same workflow
    // failure: they construct the same deterministic key and only the first
    // should win the dedup claim.
    let conn = in_memory_db();
    let key = "wf-err:my-workflow:repo/wt:12345";
    assert!(
        try_claim_notification(&conn, key, "failed"),
        "first instance must claim"
    );
    assert!(
        !try_claim_notification(&conn, key, "failed"),
        "second instance with same key must be deduped"
    );
}

// --- fire_workflow_notification smoke test ---

#[test]
fn fire_workflow_notification_disabled_does_not_claim() {
    let conn = in_memory_db();
    let cfg = config(false, true, true);
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
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
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "disabled config must not write to notification_log"
    );
}

#[test]
fn fire_workflow_notification_disabled_does_not_claim_on_failure() {
    let conn = in_memory_db();
    let cfg = config(false, true, true);
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-6",
            workflow_name: "my-workflow",
            target_label: None,
            succeeded: false,
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
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-6'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "disabled config must not write to notification_log even for failure events"
    );
}

#[test]
fn fire_workflow_notification_on_success_false_does_not_claim_success() {
    let conn = in_memory_db();
    let cfg = config(true, false, true);
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
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
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "on_success=false must not claim for success events"
    );
}

#[test]
fn fire_workflow_notification_on_failure_false_does_not_claim_failure() {
    let conn = in_memory_db();
    let cfg = config(true, true, false); // enabled, on_success=true, on_failure=false
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-5",
            workflow_name: "my-workflow",
            target_label: None,
            succeeded: false,
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
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-5'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "on_failure=false must not claim for failure events"
    );
}

#[test]
fn fire_workflow_notification_enabled_claims_once_for_success() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    // Fire twice — second call must be a no-op (claim already taken).
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-3",
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
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-3",
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
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-3' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "notification_log must contain exactly one row for dedup"
    );
}

#[test]
fn fire_workflow_notification_enabled_claims_once_for_failure() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-4",
            workflow_name: "my-workflow",
            target_label: Some("main"),
            succeeded: false,
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
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-4",
            workflow_name: "my-workflow",
            target_label: Some("main"),
            succeeded: false,
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
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-4' AND event_type = 'failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "notification_log must contain exactly one row for dedup"
    );
}

// --- deep link URL construction tests ---

#[test]
fn deep_link_all_some_produces_correct_url() {
    // Test the URL format directly via the pure helper.
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
            ),
            "deep link URL must match expected format"
        );

    // Also verify that fire_workflow_notification reads web_url from config and fires.
    let conn = in_memory_db();
    let cfg = config_with_web_url(true, true, true, "https://conductor.example.ts.net");
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-dl-1",
            workflow_name: "deploy",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: Some("repo-abc"),
            worktree_id: Some("wt-xyz"),
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-dl-1' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "notification must have been claimed");
}

#[test]
fn deep_link_trailing_slash_trimmed() {
    // Trailing slash on web_url must be stripped so the URL has no double slash.
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
            ),
            "trailing slash on web_url must be trimmed"
        );

    // Confirm fire_workflow_notification still claims the notification.
    let conn = in_memory_db();
    let cfg = config_with_web_url(true, true, true, "https://conductor.example.ts.net/");
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-dl-2",
            workflow_name: "deploy",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: Some("repo-abc"),
            worktree_id: Some("wt-xyz"),
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-dl-2' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "notification must have been claimed with trailing-slash url"
    );
}

#[test]
fn deep_link_any_none_produces_no_url() {
    // Missing worktree_id → no deep link.
    assert_eq!(
        build_workflow_deep_link(
            Some("https://conductor.example.ts.net"),
            Some("repo-abc"),
            None,
            "run-dl-3",
        ),
        None,
        "missing worktree_id must produce None"
    );
    // Missing repo_id → no deep link.
    assert_eq!(
        build_workflow_deep_link(
            Some("https://conductor.example.ts.net"),
            None,
            Some("wt-xyz"),
            "run-dl-3",
        ),
        None,
        "missing repo_id must produce None"
    );
    // Missing web_url → no deep link.
    assert_eq!(
        build_workflow_deep_link(None, Some("repo-abc"), Some("wt-xyz"), "run-dl-3"),
        None,
        "missing web_url must produce None"
    );

    // fire_workflow_notification must still fire (without a deep link) when worktree_id is absent.
    let conn = in_memory_db();
    let cfg = config_with_web_url(true, true, true, "https://conductor.example.ts.net");
    fire_workflow_notification(
        &conn,
        &cfg,
        &[],
        &WorkflowNotificationArgs {
            run_id: "run-dl-3",
            workflow_name: "deploy",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: Some("repo-abc"),
            worktree_id: None, // missing — no deep link
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-dl-3' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "notification must still fire without deep link");
}

// --- fire_feedback_notification smoke test ---

#[test]
fn fire_feedback_notification_disabled_does_not_claim() {
    let conn = in_memory_db();
    let cfg = config(false, true, true);
    fire_feedback_notification(
        &conn,
        &cfg,
        &[],
        &FeedbackNotificationParams {
            request_id: "req-1",
            prompt_preview: "Is this correct?",
            repo_slug: "",
            branch: "",
        },
    );
    // Notification was gated — no claim should have been recorded.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'req-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "disabled config must not write to notification_log"
    );
}

#[test]
fn fire_feedback_notification_enabled_claims_once() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    // Fire twice — second call must be a no-op (claim already taken).
    fire_feedback_notification(
        &conn,
        &cfg,
        &[],
        &FeedbackNotificationParams {
            request_id: "req-2",
            prompt_preview: "preview",
            repo_slug: "",
            branch: "",
        },
    );
    fire_feedback_notification(
        &conn,
        &cfg,
        &[],
        &FeedbackNotificationParams {
            request_id: "req-2",
            prompt_preview: "preview",
            repo_slug: "",
            branch: "",
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'req-2' AND event_type = 'feedback_requested'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "notification_log must contain exactly one row");
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

// --- fire_gate_notification smoke test ---

#[test]
fn fire_gate_notification_disabled_does_not_claim() {
    let conn = in_memory_db();
    let cfg = config(false, true, true);
    fire_gate_notification(
        &conn,
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
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn fire_gate_notification_enabled_claims_once() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    let params = GateNotificationParams {
        step_id: "step-2",
        step_name: "Deploy to prod",
        workflow_name: "release",
        target_label: None,
        gate_type: Some(&GateType::HumanApproval),
        gate_prompt: Some("Ready?"),
        repo_slug: "",
        branch: "",
        ticket_url: None,
    };
    fire_gate_notification(&conn, &cfg, &[], &params);
    fire_gate_notification(&conn, &cfg, &[], &params);
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-2' AND event_type = 'gate_waiting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "notification_log must contain exactly one row");
}

#[test]
fn fire_gate_notification_with_target_label_claims_once() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    let params = GateNotificationParams {
        step_id: "step-3",
        step_name: "Deploy to prod",
        workflow_name: "release",
        target_label: Some("conductor-ai/feat-1095"),
        gate_type: None,
        gate_prompt: None,
        repo_slug: "",
        branch: "",
        ticket_url: None,
    };
    fire_gate_notification(&conn, &cfg, &[], &params);
    fire_gate_notification(&conn, &cfg, &[], &params);
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-3' AND event_type = 'gate_waiting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "notification_log must contain exactly one row even with target_label"
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
    // on_gate_ci defaults to false in config() helper
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

// --- fire_gate_notification: per-gate-type filtering ---

#[test]
fn fire_gate_notification_suppressed_by_gate_type() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    // on_gate_ci is false by default — PrChecks gate should not claim
    fire_gate_notification(
        &conn,
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
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-ci-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "PrChecks gate must not claim when on_gate_ci is false"
    );
}

#[test]
fn fire_gate_notification_human_gate_allowed_by_default() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    fire_gate_notification(
        &conn,
        &cfg,
        &[],
        &GateNotificationParams {
            step_id: "step-human-1",
            step_name: "approve",
            workflow_name: "release",
            target_label: None,
            gate_type: Some(&GateType::HumanApproval),
            gate_prompt: None,
            repo_slug: "",
            branch: "",
            ticket_url: None,
        },
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-human-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "HumanApproval gate must claim when on_gate_human is true"
    );
}

// --- grouped_gate_notification_text ---

#[test]
fn grouped_text_mixed_types_picks_most_urgent() {
    let gate_types = vec![
        Some(&GateType::PrChecks),
        Some(&GateType::HumanApproval),
        Some(&GateType::PrApproval),
    ];
    let (title, body) = grouped_gate_notification_text(&gate_types, "deploy", None, 3);
    assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
    assert_eq!(body, "deploy: 3 gates pending");
}

#[test]
fn grouped_text_all_pr_checks() {
    let gate_types = vec![Some(&GateType::PrChecks), Some(&GateType::PrChecks)];
    let (title, body) = grouped_gate_notification_text(&gate_types, "ci", Some("main"), 2);
    assert_eq!(title, "Conductor \u{2014} Waiting on CI");
    assert_eq!(body, "ci on main: 2 gates pending");
}

#[test]
fn grouped_text_none_gate_types() {
    let gate_types: Vec<Option<&GateType>> = vec![None, None];
    let (title, body) = grouped_gate_notification_text(&gate_types, "release", None, 2);
    assert_eq!(title, "Conductor \u{2014} Approval Required");
    assert_eq!(body, "release: 2 gates pending");
}

#[test]
fn grouped_text_human_review_is_urgent() {
    let gate_types = vec![Some(&GateType::PrApproval), Some(&GateType::HumanReview)];
    let (title, body) = grouped_gate_notification_text(&gate_types, "review", None, 2);
    assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
    assert_eq!(body, "review: 2 gates pending");
}

#[test]
fn grouped_text_with_target_label() {
    let gate_types = vec![Some(&GateType::PrApproval)];
    let (title, body) =
        grouped_gate_notification_text(&gate_types, "release", Some("conductor-ai/feat-1095"), 1);
    assert_eq!(title, "Conductor \u{2014} Awaiting PR Review");
    assert_eq!(body, "release on conductor-ai/feat-1095: 1 gates pending");
}

#[test]
fn grouped_text_all_quality_gates() {
    let gate_types = vec![Some(&GateType::QualityGate), Some(&GateType::QualityGate)];
    let (title, body) = grouped_gate_notification_text(&gate_types, "review", None, 2);
    assert_eq!(title, "Conductor \u{2014} Quality Gate");
    assert_eq!(body, "review: 2 gates pending");
}

#[test]
fn grouped_text_quality_gate_lower_priority_than_human() {
    let gate_types = vec![Some(&GateType::QualityGate), Some(&GateType::HumanApproval)];
    let (title, body) = grouped_gate_notification_text(&gate_types, "review", None, 2);
    assert_eq!(title, "Conductor \u{2014} Awaiting Your Approval");
    assert_eq!(body, "review: 2 gates pending");
}

#[test]
fn grouped_text_quality_gate_wins_over_none() {
    let gate_types = vec![None, Some(&GateType::QualityGate), None];
    let (title, _) = grouped_gate_notification_text(&gate_types, "review", None, 3);
    assert_eq!(title, "Conductor \u{2014} Quality Gate");
}

// --- fire_grouped_gate_notification ---

#[test]
fn fire_grouped_gate_notification_disabled_does_not_claim() {
    let conn = in_memory_db();
    let cfg = config(false, true, true);
    fire_grouped_gate_notification(
        &conn,
        &cfg,
        &[],
        &GroupedGateNotificationParams {
            run_id: "run-g1",
            workflow_name: "deploy",
            target_label: None,
            gate_types: vec![Some(&GateType::HumanApproval)],
            count: 2,
        },
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-g1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn fire_grouped_gate_notification_claims_once() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    let params = GroupedGateNotificationParams {
        run_id: "run-g2",
        workflow_name: "deploy",
        target_label: None,
        gate_types: vec![Some(&GateType::PrChecks), Some(&GateType::PrApproval)],
        count: 2,
    };
    fire_grouped_gate_notification(&conn, &cfg, &[], &params);
    fire_grouped_gate_notification(&conn, &cfg, &[], &params);
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-g2' AND event_type = 'gates_grouped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "grouped notification must claim exactly once");
}

// --- fire_agent_run_notification ---

#[test]
fn fire_agent_run_notification_disabled_does_not_claim() {
    let conn = in_memory_db();
    let cfg = config(false, true, true);
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-1",
            worktree_slug: Some("my-wt"),
            succeeded: true,
            error_msg: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "disabled config must not write to notification_log"
    );
}

#[test]
fn fire_agent_run_notification_success_claims_once() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-2",
            worktree_slug: Some("feat/foo"),
            succeeded: true,
            error_msg: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-2",
            worktree_slug: Some("feat/foo"),
            succeeded: true,
            error_msg: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-2' AND event_type = 'agent_completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn fire_agent_run_notification_failure_claims_once() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-3",
            worktree_slug: Some("fix/bar"),
            succeeded: false,
            error_msg: Some("out of memory"),
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-3",
            worktree_slug: Some("fix/bar"),
            succeeded: false,
            error_msg: Some("out of memory"),
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-3' AND event_type = 'agent_failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn fire_agent_run_notification_on_success_false_suppresses_success() {
    let conn = in_memory_db();
    let cfg = config(true, false, true);
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-4",
            worktree_slug: None,
            succeeded: true,
            error_msg: None,
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-4'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn fire_agent_run_notification_on_failure_false_suppresses_failure() {
    let conn = in_memory_db();
    let cfg = config(true, true, false);
    fire_agent_run_notification(
        &conn,
        &cfg,
        &[],
        &AgentRunNotificationArgs {
            run_id: "agent-5",
            worktree_slug: None,
            succeeded: false,
            error_msg: Some("err"),
            repo_slug: "",
            branch: "",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-5'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

// --- detect_workflow_terminal_transitions ---

fn make_workflow_run(id: &str, name: &str, status: WorkflowRunStatus) -> WorkflowRun {
    WorkflowRun {
        id: id.to_string(),
        workflow_name: name.to_string(),
        status,
        worktree_id: None,
        parent_run_id: String::new(),
        dry_run: false,
        trigger: "manual".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        ended_at: None,
        result_summary: None,
        error: None,
        definition_snapshot: None,
        inputs: std::collections::HashMap::new(),
        ticket_id: None,
        repo_id: None,
        parent_workflow_run_id: None,
        target_label: None,
        default_bot_name: None,
        iteration: 0,
        blocked_on: None,
        workflow_title: None,
        total_input_tokens: None,
        total_output_tokens: None,
        total_cache_read_input_tokens: None,
        total_cache_creation_input_tokens: None,
        total_turns: None,
        total_cost_usd: None,
        total_duration_ms: None,
        model: None,
    }
}

fn make_sub_workflow_run(id: &str, name: &str, status: WorkflowRunStatus) -> WorkflowRun {
    let mut run = make_workflow_run(id, name, status);
    run.parent_workflow_run_id = Some("parent-run-1".to_string());
    run
}

/// On the first tick (`initialized = false`) no transitions are reported even
/// if runs are already terminal — this prevents startup false-positives.
#[test]
fn wf_transitions_no_notifications_before_initialized() {
    let runs = [
        make_workflow_run("r1", "deploy", WorkflowRunStatus::Completed),
        make_workflow_run("r2", "test", WorkflowRunStatus::Failed),
    ];
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let transitions =
        detect_workflow_terminal_transitions(runs.iter(), &mut seen, &mut initialized);

    assert!(
        transitions.is_empty(),
        "expected no transitions on first tick"
    );
    assert!(
        initialized,
        "initialized should be set to true after first tick"
    );
    assert_eq!(seen.len(), 2);
}

/// After initialization, a run that moves from Running → Completed must
/// produce exactly one transition entry.
#[test]
fn wf_transitions_running_to_completed() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Tick 1: seed with a running run
    let tick1 = [make_workflow_run(
        "r1",
        "deploy",
        WorkflowRunStatus::Running,
    )];
    let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
    assert!(t1.is_empty());

    // Tick 2: same run is now Completed
    let tick2 = [make_workflow_run(
        "r1",
        "deploy",
        WorkflowRunStatus::Completed,
    )];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert_eq!(t2.len(), 1);
    assert_eq!(t2[0].run_id, "r1", "run_id should be r1");
    assert_eq!(t2[0].workflow_name, "deploy");
    assert!(t2[0].succeeded, "should be succeeded=true for Completed");
}

/// A run that transitions from Running → Failed must report succeeded=false.
#[test]
fn wf_transitions_running_to_failed() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [make_workflow_run("r1", "build", WorkflowRunStatus::Running)];
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

    let tick2 = [make_workflow_run("r1", "build", WorkflowRunStatus::Failed)];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert_eq!(t2.len(), 1);
    assert!(!t2[0].succeeded, "should be succeeded=false for Failed");
}

/// A run that was already terminal on tick 1 must NOT fire again on tick 2
/// (already-terminal → terminal is not a new transition).
#[test]
fn wf_transitions_already_terminal_no_refire() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Seed the map: run is Completed on first tick (suppressed)
    let tick1 = [make_workflow_run(
        "r1",
        "deploy",
        WorkflowRunStatus::Completed,
    )];
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

    // Second tick: still Completed — should not produce a transition
    let tick2 = [make_workflow_run(
        "r1",
        "deploy",
        WorkflowRunStatus::Completed,
    )];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert!(t2.is_empty(), "completed→completed should not re-fire");
}

/// Runs that disappear from the poll results must be pruned from `seen` to
/// prevent unbounded memory growth.
#[test]
fn wf_transitions_stale_entries_pruned() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [
        make_workflow_run("r1", "deploy", WorkflowRunStatus::Running),
        make_workflow_run("r2", "test", WorkflowRunStatus::Running),
    ];
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
    assert_eq!(seen.len(), 2);

    // r2 disappears from the next poll
    let tick2 = [make_workflow_run(
        "r1",
        "deploy",
        WorkflowRunStatus::Completed,
    )];
    detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert_eq!(seen.len(), 1);
    assert!(seen.contains_key("r1"));
    assert!(!seen.contains_key("r2"), "r2 should have been pruned");
}

/// A resumed run that goes from Failed → Completed without a Running tick in
/// between must fire a notification (the fast-resume path).
#[test]
fn wf_transitions_failed_to_completed_resume() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Tick 1: run is Failed — seeds `seen` without firing (initialized=false)
    let tick1 = [make_workflow_run("r1", "ci", WorkflowRunStatus::Failed)];
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
    assert_eq!(seen[&"r1".to_string()], WorkflowRunStatus::Failed);

    // Tick 2: same run is now Completed (fast resume — no Running tick observed)
    let tick2 = [make_workflow_run("r1", "ci", WorkflowRunStatus::Completed)];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert_eq!(
        t2.len(),
        1,
        "Failed→Completed must fire exactly one notification"
    );
    assert_eq!(t2[0].run_id, "r1", "run_id should be r1");
    assert_eq!(t2[0].workflow_name, "ci", "workflow_name should be ci");
    assert!(t2[0].succeeded, "should be succeeded=true for Completed");
}

/// Sub-workflow completion must NOT produce a transition notification.
#[test]
fn wf_transitions_sub_workflow_completion_suppressed() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Tick 1: sub-workflow is running
    let tick1 = [make_sub_workflow_run(
        "sub1",
        "child-wf",
        WorkflowRunStatus::Running,
    )];
    let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
    assert!(t1.is_empty());

    // Tick 2: sub-workflow completes — no notification expected
    let tick2 = [make_sub_workflow_run(
        "sub1",
        "child-wf",
        WorkflowRunStatus::Completed,
    )];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert!(
        t2.is_empty(),
        "sub-workflow completion should be suppressed"
    );
}

/// Sub-workflow failure must NOT produce a transition notification.
#[test]
fn wf_transitions_sub_workflow_failure_suppressed() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Tick 1: sub-workflow is running
    let tick1 = [make_sub_workflow_run(
        "sub2",
        "child-wf",
        WorkflowRunStatus::Running,
    )];
    let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
    assert!(t1.is_empty());

    // Tick 2: sub-workflow fails — no notification expected
    let tick2 = [make_sub_workflow_run(
        "sub2",
        "child-wf",
        WorkflowRunStatus::Failed,
    )];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert!(t2.is_empty(), "sub-workflow failure should be suppressed");
}

/// A brand-new run that appears already-terminal on the second tick (e.g.
/// very fast completion) must trigger a notification.
#[test]
fn wf_transitions_new_run_appearing_terminal() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Tick 1: some unrelated run to seed initialized=true
    let tick1 = [make_workflow_run(
        "r1",
        "deploy",
        WorkflowRunStatus::Running,
    )];
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

    // Tick 2: a new run "r2" appears already in Completed state
    let tick2 = [
        make_workflow_run("r1", "deploy", WorkflowRunStatus::Running),
        make_workflow_run("r2", "fast-job", WorkflowRunStatus::Completed),
    ];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert_eq!(t2.len(), 1);
    assert_eq!(t2[0].run_id, "r2", "run_id should be r2");
    assert_eq!(t2[0].workflow_name, "fast-job");
}

/// Regression test for ticket/repo-targeted runs (worktree_id IS NULL).
///
/// Previously, `list_active_non_worktree_workflow_runs` filtered to
/// `status IN ('running', 'waiting')`, so completed runs vanished from the query
/// before the detector could observe the transition.  After the fix the query also
/// returns recently-terminated runs, giving the detector at least one tick to fire.
///
/// This test simulates the now-fixed scenario: a non-worktree run appears `running`
/// on tick 1 and `completed` on tick 2 (because the fixed query still returns it).
#[test]
fn wf_transitions_non_worktree_run_completed_fires_notification() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    // Tick 1: non-worktree run is running (worktree_id = None, no parent_workflow_run_id)
    let tick1 = [make_workflow_run(
        "nw1",
        "label-all-tickets",
        WorkflowRunStatus::Running,
    )];
    let t1 = detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);
    assert!(t1.is_empty());

    // Tick 2: same run is now completed — the fixed query keeps it visible via the
    // 60-second recency window, so the detector can observe the transition.
    let tick2 = [make_workflow_run(
        "nw1",
        "label-all-tickets",
        WorkflowRunStatus::Completed,
    )];
    let t2 = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);
    assert_eq!(
        t2.len(),
        1,
        "completed non-worktree run must fire exactly one notification"
    );
    assert_eq!(t2[0].run_id, "nw1");
    assert_eq!(t2[0].workflow_name, "label-all-tickets");
    assert!(t2[0].succeeded, "Completed → succeeded=true");
}

/// When `target_label` has no `'/'`, both `repo_slug` and `branch` must be empty
/// rather than misattributing the whole label as a repo slug.
#[test]
fn wf_transitions_target_label_no_slash_yields_empty_repo_and_branch() {
    let mut run = make_workflow_run("r1", "deploy", WorkflowRunStatus::Running);
    run.target_label = Some("noslash".to_string());

    let tick1 = [run.clone()];
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

    let mut run_done = run;
    run_done.status = WorkflowRunStatus::Completed;
    let tick2 = [run_done];
    let t = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);

    assert_eq!(t.len(), 1);
    assert_eq!(t[0].repo_slug, "", "repo_slug must be empty when no slash");
    assert_eq!(t[0].branch, "", "branch must be empty when no slash");
}

/// When `target_label` is `Some("repo/branch")`, both components are parsed correctly.
#[test]
fn wf_transitions_target_label_with_slash_parses_repo_and_branch() {
    let mut run = make_workflow_run("r1", "deploy", WorkflowRunStatus::Running);
    run.target_label = Some("my-repo/main".to_string());

    let tick1 = [run.clone()];
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;
    detect_workflow_terminal_transitions(tick1.iter(), &mut seen, &mut initialized);

    let mut run_done = run;
    run_done.status = WorkflowRunStatus::Completed;
    let tick2 = [run_done];
    let t = detect_workflow_terminal_transitions(tick2.iter(), &mut seen, &mut initialized);

    assert_eq!(t.len(), 1);
    assert_eq!(t[0].repo_slug, "my-repo");
    assert_eq!(t[0].branch, "main");
}

// --- detect_agent_terminal_transitions ---

fn make_agent_run(id: &str, status: AgentRunStatus) -> AgentRun {
    AgentRun {
        id: id.to_string(),
        worktree_id: None,
        repo_id: None,
        claude_session_id: None,
        prompt: String::new(),
        status,
        result_text: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        ended_at: None,
        tmux_window: None,
        log_file: None,
        model: None,
        plan: None,
        parent_run_id: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        bot_name: None,
        conversation_id: None,
        subprocess_pid: None,
    }
}

#[test]
fn agent_transitions_first_tick_suppresses_all() {
    let runs = [make_agent_run("a1", AgentRunStatus::Completed)];
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;
    let iter = runs.iter().map(|r| (None, r));
    let t = detect_agent_terminal_transitions(iter, &mut seen, &mut initialized);
    assert!(t.is_empty(), "first tick must suppress transitions");
    assert!(initialized);
    assert_eq!(seen.len(), 1);
}

#[test]
fn agent_transitions_running_to_completed_fires() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [make_agent_run("a1", AgentRunStatus::Running)];
    let iter1 = tick1.iter().map(|r| (Some("my-wt"), r));
    detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

    let tick2 = [make_agent_run("a1", AgentRunStatus::Completed)];
    let iter2 = tick2.iter().map(|r| (Some("my-wt"), r));
    let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].run_id, "a1");
    assert!(t[0].succeeded);
    assert_eq!(t[0].worktree_slug.as_deref(), Some("my-wt"));
}

#[test]
fn agent_transitions_already_seen_terminal_does_not_refire() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [make_agent_run("a1", AgentRunStatus::Completed)];
    let iter1 = tick1.iter().map(|r| (None, r));
    detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

    let tick2 = [make_agent_run("a1", AgentRunStatus::Completed)];
    let iter2 = tick2.iter().map(|r| (None, r));
    let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
    assert!(t.is_empty(), "completed→completed must not re-fire");
}

#[test]
fn agent_transitions_stale_entries_pruned() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [
        make_agent_run("a1", AgentRunStatus::Running),
        make_agent_run("a2", AgentRunStatus::Running),
    ];
    let iter1 = tick1.iter().map(|r| (None, r));
    detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);
    assert_eq!(seen.len(), 2);

    let tick2 = [make_agent_run("a1", AgentRunStatus::Completed)];
    let iter2 = tick2.iter().map(|r| (None, r));
    detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
    assert_eq!(seen.len(), 1);
    assert!(!seen.contains_key("a2"), "a2 should have been pruned");
}

#[test]
fn agent_transitions_cancelled_is_terminal() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [make_agent_run("a1", AgentRunStatus::Running)];
    let iter1 = tick1.iter().map(|r| (None, r));
    detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

    let tick2 = [make_agent_run("a1", AgentRunStatus::Cancelled)];
    let iter2 = tick2.iter().map(|r| (None, r));
    let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
    assert_eq!(t.len(), 1);
    assert!(!t[0].succeeded, "Cancelled must report succeeded=false");
}

#[test]
fn agent_transitions_failed_includes_error_msg() {
    let mut seen = std::collections::HashMap::new();
    let mut initialized = false;

    let tick1 = [make_agent_run("a1", AgentRunStatus::Running)];
    let iter1 = tick1.iter().map(|r| (None, r));
    detect_agent_terminal_transitions(iter1, &mut seen, &mut initialized);

    let mut failed_run = make_agent_run("a1", AgentRunStatus::Failed);
    failed_run.result_text = Some("out of memory".to_string());
    let tick2 = [failed_run];
    let iter2 = tick2.iter().map(|r| (None, r));
    let t = detect_agent_terminal_transitions(iter2, &mut seen, &mut initialized);
    assert_eq!(t.len(), 1);
    assert!(!t[0].succeeded);
    assert_eq!(t[0].error_msg.as_deref(), Some("out of memory"));
}

// --- should_notify_gate: QualityGate ---

#[test]
fn should_notify_gate_quality_gate_returns_false() {
    let cfg = config(true, true, true);
    assert!(
        !should_notify_gate(&cfg, Some(&GateType::QualityGate)),
        "quality gates are non-blocking and should never trigger notifications"
    );
}

// --- gate_notification_text: QualityGate ---

#[test]
fn gate_notification_text_quality_gate() {
    let (title, body) = gate_notification_text(
        Some(&GateType::QualityGate),
        "check-quality",
        "review-pr",
        Some("feat/foo"),
        None,
    );
    assert!(title.contains("Quality Gate"), "title: {title}");
    assert!(body.contains("check-quality"), "body: {body}");
    assert!(body.contains("review-pr"), "body: {body}");
}

// --- hooks fire when [notifications] enabled = false ---

fn hook_matching_all() -> HookConfig {
    HookConfig {
        on: "workflow_run.*".to_string(),
        run: None, // no actual shell command in tests
        url: None,
        ..Default::default()
    }
}

/// When `enabled = false` but hooks are configured, the dedup claim MUST be
/// made (and hooks would fire) — desktop/Slack are just skipped.
#[test]
fn hooks_fire_when_notifications_disabled_workflow() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![hook_matching_all()];
    fire_workflow_notification(
        &conn,
        &cfg,
        &hooks,
        &WorkflowNotificationArgs {
            run_id: "run-hooks-1",
            workflow_name: "deploy",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "my-repo",
            branch: "main",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-hooks-1' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "dedup claim must be made when hooks are configured, even with enabled=false"
    );
}

/// Same as above for the failure path.
#[test]
fn hooks_fire_when_notifications_disabled_workflow_failure() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![hook_matching_all()];
    fire_workflow_notification(
        &conn,
        &cfg,
        &hooks,
        &WorkflowNotificationArgs {
            run_id: "run-hooks-2",
            workflow_name: "deploy",
            target_label: None,
            succeeded: false,
            parent_workflow_run_id: None,
            repo_slug: "my-repo",
            branch: "main",
            duration_ms: None,
            ticket_url: None,
            error: Some("out of memory"),
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-hooks-2' AND event_type = 'failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "dedup claim must be made for failures when hooks are configured, even with enabled=false"
    );
}

/// When `enabled = false` and hooks are configured, the feedback path must also
/// make a dedup claim so hooks can fire.
#[test]
fn hooks_fire_when_notifications_disabled_feedback() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![HookConfig {
        on: "feedback.*".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }];
    fire_feedback_notification(
        &conn,
        &cfg,
        &hooks,
        &FeedbackNotificationParams {
            request_id: "req-hooks-1",
            prompt_preview: "Is this correct?",
            repo_slug: "my-repo",
            branch: "main",
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'req-hooks-1' AND event_type = 'feedback_requested'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "dedup claim must be made for feedback when hooks are configured, even with enabled=false"
    );
}

/// `on_success = false` does NOT suppress hooks — hooks have their own event filtering.
/// The dedup claim must be made when hooks are configured even if on_success is false.
#[test]
fn hooks_fire_when_on_success_false_but_hooks_configured() {
    let conn = in_memory_db();
    let cfg = config(true, false, true); // enabled=true, on_success=false
    let hooks = vec![hook_matching_all()];
    fire_workflow_notification(
        &conn,
        &cfg,
        &hooks,
        &WorkflowNotificationArgs {
            run_id: "run-hooks-3",
            workflow_name: "deploy",
            target_label: None,
            succeeded: true,
            parent_workflow_run_id: None,
            repo_slug: "my-repo",
            branch: "main",
            duration_ms: None,
            ticket_url: None,
            error: None,
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-hooks-3' AND event_type = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "dedup claim must be made when hooks are configured, even if on_success=false"
    );
}

/// `fire_agent_run_notification` must make a dedup claim (so hooks can fire)
/// when `enabled = false` but hooks are configured.
#[test]
fn hooks_fire_when_notifications_disabled_agent_run() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![HookConfig {
        on: "agent_run.*".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }];
    fire_agent_run_notification(
        &conn,
        &cfg,
        &hooks,
        &AgentRunNotificationArgs {
            run_id: "agent-hooks-1",
            worktree_slug: Some("my-worktree"),
            succeeded: true,
            error_msg: None,
            repo_slug: "my-repo",
            branch: "main",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-hooks-1' AND event_type = 'agent_completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "dedup claim must be made for agent_run when hooks are configured, even with enabled=false"
    );
}

/// `fire_agent_run_notification` failure path: dedup claim must be made when
/// `enabled = false` but hooks are configured.
#[test]
fn hooks_fire_when_notifications_disabled_agent_run_failure() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![HookConfig {
        on: "agent_run.*".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }];
    fire_agent_run_notification(
        &conn,
        &cfg,
        &hooks,
        &AgentRunNotificationArgs {
            run_id: "agent-hooks-2",
            worktree_slug: None,
            succeeded: false,
            error_msg: Some("exit 1"),
            repo_slug: "my-repo",
            branch: "main",
            duration_ms: None,
            ticket_url: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'agent-hooks-2' AND event_type = 'agent_failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
            count, 1,
            "dedup claim must be made for agent_run failure when hooks are configured, even with enabled=false"
        );
}

/// `fire_gate_notification` must make a dedup claim (so hooks can fire)
/// when `enabled = false` but hooks are configured.
#[test]
fn hooks_fire_when_notifications_disabled_gate() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![HookConfig {
        on: "gate.*".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }];
    fire_gate_notification(
        &conn,
        &cfg,
        &hooks,
        &GateNotificationParams {
            step_id: "gate-hooks-1",
            step_name: "approve",
            workflow_name: "deploy",
            target_label: None,
            gate_type: Some(&GateType::HumanApproval),
            gate_prompt: None,
            repo_slug: "my-repo",
            branch: "main",
            ticket_url: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'gate-hooks-1' AND event_type = 'gate_waiting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "dedup claim must be made for gate when hooks are configured, even with enabled=false"
    );
}

/// `fire_grouped_gate_notification` must make a dedup claim (so hooks can fire)
/// when `enabled = false` but hooks are configured.
#[test]
fn hooks_fire_when_notifications_disabled_grouped_gate() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false
    let hooks = vec![HookConfig {
        on: "gate.*".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }];
    fire_grouped_gate_notification(
        &conn,
        &cfg,
        &hooks,
        &GroupedGateNotificationParams {
            run_id: "grouped-gate-hooks-1",
            workflow_name: "deploy",
            target_label: None,
            gate_types: vec![Some(&GateType::HumanApproval)],
            count: 2,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'grouped-gate-hooks-1' AND event_type = 'gates_grouped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
            count, 1,
            "dedup claim must be made for grouped gate when hooks are configured, even with enabled=false"
        );
}

// --- fire_orphan_resumed_notification tests ---

#[test]
fn orphan_resumed_notification_persists() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    let ids = vec!["run-orphan-1".to_string(), "run-orphan-2".to_string()];

    fire_orphan_resumed_notification(&conn, &cfg, &[], &ids);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE event_type = 'workflow_orphan_resumed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "orphan resumed notification should be persisted");
}

#[test]
fn orphan_resumed_notification_skipped_for_empty_ids() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);

    fire_orphan_resumed_notification(&conn, &cfg, &[], &[]);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE event_type = 'workflow_orphan_resumed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "should not fire for empty run list");
}

#[test]
fn orphan_resumed_notification_deduplicates() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);
    let ids = vec!["run-orphan-dedup".to_string()];

    fire_orphan_resumed_notification(&conn, &cfg, &[], &ids);
    fire_orphan_resumed_notification(&conn, &cfg, &[], &ids);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE event_type = 'workflow_orphan_resumed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "duplicate orphan resumed notification should be deduped"
    );
}

// --- fire_heartbeat_stuck_failed_notification tests ---

#[test]
fn heartbeat_stuck_failed_notification_persists() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);

    fire_heartbeat_stuck_failed_notification(
        &conn,
        &cfg,
        &[],
        "run-stuck-1",
        "deploy",
        Some("myrepo/main"),
        "executor crashed",
    );

    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-stuck-1' AND event_type = 'workflow_run.reaped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "heartbeat stuck failed notification should be persisted"
    );
}

#[test]
fn heartbeat_stuck_failed_notification_deduplicates() {
    let conn = in_memory_db();
    let cfg = config(true, true, true);

    fire_heartbeat_stuck_failed_notification(
        &conn,
        &cfg,
        &[],
        "run-stuck-dedup",
        "deploy",
        None,
        "error 1",
    );
    fire_heartbeat_stuck_failed_notification(
        &conn,
        &cfg,
        &[],
        "run-stuck-dedup",
        "deploy",
        None,
        "error 2",
    );

    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-stuck-dedup' AND event_type = 'workflow_run.reaped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "duplicate heartbeat stuck notification should be deduped"
    );
}

#[test]
fn heartbeat_stuck_failed_notification_skipped_when_disabled() {
    let conn = in_memory_db();
    let cfg = config(false, true, true); // enabled=false

    fire_heartbeat_stuck_failed_notification(
        &conn,
        &cfg,
        &[],
        "run-stuck-disabled",
        "deploy",
        None,
        "error",
    );

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM notification_log WHERE event_type = 'workflow_run.reaped'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "should not fire when notifications disabled");
}

// ── fire_cost_spike_notification ──────────────────────────────────────

fn config_no_legacy() -> NotificationConfig {
    NotificationConfig {
        enabled: true,
        workflows: None,
        slack: SlackConfig::default(),
        web_url: None,
    }
}

fn hook_cost_spike() -> HookConfig {
    HookConfig {
        on: "workflow_run.cost_spike".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }
}

fn hook_duration_spike() -> HookConfig {
    HookConfig {
        on: "workflow_run.duration_spike".to_string(),
        run: None,
        url: None,
        ..Default::default()
    }
}

fn hook_gate_pending() -> HookConfig {
    HookConfig {
        on: "gate.pending_too_long".to_string(),
        run: None,
        url: None,
        gate_pending_ms: Some(1_000_000), // 1s - fires for anything > 1ms
        ..Default::default()
    }
}

#[test]
fn cost_spike_fires_when_above_threshold() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    let hooks = vec![hook_cost_spike()];
    fire_cost_spike_notification(
        &conn,
        &cfg,
        &hooks,
        &CostSpikeArgs {
            run_id: "run-cost-1",
            workflow_name: "deploy",
            target_label: None,
            cost_usd: 9.0,
            multiple: 4.0,
            duration_ms: None,
            repo_slug: "myrepo",
            branch: "main",
            parent_workflow_run_id: None,
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-cost-1' AND event_type = 'workflow_run.cost_spike'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "cost spike notification should be claimed");
}

#[test]
fn cost_spike_deduped_on_second_call() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    let hooks = vec![hook_cost_spike()];
    for _ in 0..2 {
        fire_cost_spike_notification(
            &conn,
            &cfg,
            &hooks,
            &CostSpikeArgs {
                run_id: "run-cost-dup",
                workflow_name: "deploy",
                target_label: None,
                cost_usd: 9.0,
                multiple: 5.0,
                duration_ms: None,
                repo_slug: "myrepo",
                branch: "main",
                parent_workflow_run_id: None,
                repo_id: None,
                worktree_id: None,
            },
        );
    }
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-cost-dup' AND event_type = 'workflow_run.cost_spike'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "cost spike should be deduped");
}

#[test]
fn cost_spike_skipped_below_threshold_no_hooks() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    fire_cost_spike_notification(
        &conn,
        &cfg,
        &[],
        &CostSpikeArgs {
            run_id: "run-cost-low",
            workflow_name: "deploy",
            target_label: None,
            cost_usd: 1.5,
            multiple: 1.5,
            duration_ms: None,
            repo_slug: "myrepo",
            branch: "main",
            parent_workflow_run_id: None,
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-cost-low' AND event_type = 'workflow_run.cost_spike'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 0, "cost spike below threshold should not fire");
}

// ── fire_duration_spike_notification ─────────────────────────────────

#[test]
fn duration_spike_fires_when_above_threshold() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    let hooks = vec![hook_duration_spike()];
    fire_duration_spike_notification(
        &conn,
        &cfg,
        &hooks,
        &DurationSpikeArgs {
            run_id: "run-dur-1",
            workflow_name: "deploy",
            target_label: None,
            multiple: 3.0,
            duration_ms: Some(90_000),
            repo_slug: "myrepo",
            branch: "main",
            parent_workflow_run_id: None,
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-dur-1' AND event_type = 'workflow_run.duration_spike'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "duration spike notification should be claimed");
}

#[test]
fn duration_spike_skipped_below_threshold_no_hooks() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    fire_duration_spike_notification(
        &conn,
        &cfg,
        &[],
        &DurationSpikeArgs {
            run_id: "run-dur-low",
            workflow_name: "deploy",
            target_label: None,
            multiple: 1.5,
            duration_ms: Some(45_000),
            repo_slug: "myrepo",
            branch: "main",
            parent_workflow_run_id: None,
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-dur-low' AND event_type = 'workflow_run.duration_spike'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 0, "duration spike below threshold should not fire");
}

// ── fire_gate_pending_too_long_notification ───────────────────────────

#[test]
fn gate_pending_fires_when_above_threshold() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    let hooks = vec![hook_gate_pending()];
    fire_gate_pending_too_long_notification(
        &conn,
        &cfg,
        &hooks,
        &GatePendingTooLongArgs {
            step_id: "step-gate-1",
            step_name: "approval-gate",
            workflow_run_id: "run-gate-1",
            workflow_name: "deploy",
            target_label: None,
            pending_ms: 2_000_000, // ~33 min > 1s hook threshold
            duration_ms: None,
            repo_slug: "myrepo",
            branch: "main",
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-gate-1' AND event_type = 'gate.pending_too_long'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(
        count, 1,
        "gate pending too long notification should be claimed"
    );
}

#[test]
fn gate_pending_skipped_below_threshold_no_hooks() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    fire_gate_pending_too_long_notification(
        &conn,
        &cfg,
        &[],
        &GatePendingTooLongArgs {
            step_id: "step-gate-short",
            step_name: "approval-gate",
            workflow_run_id: "run-gate-short",
            workflow_name: "deploy",
            target_label: None,
            pending_ms: 60_000, // 1 min < 30 min default
            duration_ms: None,
            repo_slug: "myrepo",
            branch: "main",
            repo_id: None,
            worktree_id: None,
        },
    );
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-gate-short' AND event_type = 'gate.pending_too_long'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 0, "gate pending below threshold should not fire");
}

#[test]
fn gate_pending_deduped_on_second_call() {
    let conn = in_memory_db();
    let cfg = config_no_legacy();
    let hooks = vec![hook_gate_pending()];
    for _ in 0..2 {
        fire_gate_pending_too_long_notification(
            &conn,
            &cfg,
            &hooks,
            &GatePendingTooLongArgs {
                step_id: "step-gate-dup",
                step_name: "approval-gate",
                workflow_run_id: "run-gate-dup",
                workflow_name: "deploy",
                target_label: None,
                pending_ms: 2_000_000,
                duration_ms: None,
                repo_slug: "myrepo",
                branch: "main",
                repo_id: None,
                worktree_id: None,
            },
        );
    }
    let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'step-gate-dup' AND event_type = 'gate.pending_too_long'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 1, "gate pending too long should be deduped");
}
