use std::collections::HashMap;

use serde_json::{json, Value};

use crate::error::{ConductorError, Result};

/// All non-threshold lifecycle event names with their display labels and whether
/// they are workflow events (supporting the `:root` modifier).
///
/// This is the authoritative list used by `GET /api/config/hooks/events` to populate
/// the hook × event matrix UI. Threshold-based events (`workflow_run.cost_spike`,
/// `workflow_run.duration_spike`, `gate.pending_too_long`) are excluded because they
/// require additional filter fields that cannot be represented as simple checkboxes.
///
/// Tuple: `(event_name, display_label, is_workflow_event)`.
pub const ALL_EVENTS: &[(&str, &str, bool)] = &[
    ("workflow_run.completed", "Workflow completed", true),
    ("workflow_run.failed", "Workflow failed", true),
    ("workflow_run.stale", "Workflow step stale", true),
    ("workflow_run.reaped", "Dead workflow detected", true),
    (
        "workflow_run.orphan_resumed",
        "Orphaned workflows resumed",
        true,
    ),
    ("agent_run.completed", "Agent completed", false),
    ("agent_run.failed", "Agent failed", false),
    ("gate.waiting", "Gate waiting", false),
    ("feedback.requested", "Feedback requested", false),
];

/// All concrete event names that `NotificationEvent::synthetic` accepts.
///
/// This is the single source of truth shared by both `synthetic()` (for its error
/// message) and `synthetic_for_pattern()` (for candidate selection). Adding a new
/// variant requires updating this list *and* the `synthetic()` match arms together.
const VALID_SYNTHETIC_EVENTS: &[&str] = &[
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
        /// Raw workflow name, e.g. `"deploy-staging"`.
        workflow_name: String,
        /// ID of the parent workflow run when this is a sub-workflow; `None` for root runs.
        parent_workflow_run_id: Option<String>,
        /// Repository slug (e.g. `"conductor-ai"`).
        repo_slug: String,
        /// Branch name (e.g. `"main"`).
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
    /// A workflow run finished with a failure.
    WorkflowRunFailed {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// Raw workflow name, e.g. `"deploy-staging"`.
        workflow_name: String,
        /// ID of the parent workflow run when this is a sub-workflow; `None` for root runs.
        parent_workflow_run_id: Option<String>,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
        /// Optional error message from the failed run.
        error: Option<String>,
    },
    /// An orphaned workflow run was auto-resumed by the heartbeat watchdog.
    WorkflowRunOrphanResumed {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// Raw workflow name, e.g. `"deploy-staging"`.
        workflow_name: String,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
    /// A stuck workflow run failed to auto-resume after being reaped by the watchdog.
    WorkflowRunReaped {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// Raw workflow name, e.g. `"deploy-staging"`.
        workflow_name: String,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
        /// Error message from the failed auto-resume attempt.
        error: Option<String>,
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
        /// Raw workflow name, e.g. `"deploy-staging"`.
        workflow_name: String,
        /// ID of the parent workflow run when this is a sub-workflow; `None` for root runs.
        parent_workflow_run_id: Option<String>,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
        /// Cost in USD for this run.
        cost_usd: Option<f64>,
    },
    /// A workflow run's duration exceeded the configured multiple over baseline.
    /// Not yet wired — defined for schema completeness.
    WorkflowRunDurationSpike {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        multiple: f64,
        /// Raw workflow name, e.g. `"deploy-staging"`.
        workflow_name: String,
        /// ID of the parent workflow run when this is a sub-workflow; `None` for root runs.
        parent_workflow_run_id: Option<String>,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
    /// A standalone agent run finished successfully.
    AgentRunCompleted {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
    /// A standalone agent run finished with a failure.
    AgentRunFailed {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        /// Optional error message from the agent run.
        error: Option<String>,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
    /// A workflow gate is waiting for external action.
    GateWaiting {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        step_name: String,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
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
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
    /// An agent run is waiting for human feedback input.
    FeedbackRequested {
        run_id: String,
        label: String,
        timestamp: String,
        url: Option<String>,
        prompt_preview: String,
        /// Repository slug.
        repo_slug: String,
        /// Branch name.
        branch: String,
        /// Run duration in milliseconds.
        duration_ms: Option<u64>,
        /// Optional ticket URL.
        ticket_url: Option<String>,
    },
}

impl NotificationEvent {
    /// Creates a synthetic test event for the given concrete event name.
    ///
    /// Used by CLI `notifications test` and the web API `POST /api/config/hooks/test` so that
    /// the factory logic lives in one place rather than being duplicated across binary crates.
    ///
    /// Returns `Err` if `name` is not a recognized event name.
    pub fn synthetic(name: &str, now: impl Into<String>) -> Result<Self> {
        let now = now.into();
        let run_id = "test-00000000000000000000000000".to_string();
        let url = Some("http://localhost".to_string());
        let ticket_url = Some("https://github.com/example-org/example-repo/issues/42".to_string());
        let ev = match name {
            "workflow_run.completed" => Self::WorkflowRunCompleted {
                run_id,
                label: "Test Run".to_string(),
                timestamp: now,
                url,
                workflow_name: "test-workflow".to_string(),
                parent_workflow_run_id: None,
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
            },
            "workflow_run.failed" => Self::WorkflowRunFailed {
                run_id,
                label: "Test Run".to_string(),
                timestamp: now,
                url,
                workflow_name: "test-workflow".to_string(),
                parent_workflow_run_id: None,
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
                error: Some("Test error".to_string()),
            },
            "agent_run.completed" => Self::AgentRunCompleted {
                run_id,
                label: "Test Agent Run".to_string(),
                timestamp: now,
                url,
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
            },
            "agent_run.failed" => Self::AgentRunFailed {
                run_id,
                label: "Test Agent Run".to_string(),
                timestamp: now,
                url,
                error: Some("Test error".to_string()),
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
            },
            "gate.waiting" => Self::GateWaiting {
                run_id,
                label: "Test Run".to_string(),
                timestamp: now,
                url,
                step_name: "test-gate".to_string(),
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
            },
            "feedback.requested" => Self::FeedbackRequested {
                run_id,
                label: "Test Agent Run".to_string(),
                timestamp: now,
                url,
                prompt_preview: "Is this correct?".to_string(),
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
            },
            "workflow_run.orphan_resumed" => Self::WorkflowRunOrphanResumed {
                run_id,
                label: "Test Run".to_string(),
                timestamp: now,
                url,
                workflow_name: "test-workflow".to_string(),
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
            },
            "workflow_run.reaped" => Self::WorkflowRunReaped {
                run_id,
                label: "Test Run".to_string(),
                timestamp: now,
                url,
                workflow_name: "test-workflow".to_string(),
                repo_slug: "test-repo".to_string(),
                branch: "main".to_string(),
                duration_ms: Some(1000),
                ticket_url,
                error: Some("Test error".to_string()),
            },
            other => {
                return Err(ConductorError::InvalidInput(format!(
                    "unknown event name: '{other}'. Valid events: {}",
                    VALID_SYNTHETIC_EVENTS.join(", ")
                )))
            }
        };
        Ok(ev)
    }

    /// Creates a synthetic test event that will pass through a hook with the given `on` pattern.
    ///
    /// Picks the first concrete event name that the pattern matches, falling back to
    /// `workflow_run.completed` for `"*"` or any unrecognized pattern. Used by the web
    /// API `POST /api/config/hooks/test` to ensure the test event actually reaches the hook.
    pub fn synthetic_for_pattern(pattern: &str, now: impl Into<String>) -> Self {
        let now = now.into();
        for &name in VALID_SYNTHETIC_EVENTS {
            if crate::notification_hooks::on_pattern_matches(pattern, name) {
                // VALID_SYNTHETIC_EVENTS entries are kept in sync with synthetic()'s
                // match arms, so this can never fail.
                return Self::synthetic(name, &now)
                    .expect("VALID_SYNTHETIC_EVENTS entry must match a synthetic() arm");
            }
        }
        // Fallback for "*" or any unrecognized pattern.
        Self::synthetic("workflow_run.completed", now)
            .expect("workflow_run.completed is always a valid synthetic event name")
    }

    /// Returns the dotted event name string used for glob matching in hook configs.
    ///
    /// Examples: `"workflow_run.completed"`, `"gate.waiting"`.
    pub fn event_name(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { .. } => "workflow_run.completed",
            Self::WorkflowRunFailed { .. } => "workflow_run.failed",
            Self::WorkflowRunOrphanResumed { .. } => "workflow_run.orphan_resumed",
            Self::WorkflowRunReaped { .. } => "workflow_run.reaped",
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
        map.insert("CONDUCTOR_REPO_SLUG".into(), self.repo_slug().into());
        map.insert("CONDUCTOR_BRANCH".into(), self.branch().into());
        map.insert(
            "CONDUCTOR_DURATION_MS".into(),
            self.duration_ms_value()
                .map(|ms| ms.to_string())
                .unwrap_or_default(),
        );
        map.insert(
            "CONDUCTOR_TICKET_URL".into(),
            self.ticket_url_value().unwrap_or("").into(),
        );

        // Pass 1: workflow hierarchy fields shared by all four workflow-run variants
        match self {
            Self::WorkflowRunCompleted {
                workflow_name,
                parent_workflow_run_id,
                ..
            }
            | Self::WorkflowRunFailed {
                workflow_name,
                parent_workflow_run_id,
                ..
            }
            | Self::WorkflowRunCostSpike {
                workflow_name,
                parent_workflow_run_id,
                ..
            }
            | Self::WorkflowRunDurationSpike {
                workflow_name,
                parent_workflow_run_id,
                ..
            } => {
                map.insert("CONDUCTOR_WORKFLOW_NAME".into(), workflow_name.clone());
                map.insert(
                    "CONDUCTOR_PARENT_WORKFLOW_RUN_ID".into(),
                    parent_workflow_run_id.as_deref().unwrap_or("").into(),
                );
            }
            _ => {}
        }

        // Pass 2: spike-specific and remaining event-specific fields
        match self {
            Self::WorkflowRunCostSpike {
                multiple, cost_usd, ..
            } => {
                map.insert("CONDUCTOR_MULTIPLE".into(), multiple.to_string());
                if let Some(cost) = cost_usd {
                    map.insert("CONDUCTOR_COST_USD".into(), cost.to_string());
                }
            }
            Self::WorkflowRunDurationSpike { multiple, .. } => {
                map.insert("CONDUCTOR_MULTIPLE".into(), multiple.to_string());
            }
            Self::WorkflowRunFailed { error, .. } => {
                map.insert(
                    "CONDUCTOR_ERROR".into(),
                    error.as_deref().unwrap_or("").into(),
                );
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
            Self::WorkflowRunOrphanResumed { workflow_name, .. } => {
                map.insert("CONDUCTOR_WORKFLOW_NAME".into(), workflow_name.clone());
            }
            Self::WorkflowRunReaped {
                workflow_name,
                error,
                ..
            } => {
                map.insert("CONDUCTOR_WORKFLOW_NAME".into(), workflow_name.clone());
                map.insert(
                    "CONDUCTOR_ERROR".into(),
                    error.as_deref().unwrap_or("").into(),
                );
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
            "repo_slug": self.repo_slug(),
            "branch": self.branch(),
        });

        if let Some(u) = self.url() {
            obj["url"] = Value::String(u.clone());
        }

        if let Some(ms) = self.duration_ms_value() {
            obj["duration_ms"] = json!(ms);
        }

        if let Some(ticket_url) = self.ticket_url_value() {
            obj["ticket_url"] = Value::String(ticket_url.to_string());
        }

        // Pass 1: workflow hierarchy fields shared by all four workflow-run variants
        match self {
            Self::WorkflowRunCompleted {
                workflow_name,
                parent_workflow_run_id,
                ..
            }
            | Self::WorkflowRunFailed {
                workflow_name,
                parent_workflow_run_id,
                ..
            }
            | Self::WorkflowRunCostSpike {
                workflow_name,
                parent_workflow_run_id,
                ..
            }
            | Self::WorkflowRunDurationSpike {
                workflow_name,
                parent_workflow_run_id,
                ..
            } => {
                obj["workflow_name"] = Value::String(workflow_name.clone());
                if let Some(parent_id) = parent_workflow_run_id {
                    obj["parent_workflow_run_id"] = Value::String(parent_id.clone());
                }
            }
            _ => {}
        }

        // Pass 2: spike-specific and remaining event-specific fields
        match self {
            Self::WorkflowRunCostSpike {
                multiple, cost_usd, ..
            } => {
                obj["multiple"] = json!(multiple);
                if let Some(cost) = cost_usd {
                    obj["cost_usd"] = json!(cost);
                }
            }
            Self::WorkflowRunDurationSpike { multiple, .. } => {
                obj["multiple"] = json!(multiple);
            }
            Self::WorkflowRunFailed { error: Some(e), .. } => {
                obj["error"] = Value::String(e.clone());
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
            Self::WorkflowRunOrphanResumed { workflow_name, .. } => {
                obj["workflow_name"] = Value::String(workflow_name.clone());
            }
            Self::WorkflowRunReaped {
                workflow_name,
                error,
                ..
            } => {
                obj["workflow_name"] = Value::String(workflow_name.clone());
                if let Some(e) = error {
                    obj["error"] = Value::String(e.clone());
                }
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
            | Self::WorkflowRunOrphanResumed { run_id, .. }
            | Self::WorkflowRunReaped { run_id, .. }
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
            | Self::WorkflowRunOrphanResumed { label, .. }
            | Self::WorkflowRunReaped { label, .. }
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
            | Self::WorkflowRunOrphanResumed { timestamp, .. }
            | Self::WorkflowRunReaped { timestamp, .. }
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
            | Self::WorkflowRunOrphanResumed { url, .. }
            | Self::WorkflowRunReaped { url, .. }
            | Self::WorkflowRunCostSpike { url, .. }
            | Self::WorkflowRunDurationSpike { url, .. }
            | Self::AgentRunCompleted { url, .. }
            | Self::AgentRunFailed { url, .. }
            | Self::GateWaiting { url, .. }
            | Self::GatePendingTooLong { url, .. }
            | Self::FeedbackRequested { url, .. } => url.as_ref(),
        }
    }

    pub(crate) fn repo_slug(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { repo_slug, .. }
            | Self::WorkflowRunFailed { repo_slug, .. }
            | Self::WorkflowRunOrphanResumed { repo_slug, .. }
            | Self::WorkflowRunReaped { repo_slug, .. }
            | Self::WorkflowRunCostSpike { repo_slug, .. }
            | Self::WorkflowRunDurationSpike { repo_slug, .. }
            | Self::AgentRunCompleted { repo_slug, .. }
            | Self::AgentRunFailed { repo_slug, .. }
            | Self::GateWaiting { repo_slug, .. }
            | Self::GatePendingTooLong { repo_slug, .. }
            | Self::FeedbackRequested { repo_slug, .. } => repo_slug,
        }
    }

    pub(crate) fn branch(&self) -> &str {
        match self {
            Self::WorkflowRunCompleted { branch, .. }
            | Self::WorkflowRunFailed { branch, .. }
            | Self::WorkflowRunOrphanResumed { branch, .. }
            | Self::WorkflowRunReaped { branch, .. }
            | Self::WorkflowRunCostSpike { branch, .. }
            | Self::WorkflowRunDurationSpike { branch, .. }
            | Self::AgentRunCompleted { branch, .. }
            | Self::AgentRunFailed { branch, .. }
            | Self::GateWaiting { branch, .. }
            | Self::GatePendingTooLong { branch, .. }
            | Self::FeedbackRequested { branch, .. } => branch,
        }
    }

    fn duration_ms_value(&self) -> Option<u64> {
        match self {
            Self::WorkflowRunCompleted { duration_ms, .. }
            | Self::WorkflowRunFailed { duration_ms, .. }
            | Self::WorkflowRunOrphanResumed { duration_ms, .. }
            | Self::WorkflowRunReaped { duration_ms, .. }
            | Self::WorkflowRunCostSpike { duration_ms, .. }
            | Self::WorkflowRunDurationSpike { duration_ms, .. }
            | Self::AgentRunCompleted { duration_ms, .. }
            | Self::AgentRunFailed { duration_ms, .. }
            | Self::GateWaiting { duration_ms, .. }
            | Self::GatePendingTooLong { duration_ms, .. }
            | Self::FeedbackRequested { duration_ms, .. } => *duration_ms,
        }
    }

    fn ticket_url_value(&self) -> Option<&str> {
        match self {
            Self::WorkflowRunCompleted { ticket_url, .. }
            | Self::WorkflowRunFailed { ticket_url, .. }
            | Self::WorkflowRunOrphanResumed { ticket_url, .. }
            | Self::WorkflowRunReaped { ticket_url, .. }
            | Self::WorkflowRunCostSpike { ticket_url, .. }
            | Self::WorkflowRunDurationSpike { ticket_url, .. }
            | Self::AgentRunCompleted { ticket_url, .. }
            | Self::AgentRunFailed { ticket_url, .. }
            | Self::GateWaiting { ticket_url, .. }
            | Self::GatePendingTooLong { ticket_url, .. }
            | Self::FeedbackRequested { ticket_url, .. } => ticket_url.as_deref(),
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
                workflow_name: "wf".into(),
                parent_workflow_run_id: None,
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
                workflow_name: "wf".into(),
                parent_workflow_run_id: None,
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
                error: None,
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
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
            workflow_name: "my-wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "my-repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            error: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_PROMPT_PREVIEW"], "Is this right?");
    }

    #[test]
    fn to_env_vars_repo_slug_and_branch() {
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "my-repo".into(),
            branch: "feature/foo".into(),
            duration_ms: Some(5000),
            ticket_url: Some("https://jira.example.com/ticket/1".into()),
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_REPO_SLUG"], "my-repo");
        assert_eq!(vars["CONDUCTOR_BRANCH"], "feature/foo");
        assert_eq!(vars["CONDUCTOR_DURATION_MS"], "5000");
        assert_eq!(
            vars["CONDUCTOR_TICKET_URL"],
            "https://jira.example.com/ticket/1"
        );
    }

    #[test]
    fn to_env_vars_duration_ms_none_is_empty() {
        let event = NotificationEvent::AgentRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_DURATION_MS"], "");
        assert_eq!(vars["CONDUCTOR_TICKET_URL"], "");
    }

    #[test]
    fn to_env_vars_workflow_run_failed_includes_error() {
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            error: Some("step failed: build error".into()),
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_ERROR"], "step failed: build error");
    }

    #[test]
    fn to_json_common_fields() {
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "run-999".into(),
            label: "wf on branch".into(),
            timestamp: "2024-06-01T12:00:00Z".into(),
            url: Some("https://localhost:3000".into()),
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "my-repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let v = event.to_json();
        assert_eq!(v["event"], "workflow_run.completed");
        assert_eq!(v["run_id"], "run-999");
        assert_eq!(v["label"], "wf on branch");
        assert_eq!(v["url"], "https://localhost:3000");
        assert_eq!(v["repo_slug"], "my-repo");
        assert_eq!(v["branch"], "main");
    }

    #[test]
    fn to_json_omits_url_when_none() {
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            error: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
                workflow_name: "wf".into(),
                parent_workflow_run_id: None,
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
                cost_usd: None,
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
                workflow_name: "wf".into(),
                parent_workflow_run_id: None,
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
                repo_slug: "repo".into(),
                branch: "main".into(),
                duration_ms: None,
                ticket_url: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            cost_usd: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            cost_usd: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let v = event.to_json();
        assert_eq!(v["event"], "agent_run.failed");
        assert!(v.get("error").is_none());
    }

    #[test]
    fn to_env_vars_workflow_run_completed_includes_hierarchy_root() {
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "deploy-staging on main".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "deploy-staging".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_WORKFLOW_NAME"], "deploy-staging");
        assert_eq!(vars["CONDUCTOR_PARENT_WORKFLOW_RUN_ID"], "");
    }

    #[test]
    fn to_env_vars_workflow_run_failed_includes_hierarchy_sub() {
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "child-wf".into(),
            parent_workflow_run_id: Some("parent-run-id".into()),
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            error: None,
        };
        let vars = event.to_env_vars();
        assert_eq!(vars["CONDUCTOR_WORKFLOW_NAME"], "child-wf");
        assert_eq!(vars["CONDUCTOR_PARENT_WORKFLOW_RUN_ID"], "parent-run-id");
    }

    #[test]
    fn to_json_workflow_run_completed_omits_parent_when_none() {
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "my-wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let v = event.to_json();
        assert_eq!(v["workflow_name"], "my-wf");
        assert!(v.get("parent_workflow_run_id").is_none());
    }

    #[test]
    fn to_json_workflow_run_failed_includes_parent_when_some() {
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "child-wf".into(),
            parent_workflow_run_id: Some("parent-run-id".into()),
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            error: None,
        };
        let v = event.to_json();
        assert_eq!(v["workflow_name"], "child-wf");
        assert_eq!(v["parent_workflow_run_id"], "parent-run-id");
    }

    // ── synthetic / synthetic_for_pattern ────────────────────────────────

    #[test]
    fn synthetic_known_event_names_succeed() {
        let names = [
            "workflow_run.completed",
            "workflow_run.failed",
            "agent_run.completed",
            "agent_run.failed",
            "gate.waiting",
            "feedback.requested",
        ];
        for name in names {
            let ev = NotificationEvent::synthetic(name, "t").unwrap();
            assert_eq!(ev.event_name(), name, "event_name mismatch for {name}");
        }
    }

    #[test]
    fn synthetic_unknown_event_name_returns_err() {
        let result = NotificationEvent::synthetic("does_not_exist", "t");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("does_not_exist"),
            "error message should include bad name"
        );
    }

    #[test]
    fn synthetic_for_pattern_star_returns_workflow_run_completed() {
        let ev = NotificationEvent::synthetic_for_pattern("*", "t");
        assert_eq!(ev.event_name(), "workflow_run.completed");
    }

    #[test]
    fn synthetic_for_pattern_workflow_prefix() {
        let ev = NotificationEvent::synthetic_for_pattern("workflow_run.*", "t");
        assert_eq!(ev.event_name(), "workflow_run.completed");
    }

    #[test]
    fn synthetic_for_pattern_agent_prefix() {
        let ev = NotificationEvent::synthetic_for_pattern("agent_run.*", "t");
        assert_eq!(ev.event_name(), "agent_run.completed");
    }

    #[test]
    fn synthetic_for_pattern_exact_gate_waiting() {
        let ev = NotificationEvent::synthetic_for_pattern("gate.waiting", "t");
        assert_eq!(ev.event_name(), "gate.waiting");
    }

    #[test]
    fn synthetic_for_pattern_exact_feedback_requested() {
        let ev = NotificationEvent::synthetic_for_pattern("feedback.requested", "t");
        assert_eq!(ev.event_name(), "feedback.requested");
    }

    #[test]
    fn synthetic_for_pattern_unrecognized_falls_back_to_workflow_completed() {
        let ev = NotificationEvent::synthetic_for_pattern("unrecognized.event", "t");
        assert_eq!(ev.event_name(), "workflow_run.completed");
    }

    #[test]
    fn synthetic_workflow_run_failed_has_error() {
        let ev = NotificationEvent::synthetic("workflow_run.failed", "t").unwrap();
        match ev {
            NotificationEvent::WorkflowRunFailed { error, .. } => {
                assert!(
                    error.is_some(),
                    "synthetic workflow_run.failed should have a non-None error"
                );
            }
            _ => panic!("expected WorkflowRunFailed"),
        }
    }

    #[test]
    fn synthetic_events_have_ticket_url() {
        let names = [
            "workflow_run.completed",
            "workflow_run.failed",
            "agent_run.completed",
            "agent_run.failed",
            "gate.waiting",
            "feedback.requested",
        ];
        for name in names {
            let ev = NotificationEvent::synthetic(name, "t").unwrap();
            let ticket_url = match &ev {
                NotificationEvent::WorkflowRunCompleted { ticket_url, .. } => ticket_url,
                NotificationEvent::WorkflowRunFailed { ticket_url, .. } => ticket_url,
                NotificationEvent::AgentRunCompleted { ticket_url, .. } => ticket_url,
                NotificationEvent::AgentRunFailed { ticket_url, .. } => ticket_url,
                NotificationEvent::GateWaiting { ticket_url, .. } => ticket_url,
                NotificationEvent::FeedbackRequested { ticket_url, .. } => ticket_url,
                _ => panic!("unexpected variant for {name}"),
            };
            assert!(
                ticket_url.is_some(),
                "synthetic {name} should have a non-None ticket_url"
            );
        }
    }
}
