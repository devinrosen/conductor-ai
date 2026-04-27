use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::dsl::GateType;
use crate::status::{WorkflowRunStatus, WorkflowStepStatus};

/// A step key is a `(name, iteration)` pair used for skip-set and step-map lookups.
#[allow(dead_code)]
pub(crate) type StepKey = (String, u32);

/// Describes what a workflow run is currently blocked on when in `Waiting` status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockedOn {
    HumanApproval {
        gate_name: String,
        prompt: Option<String>,
        #[serde(default)]
        options: Vec<String>,
    },
    HumanReview {
        gate_name: String,
        prompt: Option<String>,
        #[serde(default)]
        options: Vec<String>,
    },
    PrApproval {
        gate_name: String,
        approvals_needed: u32,
    },
    PrChecks {
        gate_name: String,
    },
}

/// A workflow run record from the database.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_name: String,
    pub worktree_id: Option<String>,
    pub parent_run_id: String,
    pub status: WorkflowRunStatus,
    pub dry_run: bool,
    pub trigger: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub definition_snapshot: Option<String>,
    pub inputs: HashMap<String, String>,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    pub parent_workflow_run_id: Option<String>,
    pub target_label: Option<String>,
    pub default_bot_name: Option<String>,
    pub iteration: i64,
    pub blocked_on: Option<BlockedOn>,
    pub workflow_title: Option<String>,
    pub total_input_tokens: Option<i64>,
    pub total_output_tokens: Option<i64>,
    pub total_cache_read_input_tokens: Option<i64>,
    pub total_cache_creation_input_tokens: Option<i64>,
    pub total_turns: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<i64>,
    pub model: Option<String>,
    pub dismissed: bool,
}

/// Extract the human-readable title from a workflow definition snapshot JSON string.
pub fn extract_workflow_title(snapshot: Option<&str>) -> Option<String> {
    let s = snapshot?;
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => v["title"].as_str().map(String::from),
        Err(e) => {
            tracing::warn!(
                "Malformed definition_snapshot JSON — could not extract workflow title: {e}"
            );
            None
        }
    }
}

impl WorkflowRun {
    /// Whether this run was triggered by a workflow hook (prevents infinite chains).
    pub fn is_triggered_by_hook(&self) -> bool {
        self.trigger == "hook"
    }

    /// Returns the human-readable display name for this run.
    pub fn display_name(&self) -> &str {
        self.workflow_title
            .as_deref()
            .unwrap_or(&self.workflow_name)
    }
}

/// A workflow step execution record from the database.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkflowRunStep {
    pub id: String,
    pub workflow_run_id: String,
    pub step_name: String,
    pub role: String,
    pub can_commit: bool,
    pub condition_expr: Option<String>,
    pub status: WorkflowStepStatus,
    pub child_run_id: Option<String>,
    pub position: i64,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub result_text: Option<String>,
    pub condition_met: Option<bool>,
    pub iteration: i64,
    pub parallel_group_id: Option<String>,
    pub context_out: Option<String>,
    pub markers_out: Option<String>,
    pub retry_count: i64,
    pub gate_type: Option<GateType>,
    pub gate_prompt: Option<String>,
    pub gate_timeout: Option<String>,
    pub gate_approved_by: Option<String>,
    pub gate_approved_at: Option<String>,
    pub gate_feedback: Option<String>,
    pub structured_output: Option<String>,
    pub output_file: Option<String>,
    pub gate_options: Option<String>,
    pub gate_selections: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    /// Cost of the child agent run, populated from the persistence layer.
    pub cost_usd: Option<f64>,
    /// Turn count from the child agent run, populated from the persistence layer.
    pub num_turns: Option<i64>,
    /// Wall-clock duration of the child agent run in ms, populated from the persistence layer.
    pub duration_ms: Option<i64>,
    pub fan_out_total: Option<i64>,
    pub fan_out_completed: i64,
    pub fan_out_failed: i64,
    pub fan_out_skipped: i64,
    pub step_error: Option<String>,
}

/// Lightweight summary of the currently-running step for a workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStepSummary {
    pub step_name: String,
    pub iteration: i64,
    pub workflow_chain: Vec<String>,
}

