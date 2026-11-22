use runkon_notify::{Event, Severity};

use crate::error::{ConductorError, Result};

use super::catalog::{self, EVENTS};
use super::{anomalies, gates, runs};

/// Build a synthetic test [`Event`] for the given concrete event name.
///
/// Returns `Err` if `name` is not a recognized event name.
pub fn build_synthetic_event(name: &str, now: impl Into<String>) -> Result<Event> {
    let now = now.into();
    let run_id = "test-00000000000000000000000000".to_string();
    let url = "http://localhost".to_string();
    let ticket_url = "https://github.com/example-org/example-repo/issues/42".to_string();

    let mut ev = match name {
        catalog::WORKFLOW_RUN_COMPLETED => Event {
            kind: name.into(),
            title: "Conductor \u{2014} Workflow Completed".into(),
            body: "test-workflow on test-repo/main".into(),
            severity: Severity::Info,
            fields: [
                ("run_id".into(), run_id),
                ("workflow_name".into(), "test-workflow".into()),
                ("parent_workflow_run_id".into(), String::new()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now.clone()),
                ("is_root".into(), "true".into()),
            ]
            .into_iter()
            .collect(),
        },
        catalog::WORKFLOW_RUN_FAILED => Event {
            kind: name.into(),
            title: "Conductor \u{2014} Workflow Failed".into(),
            body: "test-workflow on test-repo/main".into(),
            severity: Severity::Error,
            fields: [
                ("run_id".into(), run_id),
                ("workflow_name".into(), "test-workflow".into()),
                ("parent_workflow_run_id".into(), String::new()),
                ("repo_slug".into(), "test-repo".into()),
                ("branch".into(), "main".into()),
                ("duration_ms".into(), "1000".into()),
                ("ticket_url".into(), ticket_url),
                ("url".into(), url),
                ("timestamp".into(), now.clone()),
                ("is_root".into(), "true".into()),
                ("error".into(), "Test error".into()),
            ]
            .into_iter()
            .collect(),
        },
        // The remaining events go through the production builders, so a test notification
        // carries exactly the fields a real one does.
        catalog::WORKFLOW_RUN_ORPHAN_RESUMED => anomalies::orphan_resumed_event(
            &run_id,
            1,
            "test-workflow".into(),
            Some("test-repo/main"),
        ),
        catalog::WORKFLOW_RUN_REAPED => anomalies::reaped_event(
            &run_id,
            "test-workflow",
            Some("test-repo/main"),
            "Test error",
        ),
        catalog::WORKFLOW_RUN_COST_SPIKE => anomalies::cost_spike_event(
            None,
            &anomalies::CostSpikeArgs {
                run_id: &run_id,
                workflow_name: "test-workflow",
                target_label: Some("test-repo/main"),
                cost_usd: 4.2,
                multiple: 3.5,
                duration_ms: Some(1000),
                repo_slug: "test-repo",
                branch: "main",
                parent_workflow_run_id: None,
                repo_id: None,
                worktree_id: None,
            },
        ),
        catalog::WORKFLOW_RUN_DURATION_SPIKE => anomalies::duration_spike_event(
            None,
            &anomalies::DurationSpikeArgs {
                run_id: &run_id,
                workflow_name: "test-workflow",
                target_label: Some("test-repo/main"),
                multiple: 3.5,
                duration_ms: Some(1000),
                repo_slug: "test-repo",
                branch: "main",
                parent_workflow_run_id: None,
                repo_id: None,
                worktree_id: None,
            },
        ),
        catalog::AGENT_RUN_COMPLETED | catalog::AGENT_RUN_FAILED => {
            let failed = name == catalog::AGENT_RUN_FAILED;
            runs::agent_run_event(&runs::AgentRunNotificationArgs {
                run_id: &run_id,
                worktree_slug: Some("Test Agent Run"),
                succeeded: !failed,
                error_msg: failed.then_some("Test error"),
                repo_slug: "test-repo",
                branch: "main",
                duration_ms: Some(1000),
                ticket_url: Some(ticket_url),
            })
        }
        catalog::GATE_WAITING => gates::gate_event(&gates::GateNotificationParams {
            step_id: &run_id,
            step_name: "test-gate",
            workflow_name: "Test Run",
            target_label: None,
            gate_type: None,
            gate_prompt: None,
            repo_slug: "test-repo",
            branch: "main",
            ticket_url: Some(ticket_url),
        }),
        catalog::GATE_PENDING_TOO_LONG => anomalies::gate_pending_too_long_event(
            None,
            &anomalies::GatePendingTooLongArgs {
                step_id: "test-step",
                step_name: "test-gate",
                workflow_run_id: &run_id,
                workflow_name: "test-workflow",
                target_label: Some("test-repo/main"),
                pending_ms: 1_800_000,
                duration_ms: Some(1000),
                repo_slug: "test-repo",
                branch: "main",
                repo_id: None,
                worktree_id: None,
            },
        ),
        catalog::FEEDBACK_REQUESTED => {
            // Share the production event definition so the test notification cannot drift.
            runs::feedback_event(&runs::FeedbackNotificationParams {
                request_id: "01TESTFEEDBACK000000000000",
                run_id: &run_id,
                prompt_preview: "Is this correct?",
                repo_slug: "test-repo",
                branch: "main",
                worktree_slug: Some("test-worktree"),
            })
        }
        catalog::SESSION_IDLE => {
            // Share the production event definition so the test notification cannot drift.
            super::session_idle::session_idle_event(&super::session_idle::IdleSession {
                run_id: run_id.clone(),
                session_id: Some("test-session".into()),
                idle_since: now.clone(),
                worktree_id: Some("test-worktree-id".into()),
                worktree_slug: Some("test-worktree".into()),
                repo_slug: "test-repo".into(),
                branch: "main".into(),
                prompt: "Fix the flaky test".into(),
                last_message: Some("Done. The test now passes.".into()),
                background_tasks: Some(0),
                turn_duration_ms: Some(1000),
            })
        }
        other => {
            return Err(ConductorError::InvalidInput(format!(
                "unknown event name: '{other}'. Valid events: {}",
                catalog::event_names_csv()
            )))
        }
    };
    // Builders stamp Utc::now(); honor the injected clock.
    ev.fields.insert("timestamp".into(), now);
    Ok(ev)
}

/// Build a synthetic test event that will pass through a hook with the given `on` pattern.
///
/// Picks the first concrete event name that the pattern matches, falling back to
/// `workflow_run.completed` for `"*"` or any unrecognized pattern.
pub fn build_synthetic_for_pattern(pattern: &str, now: impl Into<String>) -> Event {
    let now = now.into();
    for spec in EVENTS {
        if runkon_notify::hooks::on_pattern_matches(pattern, spec.name) {
            return build_synthetic_event(spec.name, &now)
                .expect("every EVENTS entry must match a build_synthetic_event() arm");
        }
    }
    build_synthetic_event("workflow_run.completed", now)
        .expect("workflow_run.completed is always a valid event name")
}
