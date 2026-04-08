use anyhow::{bail, Result};
use chrono::Utc;

use conductor_core::config::Config;
use conductor_core::notification_event::NotificationEvent;
use conductor_core::notification_hooks::HookRunner;

use crate::commands::NotificationsCommands;

pub fn handle_notifications(command: NotificationsCommands, config: &Config) -> Result<()> {
    match command {
        NotificationsCommands::Test { event } => {
            let hooks = &config.notify.hooks;
            if hooks.is_empty() {
                println!("No hooks configured in ~/.conductor/config.toml");
                println!("See docs/examples/hooks/ for example scripts and config snippets.");
                return Ok(());
            }

            let now = Utc::now().to_rfc3339();
            let notification_event = build_event(&event, now)?;

            let runner = HookRunner::new(hooks);
            runner.fire(&notification_event);

            println!(
                "Test event '{}' dispatched through {} configured hook(s).",
                event,
                hooks.len()
            );
            println!("Hooks fire asynchronously — check hook output/logs for results.");
            Ok(())
        }
    }
}

fn build_event(event: &str, now: String) -> Result<NotificationEvent> {
    let run_id = "test-00000000000000000000000000".to_string();
    let url = Some("http://localhost".to_string());

    let ev = match event {
        "workflow_run.completed" => NotificationEvent::WorkflowRunCompleted {
            run_id,
            label: "Test Run".to_string(),
            timestamp: now,
            url,
        },
        "workflow_run.failed" => NotificationEvent::WorkflowRunFailed {
            run_id,
            label: "Test Run".to_string(),
            timestamp: now,
            url,
        },
        "agent_run.completed" => NotificationEvent::AgentRunCompleted {
            run_id,
            label: "Test Agent Run".to_string(),
            timestamp: now,
            url,
        },
        "agent_run.failed" => NotificationEvent::AgentRunFailed {
            run_id,
            label: "Test Agent Run".to_string(),
            timestamp: now,
            url,
            error: Some("Test error".to_string()),
        },
        "gate.waiting" => NotificationEvent::GateWaiting {
            run_id,
            label: "Test Run".to_string(),
            timestamp: now,
            url,
            step_name: "test-gate".to_string(),
        },
        "feedback.requested" => NotificationEvent::FeedbackRequested {
            run_id,
            label: "Test Agent Run".to_string(),
            timestamp: now,
            url,
            prompt_preview: "Is this correct?".to_string(),
        },
        other => bail!(
            "unknown event name: '{other}'. Valid events: workflow_run.completed, \
             workflow_run.failed, agent_run.completed, agent_run.failed, \
             gate.waiting, feedback.requested"
        ),
    };
    Ok(ev)
}
