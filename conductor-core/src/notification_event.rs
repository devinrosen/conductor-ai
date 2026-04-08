use std::collections::HashMap;

use serde_json::{json, Value};

/// A lifecycle event that can be dispatched to user-configured notification hooks.
///
/// Each variant corresponds to one RFC 011 event name. Cost/duration spike variants
/// are defined here for completeness but are not yet wired to a detection mechanism
/// (follow-on analytics PR, RFC step 9).
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    /// A workflow run finished successfully.
    WorkflowRunCompleted {
        run_id: String,
        /// Human-readable label, e.g. `"ticket-to-pr on repo/branch"`.
        label: String,
        /// ISO 8601 timestamp.
        timestamp: String,
        /// Optional deep link URL (empty in non-web contexts).
        url: Option<String>,
    },
    /// A workflow run finished with a failure.
    WorkflowRunFailed {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
    },
    /// A workflow run's cost exceeded the configured multiple over baseline.
    /// Not yet wired — defined for schema completeness.
    WorkflowRunCostSpike {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// How many times over baseline this run cost.
        multiple: f64,
    },
    /// A workflow run's duration exceeded the configured multiple over baseline.
    /// Not yet wired — defined for schema completeness.
    WorkflowRunDurationSpike {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        multiple: f64,
    },
    /// A standalone agent run finished successfully.
    AgentRunCompleted {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
    },
    /// A standalone agent run finished with a failure.
    AgentRunFailed {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// Optional error message from the agent run.
        error: Option<String>,
    },
    /// A workflow gate is waiting for external action.
    GateWaiting {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        step_name: String,
    },
    /// A workflow gate has been waiting longer than the configured threshold.
    /// Not yet wired — follow-on PR.
    GatePendingTooLong {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        step_name: String,
        pending_ms: u64,
    },
    /// An agent run is waiting for human feedback input.
    FeedbackRequested {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        prompt_preview: String,
    },
}