/// Configuration for workflow execution.
#[derive(Debug, Clone)]
pub struct WorkflowExecConfig {
    pub poll_interval: Duration,
    pub step_timeout: Duration,
    pub fail_fast: bool,
    pub dry_run: bool,
    pub shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for WorkflowExecConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            step_timeout: Duration::from_secs(12 * 60 * 60),
            fail_fast: true,
            dry_run: false,
            shutdown: None,
        }
    }
}

/// Result of executing a workflow.
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub workflow_run_id: String,
    pub worktree_id: Option<String>,
    pub workflow_name: String,
    pub all_succeeded: bool,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_input_tokens: i64,
    pub total_cache_creation_input_tokens: i64,
}

/// Input describing a successfully completed step, passed to `record_step_success`.
///
/// Groups the step output data that previously made call sites unwieldy.
/// Does not include `step_key` — that is an execution bookkeeping concern kept
/// as a separate parameter. `iteration` is included because it is needed to
/// populate `ContextEntry`.
#[derive(Debug, Clone, Default)]
pub struct StepSuccess {
    pub step_name: String,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub markers: Vec<String>,
    pub context: String,
    pub child_run_id: Option<String>,
    pub iteration: u32,
    pub structured_output: Option<String>,
    pub output_file: Option<String>,
}

/// Result of a single step execution (kept in memory during execution).
#[derive(Debug, Clone, Default)]
pub struct StepResult {
    pub step_name: String,
    pub status: WorkflowStepStatus,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub markers: Vec<String>,
    pub context: String,
    pub child_run_id: Option<String>,
    pub structured_output: Option<String>,
    pub output_file: Option<String>,
}

impl StepResult {
    /// Create a failed StepResult with the given error text.
    pub fn failed(step_name: &str, result_text: String) -> Self {
        Self {
            step_name: step_name.to_string(),
            status: WorkflowStepStatus::Failed,
            result_text: Some(result_text),
            ..Self::default()
        }
    }

    /// Create a skipped StepResult.
    pub fn skipped(step_name: &str) -> Self {
        Self {
            step_name: step_name.to_string(),
            status: WorkflowStepStatus::Skipped,
            ..Self::default()
        }
    }

    /// Create a completed StepResult without per-step metrics.
    ///
    /// Convenience wrapper for the common case where cost/turns/duration are
    /// not available (e.g. restored from a prior run or bubble-up from a child
    /// workflow). Metric fields on `success` are ignored.
    pub fn completed_without_metrics(success: &StepSuccess) -> Self {
        let mut s = Self::completed(success);
        s.cost_usd = None;
        s.num_turns = None;
        s.duration_ms = None;
        s
    }

    /// Create a completed StepResult from a [`StepSuccess`] description.
    pub fn completed(success: &StepSuccess) -> Self {
        Self {
            step_name: success.step_name.clone(),
            status: WorkflowStepStatus::Completed,
            result_text: success.result_text.clone(),
            cost_usd: success.cost_usd,
            num_turns: success.num_turns,
            duration_ms: success.duration_ms,
            markers: success.markers.clone(),
            context: success.context.clone(),
            child_run_id: success.child_run_id.clone(),
            structured_output: success.structured_output.clone(),
            output_file: success.output_file.clone(),
        }
    }
}

/// An entry in the accumulated context history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub step: String,
    pub iteration: u32,
    pub context: String,
    #[serde(default)]
    pub markers: Vec<String>,
    #[serde(default)]
    pub structured_output: Option<String>,
    #[serde(default)]
    pub output_file: Option<String>,
}

impl From<StepSuccess> for ContextEntry {
    fn from(success: StepSuccess) -> Self {
        Self {
            step: success.step_name,
            iteration: success.iteration,
            context: success.context,
            markers: success.markers,
            structured_output: success.structured_output,
            output_file: success.output_file,
        }
    }
}

