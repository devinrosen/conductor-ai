use anyhow::{anyhow, Result};
use chrono::Utc;

use conductor_core::config::{hooks_as_runkon, Config};
use conductor_core::notify::catalog::{events_json, events_text};
use conductor_core::notify::{build_synthetic_event, HookRunner};

use crate::commands::NotificationsCommands;

pub fn handle_notifications(command: NotificationsCommands, config: &Config) -> Result<()> {
    match command {
        NotificationsCommands::Events { json } => {
            print!("{}", render_events(json)?);
            Ok(())
        }
        NotificationsCommands::Test { event } => {
            let hooks = &config.notify.hooks;
            if hooks.is_empty() {
                println!("No hooks configured in ~/.conductor/config.toml");
                println!("See docs/examples/hooks/ for example scripts and config snippets.");
                return Ok(());
            }

            let now = Utc::now().to_rfc3339();
            let event_obj = build_synthetic_event(&event, now).map_err(|e| anyhow!("{e}"))?;

            let runner = HookRunner::new(&hooks_as_runkon(hooks));
            runner.fire(&event_obj);

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

/// The `notify events` output: the registry as text, or as pretty JSON.
fn render_events(json: bool) -> Result<String> {
    if json {
        Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&events_json())?
        ))
    } else {
        Ok(events_text())
    }
}

#[cfg(test)]
mod tests {
    use super::render_events;
    use conductor_core::notify::{build_synthetic_event, EVENTS};

    #[test]
    fn events_json_lists_every_event_with_env_vars() {
        let v: serde_json::Value = serde_json::from_str(&render_events(true).unwrap()).unwrap();
        let names: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, EVENTS.iter().map(|e| e.name).collect::<Vec<_>>());
        let gate = v
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "gate.waiting")
            .unwrap();
        assert!(gate["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["env"] == "RUNKON_NOTIFY_FIELD_STEP_NAME"));
    }

    #[test]
    fn events_text_lists_every_event() {
        let text = render_events(false).unwrap();
        for e in EVENTS {
            assert!(text.contains(e.name));
        }
    }

    #[test]
    fn synthetic_all_valid_event_names() {
        let names = EVENTS.iter().map(|e| e.name);
        for name in names {
            let result = build_synthetic_event(name, "2024-01-01T00:00:00Z");
            assert!(result.is_ok(), "expected Ok for '{name}'");
            assert_eq!(result.unwrap().kind, name);
        }
    }

    #[test]
    fn synthetic_unknown_event_name_returns_err() {
        let result = build_synthetic_event("bad.event", "t");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bad.event"));
    }
}