impl NotificationEvent {
    /// Returns the dotted event name string used for glob matching in hook configs.
    ///
    /// Examples: `"workflow_run.completed"`, `"gate.waiting"`.
    pub fn event_name(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { .. } => "workflow_run.completed",
            Self::WorkflowRunFailed { .. } => "workflow_run.failed",
            Self::WorkflowRunCostSpike { .. } => "workflow_run.cost_spike",
            Self::WorkflowRunDurationSpike { .. } => "workflow_run.duration_spike",
            Self::AgentRunCompleted { .. } => "agent_run.completed",
            Self::AgentRunFailed { .. } => "agent_run.failed",
            Self::GateWaiting { .. } => "gate.waiting",
            Self::GatePendingTooLong { .. } => "gate.pending_too_long",
            Self::FeedbackRequested { .. } => "feedback.requested",
        }
    }

    /// Returns environment variables to inject into shell hooks.
    ///
    /// All keys are `CONDUCTOR_`-prefixed. Common fields are always present;
    /// event-specific fields are added per variant.
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();

        // Common fields
        map.insert("CONDUCTOR_EVENT".into(), self.event_name().into());
        map.insert("CONDUCTOR_RUN_ID".into(), self.run_id().into());
        map.insert("CONDUCTOR_LABEL".into(), self.label().into());
        map.insert("CONDUCTOR_TIMESTAMP".into(), self.timestamp().into());
        map.insert(
            "CONDUCTOR_URL".into(),
            self.url().map(|u| u.as_str()).unwrap_or("").into(),
        );

        // Event-specific fields
        match self {
            Self::WorkflowRunCostSpike { multiple, .. }
            | Self::WorkflowRunDurationSpike { multiple, .. } => {
                map.insert("CONDUCTOR_MULTIPLE".into(), multiple.to_string());
            }
            Self::AgentRunFailed { error, .. } => {
                map.insert(
                    "CONDUCTOR_ERROR".into(),
                    error.as_deref().unwrap_or("").into(),
                );
            }
            Self::GateWaiting { step_name, .. } => {
                map.insert("CONDUCTOR_STEP_NAME".into(), step_name.clone());
            }
            Self::GatePendingTooLong {
                step_name,
                pending_ms,
                ..
            } => {
                map.insert("CONDUCTOR_STEP_NAME".into(), step_name.clone());
                map.insert("CONDUCTOR_PENDING_MS".into(), pending_ms.to_string());
            }
            Self::FeedbackRequested { prompt_preview, .. } => {
                map.insert("CONDUCTOR_PROMPT_PREVIEW".into(), prompt_preview.clone());
            }
            _ => {}
        }

        map
    }

    /// Returns a JSON object payload for HTTP hooks.
    ///
    /// Common fields (`event`, `run_id`, `label`, `timestamp`, `url`) are always
    /// present. `url` is omitted when `None`. Event-specific fields are merged in.
    pub fn to_json(&self) -> Value {
        let mut obj = json!({
            "event": self.event_name(),
            "run_id": self.run_id(),
            "label": self.label(),
            "timestamp": self.timestamp(),
        });

        if let Some(u) = self.url() {
            obj["url"] = Value::String(u.clone());
        }

        match self {
            Self::WorkflowRunCostSpike { multiple, .. }
            | Self::WorkflowRunDurationSpike { multiple, .. } => {
                obj["multiple"] = json!(multiple);
            }
            Self::AgentRunFailed { error: Some(e), .. } => {
                obj["error"] = Value::String(e.clone());
            }
            Self::GateWaiting { step_name, .. } => {
                obj["step_name"] = Value::String(step_name.clone());
            }
            Self::GatePendingTooLong {
                step_name,
                pending_ms,
                ..
            } => {
                obj["step_name"] = Value::String(step_name.clone());
                obj["pending_ms"] = json!(pending_ms);
            }
            Self::FeedbackRequested { prompt_preview, .. } => {
                obj["prompt_preview"] = Value::String(prompt_preview.clone());
            }
            _ => {}
        }

        obj
    }

    // ── Private field accessors shared by to_env_vars / to_json ──────────

    fn run_id(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { run_id, .. }
            | Self::WorkflowRunFailed { run_id, .. }
            | Self::WorkflowRunCostSpike { run_id, .. }
            | Self::WorkflowRunDurationSpike { run_id, .. }
            | Self::AgentRunCompleted { run_id, .. }
            | Self::AgentRunFailed { run_id, .. }
            | Self::GateWaiting { run_id, .. }
            | Self::GatePendingTooLong { run_id, .. }
            | Self::FeedbackRequested { run_id, .. } => run_id,
        }
    }

    pub(crate) fn label(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { label, .. }
            | Self::WorkflowRunFailed { label, .. }
            | Self::WorkflowRunCostSpike { label, .. }
            | Self::WorkflowRunDurationSpike { label, .. }
            | Self::AgentRunCompleted { label, .. }
            | Self::AgentRunFailed { label, .. }
            | Self::GateWaiting { label, .. }
            | Self::GatePendingTooLong { label, .. }
            | Self::FeedbackRequested { label, .. } => label,
        }
    }

    fn timestamp(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { timestamp, .. }
            | Self::WorkflowRunFailed { timestamp, .. }
            | Self::WorkflowRunCostSpike { timestamp, .. }
            | Self::WorkflowRunDurationSpike { timestamp, .. }
            | Self::AgentRunCompleted { timestamp, .. }
            | Self::AgentRunFailed { timestamp, .. }
            | Self::GateWaiting { timestamp, .. }
            | Self::GatePendingTooLong { timestamp, .. }
            | Self::FeedbackRequested { timestamp, .. } => timestamp,
        }
    }

    fn url(&self) -> Option<&String> {
        match self {
            Self::WorkflowRunCompleted { url, .. }
            | Self::WorkflowRunFailed { url, .. }
            | Self::WorkflowRunCostSpike { url, .. }
            | Self::WorkflowRunDurationSpike { url, .. }
            | Self::AgentRunCompleted { url, .. }
            | Self::AgentRunFailed { url, .. }
            | Self::GateWaiting { url, .. }
            | Self::GatePendingTooLong { url, .. }
            | Self::FeedbackRequested { url, .. } => url.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_name_variants() {
        assert_eq!(
            NotificationEvent::WorkflowRunCompleted {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
            }
            .event_name(),
            "workflow_run.completed"
        );
        assert_eq!(
            NotificationEvent::WorkflowRunFailed {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
            }
            .event_name(),
            "workflow_run.failed"
        );
        assert_eq!(
            NotificationEvent::AgentRunCompleted {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
            }
            .event_name(),
            "agent_run.completed"
        );
        assert_eq!(
            NotificationEvent::AgentRunFailed {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
                error: None,
            }
            .event_name(),
            "agent_run.failed"
        );
        assert_eq!(
            NotificationEvent::GateWaiting {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
                step_name: "s".into(),
            }
            .event_name(),
            "gate.waiting"
        );
        assert_eq!(
            NotificationEvent::FeedbackRequested {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
                prompt_preview: "p".into(),
            }
            .event_name(),
            "feedback.requested"
        );
    }

    #[test]
    fn to_env_vars_common_fields() {
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "run-123".into(),
            label: "my-wf on main".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            url: Some("https://example.com".into()),
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_EVENT"], "workflow_run.completed");
        assert_eq!(vars["CONDUCTOR_RUN_ID"], "run-123");
        assert_eq!(vars["CONDUCTOR_LABEL"], "my-wf on main");
        assert_eq!(vars["CONDUCTOR_TIMESTAMP"], "2024-01-01T00:00:00Z");
        assert_eq!(vars["CONDUCTOR_URL"], "https://example.com");
    }

    #[test]
    fn to_env_vars_url_none_is_empty_string() {
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_URL"], "");
    }

    #[test]
    fn to_env_vars_agent_run_failed_includes_error() {
        let event = NotificationEvent::AgentRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            error: Some("timeout".into()),
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_ERROR"], "timeout");
    }

    #[test]
    fn to_env_vars_agent_run_failed_error_none_is_empty_string() {
        let event = NotificationEvent::AgentRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            error: None,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_ERROR"], "");
        assert_eq!(vars["CONDUCTOR_EVENT"], "agent_run.failed");
    }

    #[test]
    fn to_env_vars_gate_waiting_includes_step_name() {
        let event = NotificationEvent::GateWaiting {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "human-review".into(),
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_STEP_NAME"], "human-review");
    }

    #[test]
    fn to_env_vars_feedback_requested_includes_preview() {
        let event = NotificationEvent::FeedbackRequested {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            prompt_preview: "Is this right?".into(),
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_PROMPT_PREVIEW"], "Is this right?");
    }

    #[test]
    fn to_json_common_fields() {
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "run-999".into(),
            label: "wf on branch".into(),
            timestamp: "2024-06-01T12:00:00Z".into(),
            url: Some("https://localhost:3000".into()),
        };
        let v = event.to_json();
        assert_eq!(v["event"], "workflow_run.completed");
        assert_eq!(v["run_id"], "run-999");
        assert_eq!(v["label"], "wf on branch");
        assert_eq!(v["url"], "https://localhost:3000");
    }

    #[test]
    fn to_json_omits_url_when_none() {
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
        };
        let v = event.to_json();
        assert!(v.get("url").is_none());
    }

    #[test]
    fn to_json_gate_waiting_includes_step_name() {
        let event = NotificationEvent::GateWaiting {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "approve".into(),
        };
        let v = event.to_json();
        assert_eq!(v["step_name"], "approve");
    }

    #[test]
    fn event_name_spike_and_pending_variants() {
        assert_eq!(
            NotificationEvent::WorkflowRunCostSpike {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
                multiple: 3.0,
            }
            .event_name(),
            "workflow_run.cost_spike"
        );
        assert_eq!(
            NotificationEvent::WorkflowRunDurationSpike {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
                multiple: 2.5,
            }
            .event_name(),
            "workflow_run.duration_spike"
        );
        assert_eq!(
            NotificationEvent::GatePendingTooLong {
                run_id: "r".into(),
                label: "l".into(),
                timestamp: "t".into(),
                url: None,
                step_name: "s".into(),
                pending_ms: 60_000,
            }
            .event_name(),
            "gate.pending_too_long"
        );
    }

    #[test]
    fn to_env_vars_cost_spike_includes_multiple() {
        let event = NotificationEvent::WorkflowRunCostSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 4.2,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_EVENT"], "workflow_run.cost_spike");
        assert_eq!(vars["CONDUCTOR_MULTIPLE"], "4.2");
    }

    #[test]
    fn to_env_vars_duration_spike_includes_multiple() {
        let event = NotificationEvent::WorkflowRunDurationSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 1.5,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_EVENT"], "workflow_run.duration_spike");
        assert_eq!(vars["CONDUCTOR_MULTIPLE"], "1.5");
    }

    #[test]
    fn to_env_vars_gate_pending_too_long_includes_step_and_ms() {
        let event = NotificationEvent::GatePendingTooLong {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "review".into(),
            pending_ms: 90_000,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_EVENT"], "gate.pending_too_long");
        assert_eq!(vars["CONDUCTOR_STEP_NAME"], "review");
        assert_eq!(vars["CONDUCTOR_PENDING_MS"], "90000");
    }

    #[test]
    fn to_json_cost_spike_includes_multiple() {
        let event = NotificationEvent::WorkflowRunCostSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 3.0,
        };
        let v = event.to_json();
        assert_eq!(v["event"], "workflow_run.cost_spike");
        assert!((v["multiple"].as_f64().unwrap() - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn to_json_duration_spike_includes_multiple() {
        let event = NotificationEvent::WorkflowRunDurationSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 2.5,
        };
        let v = event.to_json();
        assert_eq!(v["event"], "workflow_run.duration_spike");
        assert!((v["multiple"].as_f64().unwrap() - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn to_json_gate_pending_too_long_includes_step_and_ms() {
        let event = NotificationEvent::GatePendingTooLong {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "review".into(),
            pending_ms: 90_000,
        };
        let v = event.to_json();
        assert_eq!(v["event"], "gate.pending_too_long");
        assert_eq!(v["step_name"], "review");
        assert_eq!(v["pending_ms"], 90_000u64);
    }

    #[test]
    fn to_json_feedback_requested_includes_preview() {
        let event = NotificationEvent::FeedbackRequested {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            prompt_preview: "Is this right?".into(),
        };
        let v = event.to_json();
        assert_eq!(v["event"], "feedback.requested");
        assert_eq!(v["prompt_preview"], "Is this right?");
        assert!(v.get("url").is_none());
    }

    #[test]
    fn to_json_feedback_requested_with_url() {
        let event = NotificationEvent::FeedbackRequested {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: Some("https://example.com".into()),
            prompt_preview: "confirm?".into(),
        };
        let v = event.to_json();
        assert_eq!(v["url"], "https://example.com");
        assert_eq!(v["prompt_preview"], "confirm?");
    }

    #[test]
    fn to_json_agent_run_failed_includes_error() {
        let event = NotificationEvent::AgentRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            error: Some("timeout".into()),
        };
        let v = event.to_json();
        assert_eq!(v["event"], "agent_run.failed");
        assert_eq!(v["error"], "timeout");
    }

    #[test]
    fn to_json_agent_run_failed_no_error() {
        let event = NotificationEvent::AgentRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            error: None,
        };
        let v = event.to_json();
        assert_eq!(v["event"], "agent_run.failed");
        assert!(v.get("error").is_none());
    }
}