/// A single row in the `workflow_run_step_fan_out_items` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanOutItemRow {
    pub id: String,
    pub step_run_id: String,
    pub item_type: String,
    pub item_id: String,
    pub item_ref: String,
    pub child_run_id: Option<String>,
    pub status: String,
    pub dispatched_at: Option<String>,
    pub completed_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{StepResult, StepSuccess};
    use crate::status::WorkflowStepStatus;

    #[test]
    fn step_result_failed_sets_status_and_text() {
        let r = StepResult::failed("plan", "out of tokens".to_string());
        assert_eq!(r.step_name, "plan");
        assert_eq!(r.status, WorkflowStepStatus::Failed);
        assert_eq!(r.result_text, Some("out of tokens".to_string()));
        assert!(r.markers.is_empty());
        assert_eq!(r.context, "");
    }

    #[test]
    fn step_result_skipped_sets_status_and_defaults() {
        let r = StepResult::skipped("lint");
        assert_eq!(r.step_name, "lint");
        assert_eq!(r.status, WorkflowStepStatus::Skipped);
        assert!(r.result_text.is_none());
        assert!(r.markers.is_empty());
        assert_eq!(r.context, "");
    }

    #[test]
    fn step_result_completed_sets_all_fields() {
        let success = StepSuccess {
            step_name: "review".to_string(),
            result_text: Some("looks good".to_string()),
            cost_usd: Some(0.05),
            num_turns: Some(3),
            duration_ms: Some(1200),
            markers: vec!["approved".to_string()],
            context: "ctx".to_string(),
            child_run_id: Some("child-1".to_string()),
            structured_output: Some(r#"{"ok":true}"#.to_string()),
            output_file: Some("/tmp/out".to_string()),
            ..StepSuccess::default()
        };
        let r = StepResult::completed(&success);
        assert_eq!(r.step_name, "review");
        assert_eq!(r.status, WorkflowStepStatus::Completed);
        assert_eq!(r.result_text, Some("looks good".to_string()));
        assert_eq!(r.cost_usd, Some(0.05));
        assert_eq!(r.num_turns, Some(3));
        assert_eq!(r.duration_ms, Some(1200));
        assert_eq!(r.markers, vec!["approved"]);
        assert_eq!(r.context, "ctx");
        assert_eq!(r.child_run_id, Some("child-1".to_string()));
        assert_eq!(r.structured_output, Some(r#"{"ok":true}"#.to_string()));
        assert_eq!(r.output_file, Some("/tmp/out".to_string()));
    }

    #[test]
    fn completed_without_metrics_ignores_metric_fields() {
        let success = StepSuccess {
            step_name: "restore".to_string(),
            result_text: Some("ok".to_string()),
            cost_usd: Some(0.10),
            num_turns: Some(5),
            duration_ms: Some(3000),
            markers: vec!["done".to_string()],
            context: "restored".to_string(),
            ..StepSuccess::default()
        };
        let r = StepResult::completed_without_metrics(&success);
        assert_eq!(r.step_name, "restore");
        assert_eq!(r.status, WorkflowStepStatus::Completed);
        assert_eq!(r.result_text, Some("ok".to_string()));
        assert!(r.cost_usd.is_none(), "cost_usd should be None");
        assert!(r.num_turns.is_none(), "num_turns should be None");
        assert!(r.duration_ms.is_none(), "duration_ms should be None");
        assert_eq!(r.markers, vec!["done"]);
        assert_eq!(r.context, "restored");
    }

    #[test]
    fn step_success_into_context_entry_maps_all_fields() {
        let success = StepSuccess {
            step_name: "my-step".to_string(),
            iteration: 7,
            context: "ctx-body".to_string(),
            markers: vec!["m1".to_string(), "m2".to_string()],
            structured_output: Some(r#"{"k":"v"}"#.to_string()),
            output_file: Some("/tmp/out".to_string()),
            // Fields not mapped into ContextEntry should be distinct so we
            // would catch an accidental mapping.
            result_text: Some("rt".to_string()),
            cost_usd: Some(1.23),
            num_turns: Some(42),
            duration_ms: Some(999),
            input_tokens: Some(100),
            output_tokens: Some(200),
            cache_read_input_tokens: Some(50),
            cache_creation_input_tokens: Some(25),
            child_run_id: Some("child-1".to_string()),
        };
        let entry: super::ContextEntry = success.into();
        assert_eq!(entry.step, "my-step", "step should come from step_name");
        assert_eq!(entry.iteration, 7);
        assert_eq!(entry.context, "ctx-body");
        assert_eq!(entry.markers, vec!["m1", "m2"]);
        assert_eq!(entry.structured_output, Some(r#"{"k":"v"}"#.to_string()));
        assert_eq!(entry.output_file, Some("/tmp/out".to_string()));
    }
}
