use anyhow::{anyhow, Result};
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
            let notification_event =
                NotificationEvent::synthetic(&event, now).map_err(|e| anyhow!("{e}"))?;

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

#[cfg(test)]
mod tests {
    use conductor_core::notification_event::NotificationEvent;

    #[test]
    fn synthetic_all_valid_event_names() {
        let names = [
            "workflow_run.completed",
            "workflow_run.failed",
            "agent_run.completed",
            "agent_run.failed",
            "gate.waiting",
            "feedback.requested",
        ];
        for name in names {
            let result = NotificationEvent::synthetic(name, "2024-01-01T00:00:00Z");
            assert!(result.is_ok(), "expected Ok for '{name}'");
            assert_eq!(result.unwrap().event_name(), name);
        }
    }

    #[test]
    fn synthetic_unknown_event_name_returns_err() {
        let result = NotificationEvent::synthetic("bad.event", "t");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bad.event"));
    }
}
