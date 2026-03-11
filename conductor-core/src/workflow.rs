//! Workflow engine: execute multi-step workflow definitions with conditional
//! branching, loops, parallel execution, gates, and actor/reviewer agent roles.
//!
//! Builds on top of the existing `AgentManager` and orchestrator infrastructure,
//! adding workflow-level tracking in `workflow_runs` / `workflow_run_steps`.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::thread;
use std::time::Duration;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentManager, AgentRunStatus};
use crate::agent_config::{self, AgentSpec};
use crate::agent_runtime;
use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::prompt_config;
use crate::workflow_dsl::{
    self, CallNode, CallWorkflowNode, DoNode, DoWhileNode, GateNode, GateType, IfNode, OnMaxIter,
    OnTimeout, ParallelNode, UnlessNode, WhileNode, WorkflowNode,
};

// Re-export DSL types so consumers go through `workflow::` instead of `workflow_dsl::` directly.
use crate::schema_config::{self, OutputSchema};
pub use crate::workflow_dsl::{
    collect_agent_names, collect_workflow_refs, detect_workflow_cycles,
    validate_workflow_semantics, AgentRef, InputDecl, ValidationError, ValidationReport,
    WorkflowDef, WorkflowTrigger, MAX_WORKFLOW_DEPTH,
};
use crate::worktree::WorktreeManager;

/// Convert a DSL `AgentRef` to the `agent_config` layer's `AgentSpec`.
///
/// This is the boundary where the workflow DSL concern (`AgentRef`) maps to
/// the resolution concern (`AgentSpec`).
impl From<&AgentRef> for AgentSpec {
    fn from(r: &AgentRef) -> Self {
        match r {
            AgentRef::Name(s) => AgentSpec::Name(s.clone()),
            AgentRef::Path(s) => AgentSpec::Path(s.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Column list for `workflow_run_steps` SELECT queries (used by `row_to_workflow_step`).
const STEP_COLUMNS: &str =
    "id, workflow_run_id, step_name, role, can_commit, condition_expr, status, \
     child_run_id, position, started_at, ended_at, result_text, condition_met, \
     iteration, parallel_group_id, context_out, markers_out, retry_count, \
     gate_type, gate_prompt, gate_timeout, gate_approved_by, gate_approved_at, gate_feedback, \
     structured_output";

/// Column list for `workflow_runs` SELECT queries (used by `row_to_workflow_run`).
const RUN_COLUMNS: &str =
    "id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
     started_at, ended_at, result_summary, definition_snapshot, inputs, ticket_id, repo_id";

/// Instruction appended to every agent prompt for structured output.
pub const CONDUCTOR_OUTPUT_INSTRUCTION: &str = r#"
When you have finished your work, output the following block exactly as the
last thing in your response. Do not include this block in code examples or
anywhere else — only as the final output.

<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": ""}
<<<END_CONDUCTOR_OUTPUT>>>

markers: array of string signals consumed by the workflow engine
         (e.g. ["has_review_issues", "has_critical_issues"])
context: one or two sentence summary of what you did or found,
         passed to the next step as {{prior_context}}
"#;

// ---------------------------------------------------------------------------
// Structured output parsing
// ---------------------------------------------------------------------------

/// Parsed output from `<<<CONDUCTOR_OUTPUT>>>` block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConductorOutput {
    #[serde(default)]
    pub markers: Vec<String>,
    #[serde(default)]
    pub context: String,
}

/// Parse the `<<<CONDUCTOR_OUTPUT>>>` block from agent result text.
/// Finds the *last* occurrence to avoid false positives in code blocks.
pub fn parse_conductor_output(text: &str) -> Option<ConductorOutput> {
    let start_marker = "<<<CONDUCTOR_OUTPUT>>>";
    let end_marker = "<<<END_CONDUCTOR_OUTPUT>>>";

    // Find the last occurrence
    let start = text.rfind(start_marker)?;
    let json_start = start + start_marker.len();
    let end = text[json_start..].find(end_marker)?;
    let json_str = text[json_start..json_start + end].trim();

    serde_json::from_str(json_str).ok()
}

/// Resolve a schema by name using the standard search order.
fn resolve_schema(state: &ExecutionState<'_>, name: &str) -> Result<OutputSchema> {
    let schema_ref = schema_config::SchemaRef::from_str_value(name);
    schema_config::load_schema(
        &state.working_dir,
        &state.repo_path,
        &schema_ref,
        Some(&state.workflow_name),
    )
}

/// Interpret agent output using a schema (if present) or generic `CONDUCTOR_OUTPUT` parsing.
///
/// Returns `(markers, context, structured_json)`. The `succeeded` flag controls whether
/// a schema validation failure is treated as an error (`Err`) or silently falls back.
fn interpret_agent_output(
    result_text: Option<&str>,
    schema: Option<&OutputSchema>,
    succeeded: bool,
) -> std::result::Result<(Vec<String>, String, Option<String>), String> {
    if let Some(s) = schema {
        match result_text.map(|text| schema_config::parse_structured_output(text, s)) {
            Some(Ok(structured)) => Ok((
                structured.markers,
                structured.context,
                Some(structured.json_string),
            )),
            Some(Err(e)) if succeeded => {
                // Structured output validation failed on a successful run — caller should retry
                Err(format!("structured output validation: {e}"))
            }
            _ => {
                // No output block found or parsing error on a failed run — fall back
                let fallback = result_text
                    .and_then(parse_conductor_output)
                    .unwrap_or_default();
                Ok((fallback.markers, fallback.context, None))
            }
        }
    } else {
        let output = result_text
            .and_then(parse_conductor_output)
            .unwrap_or_default();
        Ok((output.markers, output.context, None))
    }
}

// ---------------------------------------------------------------------------
// Context threading
// ---------------------------------------------------------------------------

/// An entry in the accumulated context history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub step: String,
    pub iteration: u32,
    pub context: String,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Status of a workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Waiting,
}

impl std::fmt::Display for WorkflowRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Waiting => "waiting",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for WorkflowRunStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "waiting" => Ok(Self::Waiting),
            _ => Err(format!("unknown WorkflowRunStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(WorkflowRunStatus);

/// Status of a single workflow step execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    Waiting,
    TimedOut,
}

impl std::fmt::Display for WorkflowStepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Waiting => "waiting",
            Self::TimedOut => "timed_out",
        };
        write!(f, "{s}")
    }
}

impl WorkflowStepStatus {
    /// Short display label used in summaries and status columns.
    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Completed => "ok",
            Self::Failed => "FAIL",
            Self::Skipped => "skip",
            Self::Running => "...",
            Self::Pending => "-",
            Self::Waiting => "wait",
            Self::TimedOut => "tout",
        }
    }
}

impl std::str::FromStr for WorkflowStepStatus {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            "waiting" => Ok(Self::Waiting),
            "timed_out" => Ok(Self::TimedOut),
            _ => Err(format!("unknown WorkflowStepStatus: {s}")),
        }
    }
}

crate::impl_sql_enum!(WorkflowStepStatus);

// ---------------------------------------------------------------------------
// Data structs
// ---------------------------------------------------------------------------

/// A workflow run record from the database.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_name: String,
    /// `None` for ephemeral PR runs that have no registered worktree.
    pub worktree_id: Option<String>,
    pub parent_run_id: String,
    pub status: WorkflowRunStatus,
    pub dry_run: bool,
    pub trigger: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub result_summary: Option<String>,
    pub definition_snapshot: Option<String>,
    pub inputs: HashMap<String, String>,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
}

/// A workflow step execution record from the database.
#[derive(Debug, Clone, Serialize)]
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
    pub gate_type: Option<String>,
    pub gate_prompt: Option<String>,
    pub gate_timeout: Option<String>,
    pub gate_approved_by: Option<String>,
    pub gate_approved_at: Option<String>,
    pub gate_feedback: Option<String>,
    /// Full structured output JSON (when schema was used).
    pub structured_output: Option<String>,
}

/// A single entry in a step's metadata, either a key-value field or a
/// multi-line section with a heading and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataEntry {
    /// Short key-value pair (e.g. "Status" → "completed").
    Field { label: &'static str, value: String },
    /// Longer block with a heading and free-form body text.
    Section { heading: &'static str, body: String },
}

impl WorkflowRunStep {
    /// Return structured metadata entries for this step.
    ///
    /// Consumers are responsible for choosing how to render the entries (e.g.
    /// fixed-width columns for a TUI, HTML table for a web UI, etc.).
    pub fn metadata_fields(&self) -> Vec<MetadataEntry> {
        let mut entries = vec![
            MetadataEntry::Field {
                label: "Status",
                value: self.status.to_string(),
            },
            MetadataEntry::Field {
                label: "Role",
                value: self.role.clone(),
            },
            MetadataEntry::Field {
                label: "Can commit",
                value: self.can_commit.to_string(),
            },
            MetadataEntry::Field {
                label: "Iteration",
                value: self.iteration.to_string(),
            },
        ];
        if let Some(ref started) = self.started_at {
            entries.push(MetadataEntry::Field {
                label: "Started",
                value: started.clone(),
            });
        }
        if let Some(ref ended) = self.ended_at {
            entries.push(MetadataEntry::Field {
                label: "Ended",
                value: ended.clone(),
            });
        }
        if let Some(ref gt) = self.gate_type {
            entries.push(MetadataEntry::Field {
                label: "Gate type",
                value: gt.clone(),
            });
        }
        if let Some(ref gp) = self.gate_prompt {
            entries.push(MetadataEntry::Section {
                heading: "Gate Prompt",
                body: gp.clone(),
            });
        }
        if let Some(ref gf) = self.gate_feedback {
            entries.push(MetadataEntry::Section {
                heading: "Gate Feedback",
                body: gf.clone(),
            });
        }
        if let Some(ref rt) = self.result_text {
            entries.push(MetadataEntry::Section {
                heading: "Result",
                body: rt.clone(),
            });
        }
        if let Some(ref ctx) = self.context_out {
            entries.push(MetadataEntry::Section {
                heading: "Context Out",
                body: ctx.clone(),
            });
        }
        if let Some(ref mk) = self.markers_out {
            entries.push(MetadataEntry::Section {
                heading: "Markers Out",
                body: mk.clone(),
            });
        }
        entries
    }
}

/// Configuration for workflow execution.
#[derive(Debug, Clone)]
pub struct WorkflowExecConfig {
    pub poll_interval: Duration,
    pub step_timeout: Duration,
    pub fail_fast: bool,
    pub dry_run: bool,
    /// Optional shutdown flag. When set to `true`, in-flight steps are
    /// cancelled with "workflow cancelled: TUI was closed".
    pub shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Default for WorkflowExecConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            step_timeout: Duration::from_secs(30 * 60),
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
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<String>,
    pub workflow_name: String,
    pub all_succeeded: bool,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
}

/// Result of a single step execution (kept in memory during execution).
#[derive(Debug, Clone)]
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
    /// Raw JSON string of structured output (when schema was used).
    pub structured_output: Option<String>,
}

// ---------------------------------------------------------------------------
// Manager (CRUD)
// ---------------------------------------------------------------------------

/// Manages workflow definitions, execution, and persistence.
pub struct WorkflowManager<'a> {
    conn: &'a Connection,
}

impl<'a> WorkflowManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_workflow_run(
        &self,
        workflow_name: &str,
        worktree_id: Option<&str>,
        parent_run_id: &str,
        dry_run: bool,
        trigger: &str,
        definition_snapshot: Option<&str>,
    ) -> Result<WorkflowRun> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, worktree_id, parent_run_id, status, \
             dry_run, trigger, started_at, definition_snapshot) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                workflow_name,
                worktree_id,
                parent_run_id,
                "pending",
                dry_run as i64,
                trigger,
                now,
                definition_snapshot,
            ],
        )?;

        Ok(WorkflowRun {
            id,
            workflow_name: workflow_name.to_string(),
            worktree_id: worktree_id.map(String::from),
            parent_run_id: parent_run_id.to_string(),
            status: WorkflowRunStatus::Pending,
            dry_run,
            trigger: trigger.to_string(),
            started_at: now,
            ended_at: None,
            result_summary: None,
            definition_snapshot: definition_snapshot.map(String::from),
            inputs: HashMap::new(),
            ticket_id: None,
            repo_id: None,
        })
    }

    /// Create a workflow run record with ticket and repo target IDs in a single INSERT.
    #[allow(clippy::too_many_arguments)]
    pub fn create_workflow_run_with_targets(
        &self,
        workflow_name: &str,
        worktree_id: Option<&str>,
        ticket_id: Option<&str>,
        repo_id: Option<&str>,
        parent_run_id: &str,
        dry_run: bool,
        trigger: &str,
        definition_snapshot: Option<&str>,
    ) -> Result<WorkflowRun> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, worktree_id, ticket_id, repo_id, \
             parent_run_id, status, dry_run, trigger, started_at, definition_snapshot) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                workflow_name,
                worktree_id,
                ticket_id,
                repo_id,
                parent_run_id,
                "pending",
                dry_run as i64,
                trigger,
                now,
                definition_snapshot,
            ],
        )?;

        Ok(WorkflowRun {
            id,
            workflow_name: workflow_name.to_string(),
            worktree_id: worktree_id.map(String::from),
            parent_run_id: parent_run_id.to_string(),
            status: WorkflowRunStatus::Pending,
            dry_run,
            trigger: trigger.to_string(),
            started_at: now,
            ended_at: None,
            result_summary: None,
            definition_snapshot: definition_snapshot.map(String::from),
            inputs: HashMap::new(),
            ticket_id: ticket_id.map(String::from),
            repo_id: repo_id.map(String::from),
        })
    }

    /// Persist the input variables for a workflow run.
    pub fn set_workflow_run_inputs(
        &self,
        run_id: &str,
        inputs: &HashMap<String, String>,
    ) -> Result<()> {
        let inputs_json = serde_json::to_string(inputs).map_err(|e| {
            ConductorError::Workflow(format!("Failed to serialize workflow inputs: {e}"))
        })?;
        self.conn.execute(
            "UPDATE workflow_runs SET inputs = ?1 WHERE id = ?2",
            params![inputs_json, run_id],
        )?;
        Ok(())
    }

    pub fn update_workflow_status(
        &self,
        workflow_run_id: &str,
        status: WorkflowRunStatus,
        result_summary: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let is_terminal = matches!(
            status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        );
        let ended_at = if is_terminal {
            Some(now.as_str())
        } else {
            None
        };

        self.conn.execute(
            "UPDATE workflow_runs SET status = ?1, result_summary = ?2, ended_at = ?3 WHERE id = ?4",
            params![status, result_summary, ended_at, workflow_run_id],
        )?;
        Ok(())
    }

    /// Insert a workflow step record.
    pub fn insert_step(
        &self,
        workflow_run_id: &str,
        step_name: &str,
        role: &str,
        can_commit: bool,
        position: i64,
        iteration: i64,
    ) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        self.conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, can_commit, status, position, iteration) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                workflow_run_id,
                step_name,
                role,
                can_commit as i64,
                "pending",
                position,
                iteration,
            ],
        )?;
        Ok(id)
    }

    /// Update a step's status and associated fields.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_status(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        retry_count: Option<i64>,
    ) -> Result<()> {
        self.update_step_status_full(
            step_id,
            status,
            child_run_id,
            result_text,
            context_out,
            markers_out,
            retry_count,
            None,
        )
    }

    /// Update a step's status with all fields including structured_output.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_status_full(
        &self,
        step_id: &str,
        status: WorkflowStepStatus,
        child_run_id: Option<&str>,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        retry_count: Option<i64>,
        structured_output: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let is_starting = status == WorkflowStepStatus::Running;
        let is_terminal = matches!(
            status,
            WorkflowStepStatus::Completed
                | WorkflowStepStatus::Failed
                | WorkflowStepStatus::Skipped
                | WorkflowStepStatus::TimedOut
        );

        if is_starting {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1, child_run_id = ?2, started_at = ?3 \
                 WHERE id = ?4",
                params![status, child_run_id, now, step_id],
            )?;
        } else if is_terminal {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1, child_run_id = ?2, ended_at = ?3, \
                 result_text = ?4, context_out = ?5, markers_out = ?6, \
                 retry_count = COALESCE(?7, retry_count), structured_output = ?8 \
                 WHERE id = ?9",
                params![
                    status,
                    child_run_id,
                    now,
                    result_text,
                    context_out,
                    markers_out,
                    retry_count,
                    structured_output,
                    step_id,
                ],
            )?;
        } else {
            self.conn.execute(
                "UPDATE workflow_run_steps SET status = ?1 WHERE id = ?2",
                params![status, step_id],
            )?;
        }
        Ok(())
    }

    /// Update gate-specific columns on a step.
    pub fn set_step_gate_info(
        &self,
        step_id: &str,
        gate_type: &str,
        gate_prompt: Option<&str>,
        gate_timeout: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_type = ?1, gate_prompt = ?2, gate_timeout = ?3 \
             WHERE id = ?4",
            params![gate_type, gate_prompt, gate_timeout, step_id],
        )?;
        Ok(())
    }

    /// Set parallel_group_id on a step.
    pub fn set_step_parallel_group(&self, step_id: &str, group_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET parallel_group_id = ?1 WHERE id = ?2",
            params![group_id, step_id],
        )?;
        Ok(())
    }

    /// Approve a gate: set gate_approved_at, gate_approved_by, and optional feedback.
    pub fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_at = ?1, gate_approved_by = ?2, \
             gate_feedback = ?3, status = 'completed', ended_at = ?1 WHERE id = ?4",
            params![now, approved_by, feedback, step_id],
        )?;
        Ok(())
    }

    /// Reject a gate: set step to failed.
    pub fn reject_gate(&self, step_id: &str, rejected_by: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_steps SET gate_approved_by = ?1, status = 'failed', ended_at = ?2 \
             WHERE id = ?3",
            params![rejected_by, now, step_id],
        )?;
        Ok(())
    }

    pub fn get_workflow_run(&self, id: &str) -> Result<Option<WorkflowRun>> {
        let result = self.conn.query_row(
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
            params![id],
            row_to_workflow_run,
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_workflow_steps(&self, workflow_run_id: &str) -> Result<Vec<WorkflowRunStep>> {
        query_collect(
            self.conn,
            &format!("SELECT {STEP_COLUMNS} FROM workflow_run_steps WHERE workflow_run_id = ?1 ORDER BY position"),
            params![workflow_run_id],
            row_to_workflow_step,
        )
    }

    pub fn get_step_by_id(&self, step_id: &str) -> Result<Option<WorkflowRunStep>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {STEP_COLUMNS} FROM workflow_run_steps WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(params![step_id], row_to_workflow_step)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Return the first active (pending/running/waiting) top-level workflow run for a worktree,
    /// or `None` if none exist.
    pub fn get_active_run_for_worktree(&self, worktree_id: &str) -> Result<Option<WorkflowRun>> {
        let result = self.conn.query_row(
            &format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs \
                 WHERE worktree_id = ?1 AND status IN ('pending', 'running', 'waiting') \
                 LIMIT 1"
            ),
            params![worktree_id],
            row_to_workflow_run,
        );
        match result {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_workflow_runs(&self, worktree_id: &str) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE worktree_id = ?1 ORDER BY started_at DESC"),
            params![worktree_id],
            row_to_workflow_run,
        )
    }

    /// List recent workflow runs across all worktrees, ordered by started_at DESC.
    pub fn list_all_workflow_runs(&self, limit: usize) -> Result<Vec<WorkflowRun>> {
        query_collect(
            self.conn,
            &format!(
                "SELECT {RUN_COLUMNS} FROM workflow_runs ORDER BY started_at DESC LIMIT {limit}"
            ),
            params![],
            row_to_workflow_run,
        )
    }

    /// Load runs for a single worktree, or the most recent `global_limit` runs across all
    /// worktrees when `worktree_id` is `None`. Consolidates the scoped-vs-global branching
    /// that would otherwise be duplicated at every call site.
    pub fn list_workflow_runs_for_scope(
        &self,
        worktree_id: Option<&str>,
        global_limit: usize,
    ) -> Result<Vec<WorkflowRun>> {
        match worktree_id {
            Some(wt_id) => self.list_workflow_runs(wt_id),
            None => self.list_all_workflow_runs(global_limit),
        }
    }

    /// Recover steps stuck in `running` status whose child agent run has
    /// already reached a terminal state (completed, failed, or cancelled).
    ///
    /// This handles the case where the executor was killed before the workflow
    /// thread could write the step's final status back to the DB.
    /// Returns the number of steps recovered.
    pub fn recover_stuck_steps(&self) -> Result<usize> {
        // Single JOIN query: avoids N+1 per-step lookups and skips the
        // per-run plan-step fetch that AgentManager::get_run() would do.
        let stuck: Vec<(String, String, String, Option<String>)> = query_collect(
            self.conn,
            "SELECT wrs.id, ar.id, ar.status, ar.result_text \
             FROM workflow_run_steps wrs \
             JOIN agent_runs ar ON ar.id = wrs.child_run_id \
             WHERE wrs.status = 'running' \
               AND ar.status IN ('completed', 'failed', 'cancelled')",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

        let mut recovered = 0usize;

        for (step_id, child_run_id, ar_status, result_text) in stuck {
            let step_status = match ar_status.as_str() {
                "completed" => WorkflowStepStatus::Completed,
                _ => WorkflowStepStatus::Failed,
            };

            self.update_step_status_full(
                &step_id,
                step_status,
                Some(&child_run_id),
                result_text.as_deref(),
                None,
                None,
                None,
                None,
            )?;
            recovered += 1;
        }

        Ok(recovered)
    }

    /// Find the waiting gate step for a workflow run.
    pub fn find_waiting_gate(&self, workflow_run_id: &str) -> Result<Option<WorkflowRunStep>> {
        let result = self.conn.query_row(
            &format!(
                "SELECT {STEP_COLUMNS} FROM workflow_run_steps \
                 WHERE workflow_run_id = ?1 AND gate_type IS NOT NULL AND gate_approved_at IS NULL \
                   AND status IN ('running', 'waiting') \
                 ORDER BY position DESC LIMIT 1"
            ),
            params![workflow_run_id],
            row_to_workflow_step,
        );
        match result {
            Ok(step) => Ok(Some(step)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load workflow definitions from the filesystem for a worktree.
    ///
    /// Wraps `workflow_dsl::load_workflow_defs` so consumers don't need to
    /// reach into the low-level DSL module directly.
    pub fn list_defs(worktree_path: &str, repo_path: &str) -> Result<Vec<WorkflowDef>> {
        workflow_dsl::load_workflow_defs(worktree_path, repo_path)
    }

    /// Load a single workflow definition by name.
    pub fn load_def_by_name(
        worktree_path: &str,
        repo_path: &str,
        name: &str,
    ) -> Result<WorkflowDef> {
        workflow_dsl::load_workflow_by_name(worktree_path, repo_path, name)
    }

    const SQL_RESET_FAILED: &'static str = "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
         context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL \
         WHERE workflow_run_id = ?1 AND status IN ('failed', 'running', 'timed_out')";

    const SQL_RESET_COMPLETED: &'static str = "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
         context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL \
         WHERE workflow_run_id = ?1 AND status = 'completed'";

    const SQL_RESET_FROM_POS: &'static str = "UPDATE workflow_run_steps \
         SET status = 'pending', started_at = NULL, ended_at = NULL, result_text = NULL, \
         context_out = NULL, markers_out = NULL, structured_output = NULL, child_run_id = NULL \
         WHERE workflow_run_id = ?1 AND position >= ?2";

    /// Reset all non-completed steps for a workflow run back to `pending`.
    ///
    /// Used before resuming so that failed/running/timed_out steps get re-executed.
    pub fn reset_failed_steps(&self, workflow_run_id: &str) -> Result<u64> {
        let count = self
            .conn
            .execute(Self::SQL_RESET_FAILED, params![workflow_run_id])?;
        Ok(count as u64)
    }

    /// Reset all completed steps for a workflow run back to `pending`.
    ///
    /// Used for full restart (--restart) to re-run from scratch.
    pub fn reset_completed_steps(&self, workflow_run_id: &str) -> Result<u64> {
        let count = self
            .conn
            .execute(Self::SQL_RESET_COMPLETED, params![workflow_run_id])?;
        Ok(count as u64)
    }

    /// Reset all steps at or after a given position back to `pending`.
    ///
    /// Used for --from-step to re-run from a specific step onwards.
    pub fn reset_steps_from_position(&self, workflow_run_id: &str, position: i64) -> Result<u64> {
        let count = self
            .conn
            .execute(Self::SQL_RESET_FROM_POS, params![workflow_run_id, position])?;
        Ok(count as u64)
    }

    /// Return the set of completed step keys as `(step_name, iteration)` pairs.
    ///
    /// Used to build the skip set for resume.
    pub fn get_completed_step_keys(&self, workflow_run_id: &str) -> Result<HashSet<StepKey>> {
        let steps = self.get_workflow_steps(workflow_run_id)?;
        Ok(completed_keys_from_steps(&steps))
    }

    /// Delete workflow runs with the given statuses, optionally scoped to a repo.
    ///
    /// `statuses` should be a non-empty slice of terminal status strings
    /// (`"completed"`, `"failed"`, `"cancelled"`). `workflow_run_steps` rows are
    /// removed automatically via `ON DELETE CASCADE`.
    ///
    /// Returns the number of deleted rows.
    pub fn purge(&self, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let (where_clause, params) = purge_where_clause(statuses, repo_id);
        let sql = format!("DELETE FROM workflow_runs WHERE {where_clause}");
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        Ok(self.conn.execute(&sql, params_ref.as_slice())?)
    }

    /// Count workflow runs that *would* be deleted by [`purge`] with the same arguments.
    ///
    /// Used by `--dry-run` to preview the deletion without modifying the database.
    pub fn purge_count(&self, repo_id: Option<&str>, statuses: &[&str]) -> Result<usize> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let (where_clause, params) = purge_where_clause(statuses, repo_id);
        let sql = format!("SELECT COUNT(*) FROM workflow_runs WHERE {where_clause}");
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let count: i64 = self
            .conn
            .query_row(&sql, params_ref.as_slice(), |row| row.get(0))?;
        Ok(count as usize)
    }
}

/// Build the WHERE clause and owned parameter list for purge / purge_count queries.
///
/// Returns `(where_clause, params)` where `params` is a `Vec<String>` whose
/// elements bind to the positional placeholders in the clause.
fn purge_where_clause(statuses: &[&str], repo_id: Option<&str>) -> (String, Vec<String>) {
    let n = statuses.len();
    let placeholders = (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let where_clause = if repo_id.is_some() {
        format!(
            "status IN ({placeholders}) AND worktree_id IN \
             (SELECT id FROM worktrees WHERE repo_id = ?{})",
            n + 1
        )
    } else {
        format!("status IN ({placeholders})")
    };
    let mut params: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
    if let Some(rid) = repo_id {
        params.push(rid.to_string());
    }
    (where_clause, params)
}

fn row_to_workflow_run(row: &rusqlite::Row) -> rusqlite::Result<WorkflowRun> {
    let dry_run_int: i64 = row.get(5)?;
    let inputs_json: Option<String> = row.get(11)?;
    let inputs: HashMap<String, String> = inputs_json
        .as_deref()
        .map(|s| {
            serde_json::from_str(s).unwrap_or_else(|e| {
                tracing::warn!("Malformed inputs JSON in workflow run: {e}");
                HashMap::new()
            })
        })
        .unwrap_or_default();
    let ticket_id: Option<String> = row.get(12)?;
    let repo_id: Option<String> = row.get(13)?;
    Ok(WorkflowRun {
        id: row.get(0)?,
        workflow_name: row.get(1)?,
        worktree_id: row.get::<_, Option<String>>(2)?,
        parent_run_id: row.get(3)?,
        status: row.get(4)?,
        dry_run: dry_run_int != 0,
        trigger: row.get(6)?,
        started_at: row.get(7)?,
        ended_at: row.get(8)?,
        result_summary: row.get(9)?,
        definition_snapshot: row.get(10)?,
        inputs,
        ticket_id,
        repo_id,
    })
}

fn row_to_workflow_step(row: &rusqlite::Row) -> rusqlite::Result<WorkflowRunStep> {
    let can_commit_int: i64 = row.get(4)?;
    let condition_met_int: Option<i64> = row.get(12)?;
    Ok(WorkflowRunStep {
        id: row.get(0)?,
        workflow_run_id: row.get(1)?,
        step_name: row.get(2)?,
        role: row.get(3)?,
        can_commit: can_commit_int != 0,
        condition_expr: row.get(5)?,
        status: row.get(6)?,
        child_run_id: row.get(7)?,
        position: row.get(8)?,
        started_at: row.get(9)?,
        ended_at: row.get(10)?,
        result_text: row.get(11)?,
        condition_met: condition_met_int.map(|v| v != 0),
        iteration: row.get(13)?,
        parallel_group_id: row.get(14)?,
        context_out: row.get(15)?,
        markers_out: row.get(16)?,
        retry_count: row.get(17)?,
        gate_type: row.get(18)?,
        gate_prompt: row.get(19)?,
        gate_timeout: row.get(20)?,
        gate_approved_by: row.get(21)?,
        gate_approved_at: row.get(22)?,
        gate_feedback: row.get(23)?,
        structured_output: row.get(24)?,
    })
}

// ---------------------------------------------------------------------------
// Variable substitution
// ---------------------------------------------------------------------------

/// Replace `{{key}}` placeholders in a prompt with values from `vars`.
fn substitute_variables(prompt: &str, vars: &HashMap<&str, String>) -> String {
    let mut result = prompt.to_string();
    for (key, value) in vars {
        let pattern = format!("{{{{{key}}}}}");
        result = result.replace(&pattern, value);
    }
    result
}

/// Build the variable map from execution state (used for substitution in sub-workflow inputs).
fn build_variable_map<'a>(state: &'a ExecutionState<'_>) -> HashMap<&'a str, String> {
    let mut vars: HashMap<&str, String> = HashMap::new();
    for (k, v) in &state.inputs {
        vars.insert(k.as_str(), v.clone());
    }
    let prior_context = state
        .contexts
        .last()
        .map(|c| c.context.clone())
        .unwrap_or_default();
    vars.insert("prior_context", prior_context);
    let prior_contexts_json = serde_json::to_string(&state.contexts).unwrap_or_default();
    vars.insert("prior_contexts", prior_contexts_json);
    if let Some(ref feedback) = state.last_gate_feedback {
        vars.insert("gate_feedback", feedback.clone());
    }
    // prior_output: raw JSON from the last step's structured output (if any)
    if let Some(last_output) = state.last_structured_output.as_ref() {
        vars.insert("prior_output", last_output.clone());
    }
    vars
}

/// Build a fully-substituted agent prompt from the execution state and agent definition.
///
/// Handles: input variables, prior_context, prior_contexts, prior_output,
/// gate_feedback, dry-run prefix for committing agents, prompt snippets (via
/// `with`), and CONDUCTOR_OUTPUT instruction (generic or schema-specific).
///
/// Prompt composition order:
/// 1. Agent .md body (with variable substitution)
/// 2. `with` prompt snippets (with variable substitution)
/// 3. Schema output instructions / CONDUCTOR_OUTPUT
fn build_agent_prompt(
    state: &ExecutionState<'_>,
    agent_def: &agent_config::AgentDef,
    schema: Option<&schema_config::OutputSchema>,
    snippet_text: &str,
) -> String {
    let vars = build_variable_map(state);
    let mut prompt = substitute_variables(&agent_def.prompt, &vars);

    if agent_def.can_commit && state.exec_config.dry_run {
        prompt = format!("DO NOT commit or push any changes. This is a dry run.\n\n{prompt}");
    }

    // Append prompt snippets (already concatenated by caller)
    if !snippet_text.is_empty() {
        let substituted = substitute_variables(snippet_text, &vars);
        prompt.push_str("\n\n");
        prompt.push_str(&substituted);
    }

    // Append output instructions: schema-specific if a schema is provided,
    // otherwise the generic CONDUCTOR_OUTPUT instruction.
    match schema {
        Some(s) => {
            prompt.push('\n');
            prompt.push_str(&schema_config::generate_prompt_instructions(s));
        }
        None => {
            prompt.push_str(CONDUCTOR_OUTPUT_INSTRUCTION);
        }
    }
    prompt
}

// ---------------------------------------------------------------------------
// Execution state
// ---------------------------------------------------------------------------

/// A step key is a `(name, iteration)` pair used for skip-set and step-map lookups.
type StepKey = (String, u32);

/// Extract completed step keys from a slice of step records.
///
/// Shared by [`WorkflowManager::get_completed_step_keys`] and [`resume_workflow`]
/// so the key-building logic lives in one place.
fn completed_keys_from_steps(steps: &[WorkflowRunStep]) -> HashSet<StepKey> {
    steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| (s.step_name.clone(), s.iteration as u32))
        .collect()
}

/// Pre-loaded context for resuming a workflow run.
///
/// Separated from [`ExecutionState`] so that fresh runs carry no resume
/// overhead and the borrow-splitting between "read completed data" and
/// "mutate execution state" is explicit.
struct ResumeContext {
    /// Step keys to skip (e.g. `("lint", 0)`).
    skip_completed: HashSet<StepKey>,
    /// Completed step records keyed by step key, for O(1) restore.
    step_map: HashMap<StepKey, WorkflowRunStep>,
    /// Pre-loaded child agent runs keyed by run ID, avoiding N+1 queries
    /// when accumulating costs during restore.
    child_runs: HashMap<String, crate::agent::AgentRun>,
}

/// Mutable runtime state for a workflow execution.
struct ExecutionState<'a> {
    conn: &'a Connection,
    config: &'a Config,
    workflow_run_id: String,
    workflow_name: String,
    worktree_id: Option<String>,
    working_dir: String,
    worktree_slug: String,
    repo_path: String,
    ticket_id: Option<String>,
    repo_id: Option<String>,
    model: Option<String>,
    exec_config: WorkflowExecConfig,
    inputs: HashMap<String, String>,
    agent_mgr: AgentManager<'a>,
    wf_mgr: WorkflowManager<'a>,
    parent_run_id: String,
    /// Current nesting depth (0 = top-level workflow).
    depth: u32,
    // Runtime
    step_results: HashMap<String, StepResult>,
    contexts: Vec<ContextEntry>,
    position: i64,
    all_succeeded: bool,
    total_cost: f64,
    total_turns: i64,
    total_duration_ms: i64,
    last_gate_feedback: Option<String>,
    /// Raw JSON from the most recent step's structured output (for `{{prior_output}}`).
    last_structured_output: Option<String>,
    /// Block-level output schema name inherited from an enclosing `do {}` block.
    block_output: Option<String>,
    /// Block-level prompt snippet refs inherited from an enclosing `do {}` block.
    block_with: Vec<String>,
    /// Resume context — `None` for fresh runs, `Some` when resuming.
    resume_ctx: Option<ResumeContext>,
}

// ---------------------------------------------------------------------------
// Execution engine
// ---------------------------------------------------------------------------

/// Validate required workflow inputs are present and apply default values.
///
/// Returns an error if a required input is missing.
pub fn apply_workflow_input_defaults(
    workflow: &WorkflowDef,
    inputs: &mut HashMap<String, String>,
) -> Result<()> {
    for input_decl in &workflow.inputs {
        if input_decl.required && !inputs.contains_key(&input_decl.name) {
            return Err(ConductorError::Workflow(format!(
                "Missing required input: '{}'. Use --input {}=<value>.",
                input_decl.name, input_decl.name
            )));
        }
        if let Some(ref default) = input_decl.default {
            inputs
                .entry(input_decl.name.clone())
                .or_insert_with(|| default.clone());
        }
    }
    Ok(())
}

/// Input parameters for workflow execution.
pub struct WorkflowExecInput<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub workflow: &'a WorkflowDef,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<&'a str>,
    pub working_dir: &'a str,
    pub repo_path: &'a str,
    pub model: Option<&'a str>,
    pub exec_config: &'a WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
    pub ticket_id: Option<&'a str>,
    pub repo_id: Option<&'a str>,
    /// Current nesting depth for sub-workflow calls (0 = top-level).
    pub depth: u32,
}

/// Execute a workflow definition against a worktree.
pub fn execute_workflow(input: &WorkflowExecInput<'_>) -> Result<WorkflowResult> {
    let conn = input.conn;
    let config = input.config;
    let workflow = input.workflow;

    let agent_mgr = AgentManager::new(conn);
    let wf_mgr = WorkflowManager::new(conn);
    let worktree_slug = if let Some(wt_id) = input.worktree_id {
        let wt_mgr = WorktreeManager::new(conn, config);
        wt_mgr.get_by_id(wt_id)?.slug
    } else {
        String::new()
    };

    // Validate all referenced agents exist before starting
    let mut all_agents = workflow_dsl::collect_agent_names(&workflow.body);
    all_agents.extend(workflow_dsl::collect_agent_names(&workflow.always));
    all_agents.sort();
    all_agents.dedup();

    let specs: Vec<AgentSpec> = all_agents.iter().map(AgentSpec::from).collect();
    let missing_agents = agent_config::find_missing_agents(
        input.working_dir,
        input.repo_path,
        &specs,
        Some(&workflow.name),
    );
    if !missing_agents.is_empty() {
        return Err(ConductorError::Workflow(format!(
            "Missing agent definitions: {}. Run 'conductor workflow validate' for details.",
            missing_agents.join(", ")
        )));
    }

    // Validate all referenced prompt snippets exist before starting
    let all_snippets = workflow.collect_all_snippet_refs();

    if !all_snippets.is_empty() {
        let missing_snippets = prompt_config::find_missing_snippets(
            input.working_dir,
            input.repo_path,
            &all_snippets,
            Some(&workflow.name),
        );
        if !missing_snippets.is_empty() {
            return Err(ConductorError::Workflow(format!(
                "Missing prompt snippets: {}. Check .conductor/prompts/ directory.",
                missing_snippets.join(", ")
            )));
        }
    }

    // Snapshot the definition
    let snapshot_json = serde_json::to_string(workflow).map_err(|e| {
        ConductorError::Workflow(format!("Failed to serialize workflow definition: {e}"))
    })?;

    // Guard: prevent multiple concurrent top-level runs on the same worktree
    // (skipped for ephemeral PR runs which have no registered worktree).
    if input.depth == 0 {
        if let Some(wt_id) = input.worktree_id {
            if let Some(active) = wf_mgr.get_active_run_for_worktree(wt_id)? {
                return Err(ConductorError::WorkflowRunAlreadyActive {
                    name: active.workflow_name,
                });
            }
        }
    }

    // Create parent agent run (uses empty worktree_id for ephemeral PR runs).
    let parent_prompt = format!("Workflow: {} — {}", workflow.name, workflow.description);
    let parent_run = agent_mgr.create_run(input.worktree_id, &parent_prompt, None, input.model)?;

    // Create workflow run record with snapshot and target FKs in a single INSERT
    let wf_run = wf_mgr.create_workflow_run_with_targets(
        &workflow.name,
        input.worktree_id,
        input.ticket_id,
        input.repo_id,
        &parent_run.id,
        input.exec_config.dry_run,
        &workflow.trigger.to_string(),
        Some(&snapshot_json),
    )?;

    // Build inputs map, injecting implicit ticket/repo variables
    let mut merged_inputs = input.inputs.clone();
    if let Some(tid) = input.ticket_id {
        let ticket = crate::tickets::TicketSyncer::new(conn).get_by_id(tid)?;
        merged_inputs
            .entry("ticket_id".to_string())
            .or_insert_with(|| ticket.id.clone());
        merged_inputs
            .entry("ticket_title".to_string())
            .or_insert_with(|| ticket.title.clone());
        merged_inputs
            .entry("ticket_url".to_string())
            .or_insert_with(|| ticket.url.clone());
    }
    if let Some(rid) = input.repo_id {
        let repo = crate::repo::RepoManager::new(conn, config).get_by_id(rid)?;
        merged_inputs
            .entry("repo_id".to_string())
            .or_insert_with(|| repo.id.clone());
        merged_inputs
            .entry("repo_path".to_string())
            .or_insert_with(|| repo.local_path.clone());
        merged_inputs
            .entry("repo_name".to_string())
            .or_insert_with(|| repo.slug.clone());
    }

    // Persist inputs so they can be restored on resume
    if !merged_inputs.is_empty() {
        wf_mgr.set_workflow_run_inputs(&wf_run.id, &merged_inputs)?;
    }

    // Mark as running
    wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None)?;

    let mut state = ExecutionState {
        conn,
        config,
        workflow_run_id: wf_run.id.clone(),
        workflow_name: workflow.name.clone(),
        worktree_id: input.worktree_id.map(String::from),
        working_dir: input.working_dir.to_string(),
        worktree_slug,
        repo_path: input.repo_path.to_string(),
        ticket_id: input.ticket_id.map(String::from),
        repo_id: input.repo_id.map(String::from),
        model: input.model.map(String::from),
        exec_config: input.exec_config.clone(),
        inputs: merged_inputs,
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: parent_run.id.clone(),
        depth: input.depth,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        last_structured_output: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
    };

    run_workflow_engine(&mut state, workflow)
}

/// Shared orchestration: execute body → always block → build summary → finalize.
///
/// Both `execute_workflow` and `resume_workflow` delegate here after constructing
/// their `ExecutionState`.
fn run_workflow_engine(
    state: &mut ExecutionState<'_>,
    workflow: &WorkflowDef,
) -> Result<WorkflowResult> {
    // Execute main body
    let mut body_error: Option<String> = None;
    let body_result = execute_nodes(state, &workflow.body);
    if let Err(ref e) = body_result {
        let msg = e.to_string();
        tracing::error!("Body execution error: {msg}");
        state.all_succeeded = false;
        body_error = Some(msg);
    }

    // Execute always block regardless of outcome
    if !workflow.always.is_empty() {
        let workflow_status = if state.all_succeeded {
            "completed"
        } else {
            "failed"
        };
        state
            .inputs
            .insert("workflow_status".to_string(), workflow_status.to_string());
        let always_result = execute_nodes(state, &workflow.always);
        if let Err(ref e) = always_result {
            tracing::warn!("Always block error (non-fatal): {e}");
        }
    }

    // Build summary
    let mut summary = build_workflow_summary(state);
    if let Some(ref err) = body_error {
        summary.push_str(&format!("\nError: {err}"));
    }

    // Finalize
    let wf_run_id = state.workflow_run_id.clone();
    let parent_run_id = state.parent_run_id.clone();
    if state.all_succeeded {
        state.agent_mgr.update_run_completed(
            &parent_run_id,
            None,
            Some(&summary),
            Some(state.total_cost),
            Some(state.total_turns),
            Some(state.total_duration_ms),
        )?;
        state.wf_mgr.update_workflow_status(
            &wf_run_id,
            WorkflowRunStatus::Completed,
            Some(&summary),
        )?;
        tracing::info!("Workflow '{}' completed successfully", workflow.name);
    } else {
        state
            .agent_mgr
            .update_run_failed(&parent_run_id, &summary)?;
        state.wf_mgr.update_workflow_status(
            &wf_run_id,
            WorkflowRunStatus::Failed,
            Some(&summary),
        )?;
        tracing::warn!("Workflow '{}' finished with failures", workflow.name);
    }

    tracing::info!(
        "Total: ${:.4}, {} turns, {:.1}s",
        state.total_cost,
        state.total_turns,
        state.total_duration_ms as f64 / 1000.0
    );

    Ok(WorkflowResult {
        workflow_run_id: wf_run_id,
        worktree_id: state.worktree_id.clone(),
        workflow_name: workflow.name.clone(),
        all_succeeded: state.all_succeeded,
        total_cost: state.total_cost,
        total_turns: state.total_turns,
        total_duration_ms: state.total_duration_ms,
    })
}

/// Owned inputs for [`execute_workflow_standalone`], avoiding lifetime issues
/// when spawning background threads.
pub struct WorkflowExecStandalone {
    pub config: Config,
    pub workflow: WorkflowDef,
    /// `None` for ephemeral PR runs with no registered worktree.
    pub worktree_id: Option<String>,
    pub working_dir: String,
    pub repo_path: String,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    pub model: Option<String>,
    pub exec_config: WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
}

/// Execute a workflow in a self-contained manner: opens its own database
/// connection and resolves the conductor binary path. Designed for use in
/// background threads where the caller cannot share a `&Connection`.
pub fn execute_workflow_standalone(params: &WorkflowExecStandalone) -> Result<WorkflowResult> {
    let db = crate::config::db_path();
    let conn = crate::db::open_database(&db)?;

    let input = WorkflowExecInput {
        conn: &conn,
        config: &params.config,
        workflow: &params.workflow,
        worktree_id: params.worktree_id.as_deref(),
        working_dir: &params.working_dir,
        repo_path: &params.repo_path,
        ticket_id: params.ticket_id.as_deref(),
        repo_id: params.repo_id.as_deref(),
        model: params.model.as_deref(),
        exec_config: &params.exec_config,
        inputs: params.inputs.clone(),
        depth: 0,
    };

    execute_workflow(&input)
}

/// Owned inputs for [`resume_workflow_standalone`], avoiding lifetime issues
/// when spawning background threads.
pub struct WorkflowResumeStandalone {
    pub config: Config,
    pub workflow_run_id: String,
    pub model: Option<String>,
    pub from_step: Option<String>,
    pub restart: bool,
}

/// Validate resume preconditions that can be checked from status alone.
///
/// Shared by the core `resume_workflow` function and the web endpoint so that
/// validation rules and error strings stay in a single place.
pub fn validate_resume_preconditions(
    status: &WorkflowRunStatus,
    restart: bool,
    from_step: Option<&str>,
) -> Result<()> {
    if matches!(status, WorkflowRunStatus::Completed) && !restart {
        return Err(ConductorError::Workflow(
            "Cannot resume a completed workflow run. Use --restart to re-run from the beginning."
                .to_string(),
        ));
    }
    if matches!(status, WorkflowRunStatus::Running) {
        return Err(ConductorError::Workflow(
            "Cannot resume a workflow run that is already running.".to_string(),
        ));
    }
    if matches!(status, WorkflowRunStatus::Cancelled) {
        return Err(ConductorError::Workflow(
            "Cannot resume a cancelled workflow run.".to_string(),
        ));
    }
    if restart && from_step.is_some() {
        return Err(ConductorError::Workflow(
            "Cannot use --restart and --from-step together: --restart re-runs all steps, \
             --from-step resumes from a specific step."
                .to_string(),
        ));
    }
    Ok(())
}

/// Resume a workflow in a self-contained manner: opens its own database
/// connection. Designed for use in background threads.
pub fn resume_workflow_standalone(params: &WorkflowResumeStandalone) -> Result<WorkflowResult> {
    let db = crate::config::db_path();
    let conn = crate::db::open_database(&db)?;

    let input = WorkflowResumeInput {
        conn: &conn,
        config: &params.config,
        workflow_run_id: &params.workflow_run_id,
        model: params.model.as_deref(),
        from_step: params.from_step.as_deref(),
        restart: params.restart,
    };

    resume_workflow(&input)
}

/// Input parameters for resuming a workflow run.
pub struct WorkflowResumeInput<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub workflow_run_id: &'a str,
    /// Optional model override for agent steps.
    pub model: Option<&'a str>,
    /// Resume from a specific step name (re-runs steps from that point).
    pub from_step: Option<&'a str>,
    /// Restart from the beginning (clear all step results).
    pub restart: bool,
}

/// Resume a failed or stalled workflow run from the point of failure.
///
/// Loads the workflow definition from the run's `definition_snapshot`, rebuilds
/// the skip set from completed steps, resets failed steps to pending, and
/// re-enters the execution loop.
pub fn resume_workflow(input: &WorkflowResumeInput<'_>) -> Result<WorkflowResult> {
    let conn = input.conn;
    let config = input.config;
    let wf_mgr = WorkflowManager::new(conn);
    let wt_mgr = WorktreeManager::new(conn, config);

    // Load and validate the workflow run
    let wf_run = wf_mgr
        .get_workflow_run(input.workflow_run_id)?
        .ok_or_else(|| {
            ConductorError::Workflow(format!("Workflow run not found: {}", input.workflow_run_id))
        })?;

    validate_resume_preconditions(&wf_run.status, input.restart, input.from_step)?;

    // Load all steps once (avoids N+1 queries later)
    let all_steps = wf_mgr.get_workflow_steps(&wf_run.id)?;

    // Validate --from-step early (fail-fast before heavier worktree/snapshot operations)
    if let Some(from_step) = input.from_step {
        if !input.restart && !all_steps.iter().any(|s| s.step_name == from_step) {
            return Err(ConductorError::Workflow(format!(
                "Step '{}' not found in workflow run '{}'",
                from_step, wf_run.id
            )));
        }
    }

    // Fail early for ephemeral PR runs (no worktree_id, repo_id, or ticket_id).
    if wf_run.worktree_id.is_none() && wf_run.repo_id.is_none() && wf_run.ticket_id.is_none() {
        return Err(ConductorError::Workflow(format!(
            "Workflow run '{}' was an ephemeral PR run with no registered worktree — cannot resume.",
            wf_run.id
        )));
    }

    // Deserialize definition from snapshot
    let snapshot = wf_run.definition_snapshot.as_deref().ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Workflow run '{}' has no definition snapshot — cannot resume.",
            wf_run.id
        ))
    })?;
    let workflow: WorkflowDef = serde_json::from_str(snapshot).map_err(|e| {
        ConductorError::Workflow(format!("Failed to deserialize workflow snapshot: {e}"))
    })?;

    // Determine execution paths based on target type.
    // - Worktree run: look up worktree and derive repo from it.
    // - Repo/ticket run: look up repo directly (via repo_id or ticket.repo_id).
    let (worktree_path, worktree_slug, repo_path) =
        if let Some(wt_id) = wf_run.worktree_id.as_deref() {
            let worktree = wt_mgr.get_by_id(wt_id)?;
            let repo = crate::repo::RepoManager::new(conn, config).get_by_id(&worktree.repo_id)?;
            (
                worktree.path.clone(),
                worktree.slug.clone(),
                repo.local_path.clone(),
            )
        } else {
            // Resolve repo_id from the run or via the linked ticket.
            // (The ephemeral guard above ensures at least one FK is set.)
            let effective_repo_id = if let Some(rid) = wf_run.repo_id.as_deref() {
                rid.to_string()
            } else {
                let tid = wf_run.ticket_id.as_deref().expect("guarded above");
                crate::tickets::TicketSyncer::new(conn)
                    .get_by_id(tid)
                    .map_err(|e| {
                        ConductorError::Workflow(format!(
                            "Cannot resolve repo for ticket '{}' during resume: {e}",
                            tid
                        ))
                    })?
                    .repo_id
            };
            let repo = crate::repo::RepoManager::new(conn, config).get_by_id(&effective_repo_id)?;
            let path = repo.local_path.clone();
            (path.clone(), String::new(), path)
        };

    // Build the skip set
    let skip_completed = if input.restart {
        // Restart: clear all step results — skip nothing
        wf_mgr.reset_failed_steps(&wf_run.id)?;
        wf_mgr.reset_completed_steps(&wf_run.id)?;
        HashSet::new()
    } else {
        let mut keys = completed_keys_from_steps(&all_steps);

        // Handle --from-step: remove completed keys at or after the specified step
        if let Some(from_step) = input.from_step {
            // Safety: from_step existence was validated above
            let pos = all_steps
                .iter()
                .find(|s| s.step_name == from_step)
                .expect("from_step validated above")
                .position;

            let to_remove: Vec<StepKey> = all_steps
                .iter()
                .filter(|s| s.position >= pos && s.status == WorkflowStepStatus::Completed)
                .map(|s| (s.step_name.clone(), s.iteration as u32))
                .collect();
            for key in to_remove {
                keys.remove(&key);
            }
            // Reset those steps in DB
            wf_mgr.reset_steps_from_position(&wf_run.id, pos)?;
        }

        // Reset non-completed steps
        wf_mgr.reset_failed_steps(&wf_run.id)?;
        keys
    };

    // Build the step map from `all_steps` (only the keys still in skip_completed
    // survived any --from-step pruning, so filter by membership).
    let step_map: HashMap<StepKey, WorkflowRunStep> = all_steps
        .into_iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| {
            let key = (s.step_name.clone(), s.iteration as u32);
            (key, s)
        })
        .filter(|(key, _)| skip_completed.contains(key))
        .collect();

    // Batch-load child agent runs in a single query to avoid N+1 during cost accumulation
    let agent_mgr = AgentManager::new(conn);
    let child_run_ids: Vec<&str> = step_map
        .values()
        .filter_map(|s| s.child_run_id.as_deref())
        .collect();
    let child_runs = agent_mgr.get_runs_by_ids(&child_run_ids)?;

    let resume_ctx = if skip_completed.is_empty() {
        None
    } else {
        Some(ResumeContext {
            skip_completed,
            step_map,
            child_runs,
        })
    };

    // Reset run status to Running
    wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None)?;

    tracing::info!(
        "Resuming workflow '{}' (run {}), {} completed steps to skip",
        workflow.name,
        wf_run.id,
        resume_ctx
            .as_ref()
            .map_or(0, |ctx| ctx.skip_completed.len()),
    );

    let mut state = ExecutionState {
        conn,
        config,
        workflow_run_id: wf_run.id.clone(),
        workflow_name: workflow.name.clone(),
        worktree_id: wf_run.worktree_id.clone(),
        working_dir: worktree_path,
        worktree_slug,
        repo_path,
        ticket_id: wf_run.ticket_id.clone(),
        repo_id: wf_run.repo_id.clone(),
        model: input.model.map(String::from),
        exec_config: WorkflowExecConfig::default(),
        inputs: wf_run.inputs.clone(),
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: wf_run.parent_run_id.clone(),
        depth: 0,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        last_structured_output: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx,
    };

    run_workflow_engine(&mut state, &workflow)
}

/// Walk a list of workflow nodes, dispatching to the appropriate handler.
fn execute_single_node(
    state: &mut ExecutionState<'_>,
    node: &WorkflowNode,
    iteration: u32,
) -> Result<()> {
    match node {
        WorkflowNode::Call(n) => execute_call(state, n, iteration)?,
        WorkflowNode::CallWorkflow(n) => execute_call_workflow(state, n, iteration)?,
        WorkflowNode::If(n) => execute_if(state, n)?,
        WorkflowNode::Unless(n) => execute_unless(state, n)?,
        WorkflowNode::While(n) => execute_while(state, n)?,
        WorkflowNode::DoWhile(n) => execute_do_while(state, n)?,
        WorkflowNode::Do(n) => execute_do(state, n)?,
        WorkflowNode::Parallel(n) => execute_parallel(state, n, iteration)?,
        WorkflowNode::Gate(n) => execute_gate(state, n, iteration)?,
        WorkflowNode::Always(n) => {
            // Nested always — just execute body
            execute_nodes(state, &n.body)?;
        }
    }
    Ok(())
}

fn execute_nodes(state: &mut ExecutionState<'_>, nodes: &[WorkflowNode]) -> Result<()> {
    for node in nodes {
        if !state.all_succeeded && state.exec_config.fail_fast {
            break;
        }
        execute_single_node(state, node, 0)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers for retry-exhaustion epilogue
// ---------------------------------------------------------------------------

/// Run the on_fail agent after all retries for a step are exhausted.
///
/// Injects `failed_step`, `failure_reason`, and `retry_count` into the
/// workflow inputs for the duration of the on_fail call, then cleans up.
fn run_on_fail_agent(
    state: &mut ExecutionState<'_>,
    step_label: &str,
    on_fail_agent: &crate::workflow_dsl::AgentRef,
    last_error: &str,
    retries: u32,
    iteration: u32,
) {
    tracing::warn!(
        "All retries exhausted for '{}', running on_fail agent '{}'",
        step_label,
        on_fail_agent.label(),
    );
    state
        .inputs
        .insert("failed_step".to_string(), step_label.to_string());
    state
        .inputs
        .insert("failure_reason".to_string(), last_error.to_string());
    state
        .inputs
        .insert("retry_count".to_string(), retries.to_string());

    let on_fail_node = CallNode {
        agent: on_fail_agent.clone(),
        retries: 0,
        on_fail: None,
        output: None,
        with: Vec::new(),
    };
    if let Err(e) = execute_call(state, &on_fail_node, iteration) {
        tracing::warn!("on_fail agent '{}' also failed: {e}", on_fail_agent.label(),);
    }

    state.inputs.remove("failed_step");
    state.inputs.remove("failure_reason");
    state.inputs.remove("retry_count");
}

/// Record a failed step result and optionally return a fail-fast error.
fn record_step_failure(
    state: &mut ExecutionState<'_>,
    step_key: String,
    step_label: &str,
    last_error: String,
    max_attempts: u32,
) -> Result<()> {
    state.all_succeeded = false;
    let step_result = StepResult {
        step_name: step_label.to_string(),
        status: WorkflowStepStatus::Failed,
        result_text: Some(last_error),
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: Vec::new(),
        context: String::new(),
        child_run_id: None,
        structured_output: None,
    };
    state.step_results.insert(step_key, step_result);

    if state.exec_config.fail_fast {
        return Err(ConductorError::Workflow(format!(
            "Step '{}' failed after {} attempts",
            step_label, max_attempts
        )));
    }

    Ok(())
}

/// Record a successful step: accumulate stats, insert StepResult, push context.
#[allow(clippy::too_many_arguments)]
fn record_step_success(
    state: &mut ExecutionState<'_>,
    step_key: String,
    step_name: &str,
    result_text: Option<String>,
    cost_usd: Option<f64>,
    num_turns: Option<i64>,
    duration_ms: Option<i64>,
    markers: Vec<String>,
    context: String,
    child_run_id: Option<String>,
    iteration: u32,
    structured_output: Option<String>,
) {
    if let Some(cost) = cost_usd {
        state.total_cost += cost;
    }
    if let Some(turns) = num_turns {
        state.total_turns += turns;
    }
    if let Some(dur) = duration_ms {
        state.total_duration_ms += dur;
    }

    // Update last_structured_output for {{prior_output}} substitution
    if structured_output.is_some() {
        state.last_structured_output = structured_output.clone();
    }

    let step_result = StepResult {
        step_name: step_name.to_string(),
        status: WorkflowStepStatus::Completed,
        result_text,
        cost_usd,
        num_turns,
        duration_ms,
        markers,
        context: context.clone(),
        child_run_id,
        structured_output,
    };
    state.step_results.insert(step_key, step_result);

    state.contexts.push(ContextEntry {
        step: step_name.to_string(),
        iteration,
        context,
    });
}

/// Resolve child workflow inputs: substitute variables, apply defaults, and
/// check for missing required inputs.
///
/// Returns `Ok(resolved_inputs)` or `Err(missing_input_name)`.
fn resolve_child_inputs(
    raw_inputs: &HashMap<String, String>,
    vars: &HashMap<&str, String>,
    input_decls: &[workflow_dsl::InputDecl],
) -> std::result::Result<HashMap<String, String>, String> {
    let mut child_inputs = HashMap::new();
    for (k, v) in raw_inputs {
        child_inputs.insert(k.clone(), substitute_variables(v, vars));
    }
    for decl in input_decls {
        if !child_inputs.contains_key(&decl.name) {
            if decl.required {
                return Err(decl.name.clone());
            }
            if let Some(ref default) = decl.default {
                child_inputs.insert(decl.name.clone(), default.clone());
            }
        }
    }
    Ok(child_inputs)
}

// ---------------------------------------------------------------------------
// Node executors
// ---------------------------------------------------------------------------

fn execute_call(state: &mut ExecutionState<'_>, node: &CallNode, iteration: u32) -> Result<()> {
    // Call-level output overrides block-level; if neither is set, use None.
    // We must clone into a local because execute_call_with_schema takes &mut state.
    let effective_output: Option<String> = match (&node.output, &state.block_output) {
        (Some(o), _) => Some(o.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };
    // Block-level `with` snippets prepended to call-level `with`.
    // Only allocate a new Vec when both sources are non-empty; when only one
    // source has entries, clone it into a local so we don't hold a borrow on
    // state across the mutable call to execute_call_with_schema.
    let effective_with: Vec<String> = if state.block_with.is_empty() {
        node.with.clone()
    } else if node.with.is_empty() {
        state.block_with.clone()
    } else {
        state
            .block_with
            .iter()
            .chain(node.with.iter())
            .cloned()
            .collect()
    };
    execute_call_with_schema(
        state,
        node,
        iteration,
        effective_output.as_deref(),
        &effective_with,
    )
}

/// Inner implementation of execute_call that accepts an optional schema override
/// and prompt snippet references.
///
/// The `schema_override` parameter allows parallel blocks to pass their block-level
/// output schema to individual calls. The `with_refs` parameter provides prompt
/// snippet names to load and append to the agent prompt.
fn execute_call_with_schema(
    state: &mut ExecutionState<'_>,
    node: &CallNode,
    iteration: u32,
    schema_name: Option<&str>,
    with_refs: &[String],
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    let step_key_check = node.agent.step_key();
    if should_skip(state, &step_key_check, iteration) {
        tracing::info!(
            "Skipping completed step '{}' (iteration {})",
            step_key_check,
            iteration
        );
        restore_step(state, &step_key_check, iteration);
        return Ok(());
    }

    // Load agent definition
    let agent_def = agent_config::load_agent(
        &state.working_dir,
        &state.repo_path,
        &AgentSpec::from(&node.agent),
        Some(&state.workflow_name),
    )?;
    let agent_label = node.agent.label();
    let step_key = node.agent.step_key();

    // Load output schema if specified
    let schema = schema_name
        .map(|name| resolve_schema(state, name))
        .transpose()?;

    // Load and concatenate prompt snippets
    let snippet_text = prompt_config::load_and_concat_snippets(
        &state.working_dir,
        &state.repo_path,
        with_refs,
        Some(&state.workflow_name),
    )?;

    let prompt = build_agent_prompt(state, &agent_def, schema.as_ref(), &snippet_text);
    let step_model = agent_def.model.as_deref().or(state.model.as_deref());

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            agent_label,
            &agent_def.role.to_string(),
            agent_def.can_commit,
            pos,
            iteration as i64,
        )?;

        let window_prefix = if state.worktree_slug.is_empty() {
            state
                .workflow_run_id
                .get(..8)
                .unwrap_or(&state.workflow_run_id)
        } else {
            state.worktree_slug.as_str()
        };
        let child_window = sanitize_tmux_name(&format!("{}-wf-{}", window_prefix, agent_label));
        let child_run = state.agent_mgr.create_child_run(
            state.worktree_id.as_deref(),
            &prompt,
            Some(&child_window),
            step_model,
            &state.parent_run_id,
        )?;

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child_run.id),
            None,
            None,
            None,
            Some(attempt as i64),
        )?;

        tracing::info!(
            "Step '{}' (attempt {}/{}): spawning in '{}'",
            agent_label,
            attempt + 1,
            max_attempts,
            child_window,
        );

        // Spawn in tmux
        if let Err(e) = agent_runtime::spawn_child_tmux(
            &child_run.id,
            &state.working_dir,
            &prompt,
            step_model,
            &child_window,
        ) {
            tracing::warn!("Failed to spawn child: {e}");
            let _ = state
                .agent_mgr
                .update_run_failed(&child_run.id, &format!("spawn failed: {e}"));
            state.wf_mgr.update_step_status(
                &step_id,
                WorkflowStepStatus::Failed,
                Some(&child_run.id),
                Some(&format!("spawn failed: {e}")),
                None,
                None,
                Some(attempt as i64),
            )?;
            last_error = format!("spawn failed: {e}");
            continue;
        }

        // Poll for completion
        match agent_runtime::poll_child_completion(
            state.conn,
            &child_run.id,
            state.exec_config.poll_interval,
            state.exec_config.step_timeout,
            state.exec_config.shutdown.as_ref(),
        ) {
            Ok(completed_run) => {
                let succeeded = completed_run.status == AgentRunStatus::Completed;

                // Parse output: structured (schema) or generic (markers + context)
                let (markers, context, structured_json) = match interpret_agent_output(
                    completed_run.result_text.as_deref(),
                    schema.as_ref(),
                    succeeded,
                ) {
                    Ok(result) => result,
                    Err(validation_err) => {
                        tracing::warn!(
                            "Step '{}' structured output validation failed: {validation_err}",
                            agent_label,
                        );
                        state.wf_mgr.update_step_status(
                            &step_id,
                            WorkflowStepStatus::Failed,
                            Some(&completed_run.id),
                            completed_run.result_text.as_deref(),
                            None,
                            None,
                            Some(attempt as i64),
                        )?;
                        last_error = validation_err;
                        continue;
                    }
                };

                let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                if succeeded {
                    tracing::info!(
                        "Step '{}' completed: cost=${:.4}, {} turns, markers={:?}",
                        agent_label,
                        completed_run.cost_usd.unwrap_or(0.0),
                        completed_run.num_turns.unwrap_or(0),
                        markers,
                    );

                    state.wf_mgr.update_step_status_full(
                        &step_id,
                        WorkflowStepStatus::Completed,
                        Some(&completed_run.id),
                        completed_run.result_text.as_deref(),
                        Some(&context),
                        Some(&markers_json),
                        Some(attempt as i64),
                        structured_json.as_deref(),
                    )?;

                    record_step_success(
                        state,
                        step_key.clone(),
                        agent_label,
                        completed_run.result_text,
                        completed_run.cost_usd,
                        completed_run.num_turns,
                        completed_run.duration_ms,
                        markers,
                        context,
                        Some(completed_run.id),
                        iteration,
                        structured_json,
                    );

                    return Ok(());
                } else {
                    tracing::warn!(
                        "Step '{}' failed (attempt {}/{}): {}",
                        agent_label,
                        attempt + 1,
                        max_attempts,
                        completed_run
                            .result_text
                            .as_deref()
                            .unwrap_or("unknown error"),
                    );

                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Failed,
                        Some(&completed_run.id),
                        completed_run.result_text.as_deref(),
                        Some(&context),
                        Some(&markers_json),
                        Some(attempt as i64),
                    )?;

                    last_error = completed_run
                        .result_text
                        .unwrap_or_else(|| "unknown error".to_string());
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!("Step '{}' poll error: {e}", agent_label);
                let _ = state.agent_mgr.update_run_cancelled(&child_run.id);
                let cancel_msg = e.to_string();
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&child_run.id),
                    Some(&cancel_msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                if matches!(e, agent_runtime::PollError::Shutdown) {
                    return Err(ConductorError::Workflow(cancel_msg));
                }
                last_error = cancel_msg;
                continue;
            }
        }
    }

    // All retries exhausted — run on_fail agent if specified
    if let Some(ref on_fail_agent) = node.on_fail {
        run_on_fail_agent(
            state,
            agent_label,
            on_fail_agent,
            &last_error,
            node.retries,
            iteration,
        );
    }

    record_step_failure(state, step_key, agent_label, last_error, max_attempts)
}

fn execute_call_workflow(
    state: &mut ExecutionState<'_>,
    node: &CallWorkflowNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    // Skip completed sub-workflow steps on resume
    let wf_step_name = format!("workflow:{}", node.workflow);
    if should_skip(state, &wf_step_name, iteration) {
        tracing::info!("Skipping completed sub-workflow '{}'", node.workflow);
        restore_step(state, &wf_step_name, iteration);
        return Ok(());
    }

    let child_depth = state.depth + 1;
    if child_depth > workflow_dsl::MAX_WORKFLOW_DEPTH {
        let msg = format!(
            "Workflow nesting depth exceeds maximum of {}: parent '{}' calling '{}'",
            workflow_dsl::MAX_WORKFLOW_DEPTH,
            state.workflow_name,
            node.workflow,
        );
        state.all_succeeded = false;
        if state.exec_config.fail_fast {
            return Err(ConductorError::Workflow(msg));
        }
        tracing::error!("{msg}");
        return Ok(());
    }

    let step_key = node.workflow.clone();

    // Load the child workflow definition once (it won't change between retries)
    let child_def =
        workflow_dsl::load_workflow_by_name(&state.working_dir, &state.repo_path, &node.workflow)
            .map_err(|e| {
            ConductorError::Workflow(format!(
                "Failed to load sub-workflow '{}': {e}",
                node.workflow
            ))
        })?;

    // Retry loop
    let max_attempts = 1 + node.retries;
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &wf_step_name,
            "workflow",
            false,
            pos,
            iteration as i64,
        )?;

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            Some(attempt as i64),
        )?;

        tracing::info!(
            "Step 'workflow:{}' (attempt {}/{}): executing sub-workflow",
            node.workflow,
            attempt + 1,
            max_attempts,
        );

        // Resolve child inputs: substitute variables, apply defaults, check required
        let vars = build_variable_map(state);
        let child_inputs = match resolve_child_inputs(&node.inputs, &vars, &child_def.inputs) {
            Ok(inputs) => inputs,
            Err(missing) => {
                let msg = format!(
                    "Sub-workflow '{}' requires input '{}' but it was not provided",
                    node.workflow, missing,
                );
                tracing::warn!("{msg}");
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = msg;
                continue;
            }
        };

        // Execute the child workflow
        let child_input = WorkflowExecInput {
            conn: state.conn,
            config: state.config,
            workflow: &child_def,
            worktree_id: state.worktree_id.as_deref(),
            working_dir: &state.working_dir,
            repo_path: &state.repo_path,
            ticket_id: state.ticket_id.as_deref(),
            repo_id: state.repo_id.as_deref(),
            model: state.model.as_deref(),
            exec_config: &state.exec_config,
            inputs: child_inputs,
            depth: child_depth,
        };

        match execute_workflow(&child_input) {
            Ok(result) => {
                if result.all_succeeded {
                    tracing::info!(
                        "Sub-workflow '{}' completed: cost=${:.4}, {} turns",
                        node.workflow,
                        result.total_cost,
                        result.total_turns,
                    );

                    // Bubble up the child's final step output (markers + context)
                    let (markers, context) =
                        fetch_child_final_output(&state.wf_mgr, &result.workflow_run_id);

                    let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Completed,
                        None,
                        Some(&format!("Sub-workflow '{}' completed", node.workflow)),
                        Some(&context),
                        Some(&markers_json),
                        Some(attempt as i64),
                    )?;

                    record_step_success(
                        state,
                        step_key.clone(),
                        &node.workflow,
                        Some(format!(
                            "Sub-workflow '{}' completed successfully",
                            node.workflow
                        )),
                        Some(result.total_cost),
                        Some(result.total_turns),
                        Some(result.total_duration_ms),
                        markers,
                        context,
                        Some(result.workflow_run_id.clone()),
                        iteration,
                        None,
                    );

                    // Bubble up child step results so parent can reference internal
                    // sub-workflow markers (e.g. review-aggregator.has_review_issues).
                    let child_steps =
                        bubble_up_child_step_results(&state.wf_mgr, &result.workflow_run_id);
                    for (key, value) in child_steps {
                        state.step_results.entry(key).or_insert(value);
                    }

                    return Ok(());
                } else {
                    let msg = format!("Sub-workflow '{}' failed", node.workflow);
                    tracing::warn!("{} (attempt {}/{})", msg, attempt + 1, max_attempts,);
                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Failed,
                        None,
                        Some(&msg),
                        None,
                        None,
                        Some(attempt as i64),
                    )?;
                    last_error = msg;
                    continue;
                }
            }
            Err(e) => {
                let msg = format!("Sub-workflow '{}' error: {e}", node.workflow);
                tracing::warn!("{} (attempt {}/{})", msg, attempt + 1, max_attempts,);
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    None,
                    Some(&msg),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = msg;
                continue;
            }
        }
    }

    // All retries exhausted — run on_fail agent if specified
    if let Some(ref on_fail_agent) = node.on_fail {
        run_on_fail_agent(
            state,
            &node.workflow,
            on_fail_agent,
            &last_error,
            node.retries,
            iteration,
        );
    }

    record_step_failure(state, step_key, &node.workflow, last_error, max_attempts)
}

/// Fetch the final step's markers and context from a completed child workflow run.
fn fetch_child_final_output(
    wf_mgr: &WorkflowManager<'_>,
    workflow_run_id: &str,
) -> (Vec<String>, String) {
    let steps = match wf_mgr.get_workflow_steps(workflow_run_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch steps for child workflow run '{}': {e}",
                workflow_run_id,
            );
            return (Vec::new(), String::new());
        }
    };

    // Find the last completed step (by position descending)
    let last_completed = steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .max_by_key(|s| s.position);

    match last_completed {
        Some(step) => {
            let markers: Vec<String> = step
                .markers_out
                .as_deref()
                .map(|m| {
                    serde_json::from_str(m).unwrap_or_else(|e| {
                        tracing::warn!(
                            "Malformed markers_out JSON in step '{}': {e}",
                            step.step_name,
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let context = step.context_out.clone().unwrap_or_default();
            (markers, context)
        }
        None => (Vec::new(), String::new()),
    }
}

/// Fetch all completed child steps and build minimal `StepResult` objects for
/// merging into the parent's `step_results` map.
fn bubble_up_child_step_results(
    wf_mgr: &WorkflowManager<'_>,
    workflow_run_id: &str,
) -> HashMap<String, StepResult> {
    let steps = match wf_mgr.get_workflow_steps(workflow_run_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch steps for child workflow run '{}' during bubble-up: {e}",
                workflow_run_id,
            );
            return HashMap::new();
        }
    };

    steps
        .into_iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| {
            let markers: Vec<String> = s
                .markers_out
                .as_deref()
                .map(|m| {
                    serde_json::from_str(m).unwrap_or_else(|e| {
                        tracing::warn!(
                            "Malformed markers_out JSON in child step '{}': {e}",
                            s.step_name,
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let context = s.context_out.clone().unwrap_or_default();
            let result = StepResult {
                step_name: s.step_name.clone(),
                status: WorkflowStepStatus::Completed,
                result_text: s.result_text.clone(),
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers,
                context,
                child_run_id: s.child_run_id.clone(),
                structured_output: s.structured_output.clone(),
            };
            (s.step_name, result)
        })
        .collect()
}

fn execute_if(state: &mut ExecutionState<'_>, node: &IfNode) -> Result<()> {
    let has_marker = state
        .step_results
        .get(&node.step)
        .map(|r| r.markers.iter().any(|m| m == &node.marker))
        .unwrap_or(false);

    if has_marker {
        tracing::info!(
            "if {}.{} — condition met, executing body",
            node.step,
            node.marker
        );
        execute_nodes(state, &node.body)?;
    } else {
        tracing::info!(
            "if {}.{} — condition not met, skipping",
            node.step,
            node.marker
        );
    }

    Ok(())
}

fn execute_unless(state: &mut ExecutionState<'_>, node: &UnlessNode) -> Result<()> {
    let has_marker = state
        .step_results
        .get(&node.step)
        .map(|r| r.markers.iter().any(|m| m == &node.marker))
        .unwrap_or(false);

    if !has_marker {
        tracing::info!(
            "unless {}.{} — marker absent, executing body",
            node.step,
            node.marker
        );
        execute_nodes(state, &node.body)?;
    } else {
        tracing::info!(
            "unless {}.{} — marker present, skipping",
            node.step,
            node.marker
        );
    }

    Ok(())
}

/// Check whether the loop is stuck (identical marker sets for `stuck_after` consecutive
/// iterations). Returns `Err` if stuck, `Ok(())` otherwise.
fn check_stuck(
    state: &mut ExecutionState<'_>,
    prev_marker_sets: &mut Vec<HashSet<String>>,
    step: &str,
    marker: &str,
    stuck_after: u32,
    loop_kind: &str,
) -> Result<()> {
    let current_markers: HashSet<String> = state
        .step_results
        .get(step)
        .map(|r| r.markers.iter().cloned().collect())
        .unwrap_or_default();

    prev_marker_sets.push(current_markers.clone());

    if prev_marker_sets.len() >= stuck_after as usize {
        let window = &prev_marker_sets[prev_marker_sets.len() - stuck_after as usize..];
        if window.iter().all(|s| s == &current_markers) {
            tracing::warn!(
                "{loop_kind} {step}.{marker} — stuck: identical markers for {stuck_after} consecutive iterations",
            );
            state.all_succeeded = false;
            return Err(ConductorError::Workflow(format!(
                "{loop_kind} {step}.{marker} stuck after {stuck_after} iterations with identical markers",
            )));
        }
    }

    Ok(())
}

/// Check whether the loop has exceeded `max_iterations`. Returns `Ok(true)` if the caller
/// should break out of the loop (`on_max_iter = continue`), `Ok(false)` to keep going,
/// or `Err` if `on_max_iter = fail`.
fn check_max_iterations(
    state: &mut ExecutionState<'_>,
    iteration: u32,
    max_iterations: u32,
    on_max_iter: &OnMaxIter,
    step: &str,
    marker: &str,
    loop_kind: &str,
) -> Result<bool> {
    if iteration >= max_iterations {
        tracing::warn!("{loop_kind} {step}.{marker} — reached max_iterations ({max_iterations})",);
        match on_max_iter {
            OnMaxIter::Fail => {
                state.all_succeeded = false;
                return Err(ConductorError::Workflow(format!(
                    "{loop_kind} {step}.{marker} reached max_iterations ({max_iterations})",
                )));
            }
            OnMaxIter::Continue => return Ok(true),
        }
    }
    Ok(false)
}

fn execute_while(state: &mut ExecutionState<'_>, node: &WhileNode) -> Result<()> {
    // On resume, determine the last completed iteration so we can fast-forward
    let start_iteration = if state.resume_ctx.is_some() {
        find_max_completed_while_iteration(state, node)
    } else {
        0u32
    };
    let mut iteration = start_iteration;
    let mut prev_marker_sets: Vec<HashSet<String>> = Vec::new();

    loop {
        // Check condition
        let has_marker = state
            .step_results
            .get(&node.step)
            .map(|r| r.markers.iter().any(|m| m == &node.marker))
            .unwrap_or(false);

        if !has_marker {
            tracing::info!(
                "while {}.{} — condition no longer met after {} iterations",
                node.step,
                node.marker,
                iteration
            );
            break;
        }

        if check_max_iterations(
            state,
            iteration,
            node.max_iterations,
            &node.on_max_iter,
            &node.step,
            &node.marker,
            "while",
        )? {
            break;
        }

        tracing::info!(
            "while {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body
        for body_node in &node.body {
            execute_single_node(state, body_node, iteration)?;

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            check_stuck(
                state,
                &mut prev_marker_sets,
                &node.step,
                &node.marker,
                stuck_after,
                "while",
            )?;
        }

        iteration += 1;
    }

    Ok(())
}

fn execute_do_while(state: &mut ExecutionState<'_>, node: &DoWhileNode) -> Result<()> {
    let mut iteration = 0u32;
    let mut prev_marker_sets: Vec<HashSet<String>> = Vec::new();

    loop {
        if check_max_iterations(
            state,
            iteration,
            node.max_iterations,
            &node.on_max_iter,
            &node.step,
            &node.marker,
            "do",
        )? {
            break;
        }

        tracing::info!(
            "do {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body first (do-while: body always runs before condition check)
        for body_node in &node.body {
            execute_single_node(state, body_node, iteration)?;

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Check condition after body
        let has_marker = state
            .step_results
            .get(&node.step)
            .map(|r| r.markers.iter().any(|m| m == &node.marker))
            .unwrap_or(false);

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            check_stuck(
                state,
                &mut prev_marker_sets,
                &node.step,
                &node.marker,
                stuck_after,
                "do",
            )?;
        }

        if !has_marker {
            tracing::info!(
                "do {}.{} — condition no longer met after {} iterations",
                node.step,
                node.marker,
                iteration + 1
            );
            break;
        }

        iteration += 1;
    }

    Ok(())
}

fn execute_do(state: &mut ExecutionState<'_>, node: &DoNode) -> Result<()> {
    tracing::info!(
        "do block: executing {} body nodes sequentially",
        node.body.len()
    );

    // Save and apply block-level output/with so nested calls can inherit them
    let saved_output = state.block_output.clone();
    let saved_with = state.block_with.clone();

    if node.output.is_some() {
        state.block_output = node.output.clone();
    }
    if !node.with.is_empty() {
        // Prepend block's with to any outer block_with already in state
        let mut combined = node.with.clone();
        combined.extend(saved_with.iter().cloned());
        state.block_with = combined;
    }

    for body_node in &node.body {
        if let Err(e) = execute_single_node(state, body_node, 0) {
            // Restore block-level context before propagating so that
            // always-blocks and subsequent nodes don't inherit do-block state.
            state.block_output = saved_output;
            state.block_with = saved_with;
            return Err(e);
        }
        if !state.all_succeeded && state.exec_config.fail_fast {
            break;
        }
    }

    // Restore block-level context
    state.block_output = saved_output;
    state.block_with = saved_with;

    Ok(())
}

fn execute_parallel(
    state: &mut ExecutionState<'_>,
    node: &ParallelNode,
    iteration: u32,
) -> Result<()> {
    let group_id = ulid::Ulid::new().to_string();
    let pos_base = state.position;

    tracing::info!(
        "parallel: spawning {} agents (fail_fast={}, min_success={:?})",
        node.calls.len(),
        node.fail_fast,
        node.min_success,
    );

    // Load block-level schema (if any)
    let block_schema = node
        .output
        .as_deref()
        .map(|name| resolve_schema(state, name))
        .transpose()?;

    // Spawn all agents
    struct ParallelChild {
        agent_name: String,
        child_run_id: String,
        step_id: String,
        window_name: String,
        /// Resolved schema for this child (computed at spawn time).
        schema: Option<schema_config::OutputSchema>,
    }

    let mut children = Vec::new();
    let mut skipped_count = 0u32;

    for (i, agent_ref) in node.calls.iter().enumerate() {
        let pos = pos_base + i as i64;
        state.position = pos + 1;
        let agent_label = agent_ref.label();

        // Skip completed agents on resume
        let agent_step_key = agent_ref.step_key();
        if should_skip(state, &agent_step_key, iteration) {
            tracing::info!("parallel: skipping completed agent '{}'", agent_label);
            restore_step(state, &agent_step_key, iteration);
            skipped_count += 1;
            continue;
        }

        let agent_def = agent_config::load_agent(
            &state.working_dir,
            &state.repo_path,
            &AgentSpec::from(agent_ref),
            Some(&state.workflow_name),
        )?;

        // Determine schema for this call: per-call override > block-level
        let call_schema = node
            .call_outputs
            .get(&i)
            .map(|name| resolve_schema(state, name))
            .transpose()?;
        let effective_schema = call_schema.as_ref().or(block_schema.as_ref());

        // Combine block-level `with` + per-call `with` additions
        let mut effective_with = node.with.clone();
        if let Some(extra) = node.call_with.get(&i) {
            effective_with.extend(extra.iter().cloned());
        }

        let snippet_text = prompt_config::load_and_concat_snippets(
            &state.working_dir,
            &state.repo_path,
            &effective_with,
            Some(&state.workflow_name),
        )?;

        let prompt = build_agent_prompt(state, &agent_def, effective_schema, &snippet_text);
        let step_model = agent_def.model.as_deref().or(state.model.as_deref());
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            agent_label,
            &agent_def.role.to_string(),
            agent_def.can_commit,
            pos,
            iteration as i64,
        )?;
        state.wf_mgr.set_step_parallel_group(&step_id, &group_id)?;

        let window_prefix = if state.worktree_slug.is_empty() {
            state
                .workflow_run_id
                .get(..8)
                .unwrap_or(&state.workflow_run_id)
        } else {
            state.worktree_slug.as_str()
        };
        let window_name =
            sanitize_tmux_name(&format!("{}-wf-{}-{}", window_prefix, agent_label, i));
        let child_run = state.agent_mgr.create_child_run(
            state.worktree_id.as_deref(),
            &prompt,
            Some(&window_name),
            step_model,
            &state.parent_run_id,
        )?;

        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Running,
            Some(&child_run.id),
            None,
            None,
            None,
            None,
        )?;

        if let Err(e) = agent_runtime::spawn_child_tmux(
            &child_run.id,
            &state.working_dir,
            &prompt,
            step_model,
            &window_name,
        ) {
            tracing::warn!("Failed to spawn parallel agent '{agent_label}': {e}");
            let _ = state
                .agent_mgr
                .update_run_failed(&child_run.id, &format!("spawn failed: {e}"));
            state.wf_mgr.update_step_status(
                &step_id,
                WorkflowStepStatus::Failed,
                Some(&child_run.id),
                Some(&format!("spawn failed: {e}")),
                None,
                None,
                None,
            )?;
            continue;
        }

        children.push(ParallelChild {
            agent_name: agent_label.to_string(),
            child_run_id: child_run.id,
            step_id,
            window_name,
            schema: call_schema.or_else(|| block_schema.clone()),
        });
    }

    // Poll all children until completion
    let start = std::time::Instant::now();
    let mut completed: HashSet<usize> = HashSet::new();
    let mut successes = 0u32;
    let mut failures = 0u32;
    let mut merged_markers: Vec<String> = Vec::new();

    loop {
        if completed.len() == children.len() {
            break;
        }
        if start.elapsed() > state.exec_config.step_timeout {
            tracing::warn!("parallel: timeout reached");
            // Cancel remaining
            for (i, child) in children.iter().enumerate() {
                if !completed.contains(&i) {
                    if let Err(e) = state.agent_mgr.update_run_cancelled(&child.child_run_id) {
                        tracing::warn!(
                            "parallel: failed to cancel run for '{}': {e}",
                            child.agent_name
                        );
                    }
                    let _ = Command::new("tmux")
                        .args(["kill-window", "-t", &format!(":{}", child.window_name)])
                        .output();
                    if let Err(e) = state.wf_mgr.update_step_status(
                        &child.step_id,
                        WorkflowStepStatus::Failed,
                        Some(&child.child_run_id),
                        Some("timed out"),
                        None,
                        None,
                        None,
                    ) {
                        tracing::warn!(
                            "parallel: failed to update timed-out step for '{}': {e}",
                            child.agent_name
                        );
                    }
                    failures += 1;
                    completed.insert(i);
                }
            }
            break;
        }

        for (i, child) in children.iter().enumerate() {
            if completed.contains(&i) {
                continue;
            }
            if let Ok(Some(run)) = state.agent_mgr.get_run(&child.child_run_id) {
                match run.status {
                    AgentRunStatus::Completed
                    | AgentRunStatus::Failed
                    | AgentRunStatus::Cancelled => {
                        completed.insert(i);
                        let succeeded = run.status == AgentRunStatus::Completed;

                        // In parallel blocks, schema validation failures fall back
                        // to generic parsing (no retry mechanism for individual calls).
                        let (markers, context, structured_json) = interpret_agent_output(
                            run.result_text.as_deref(),
                            child.schema.as_ref(),
                            succeeded,
                        )
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "parallel: '{}' schema validation failed, falling back: {e}",
                                child.agent_name
                            );
                            let fb = run
                                .result_text
                                .as_deref()
                                .and_then(parse_conductor_output)
                                .unwrap_or_default();
                            (fb.markers, fb.context, None)
                        });

                        let markers_json = serde_json::to_string(&markers).unwrap_or_default();

                        let step_status = if succeeded {
                            successes += 1;
                            merged_markers.extend(markers.iter().cloned());
                            // Push parallel agent context so downstream {{prior_contexts}} can see it
                            state.contexts.push(ContextEntry {
                                step: child.agent_name.clone(),
                                iteration,
                                context: context.clone(),
                            });
                            WorkflowStepStatus::Completed
                        } else {
                            failures += 1;
                            WorkflowStepStatus::Failed
                        };

                        if let Err(e) = state.wf_mgr.update_step_status_full(
                            &child.step_id,
                            step_status,
                            Some(&child.child_run_id),
                            run.result_text.as_deref(),
                            Some(&context),
                            Some(&markers_json),
                            None,
                            structured_json.as_deref(),
                        ) {
                            tracing::warn!(
                                "parallel: failed to update step status for '{}': {e}",
                                child.agent_name
                            );
                        }

                        if let Some(cost) = run.cost_usd {
                            state.total_cost += cost;
                        }
                        if let Some(turns) = run.num_turns {
                            state.total_turns += turns;
                        }
                        if let Some(dur) = run.duration_ms {
                            state.total_duration_ms += dur;
                        }

                        tracing::info!(
                            "parallel: '{}' {} (cost=${:.4})",
                            child.agent_name,
                            if succeeded { "completed" } else { "failed" },
                            run.cost_usd.unwrap_or(0.0),
                        );

                        // fail_fast: cancel remaining on first failure
                        if !succeeded && node.fail_fast {
                            tracing::warn!("parallel: fail_fast — cancelling remaining");
                            for (j, other) in children.iter().enumerate() {
                                if !completed.contains(&j) {
                                    if let Err(e) =
                                        state.agent_mgr.update_run_cancelled(&other.child_run_id)
                                    {
                                        tracing::warn!(
                                            "parallel: failed to cancel run for '{}': {e}",
                                            other.agent_name
                                        );
                                    }
                                    let _ = Command::new("tmux")
                                        .args([
                                            "kill-window",
                                            "-t",
                                            &format!(":{}", other.window_name),
                                        ])
                                        .output();
                                    if let Err(e) = state.wf_mgr.update_step_status(
                                        &other.step_id,
                                        WorkflowStepStatus::Failed,
                                        Some(&other.child_run_id),
                                        Some("cancelled by fail_fast"),
                                        None,
                                        None,
                                        None,
                                    ) {
                                        tracing::warn!(
                                            "parallel: failed to update step for '{}': {e}",
                                            other.agent_name
                                        );
                                    }
                                    completed.insert(j);
                                    failures += 1;
                                }
                            }
                        }
                    }
                    AgentRunStatus::Running | AgentRunStatus::WaitingForFeedback => {}
                }
            }
        }

        thread::sleep(state.exec_config.poll_interval);
    }

    // Apply min_success policy (skipped-on-resume agents count as successes)
    let effective_successes = successes + skipped_count;
    let total_agents = children.len() as u32 + skipped_count;
    let min_required = node.min_success.unwrap_or(total_agents);
    tracing::info!(
        "parallel: {successes} succeeded, {failures} failed, {skipped_count} skipped (resume) out of {total_agents} agents",
    );
    if effective_successes < min_required {
        tracing::warn!(
            "parallel: only {}/{} succeeded (min_success={})",
            effective_successes,
            total_agents,
            min_required
        );
        state.all_succeeded = false;
    }

    // Store merged markers as a synthetic result
    let synthetic_result = StepResult {
        step_name: format!("parallel:{}", group_id),
        status: if effective_successes >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        },
        result_text: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: merged_markers,
        context: String::new(),
        child_run_id: None,
        structured_output: None,
    };
    state
        .step_results
        .insert(format!("parallel:{}", group_id), synthetic_result);

    Ok(())
}

fn execute_gate(state: &mut ExecutionState<'_>, node: &GateNode, iteration: u32) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    // Skip completed gates on resume — restore feedback for downstream steps
    if should_skip(state, &node.name, iteration) {
        tracing::info!("Skipping completed gate '{}'", node.name);
        restore_step(state, &node.name, iteration);
        return Ok(());
    }

    // Dry-run: auto-approve all gates
    if state.exec_config.dry_run {
        tracing::info!("gate '{}': dry-run auto-approved", node.name);
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &node.name,
            "reviewer",
            false,
            pos,
            iteration as i64,
        )?;
        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("dry-run: auto-approved"),
            None,
            None,
            None,
        )?;
        return Ok(());
    }

    let step_id = state.wf_mgr.insert_step(
        &state.workflow_run_id,
        &node.name,
        "gate",
        false,
        pos,
        iteration as i64,
    )?;

    state.wf_mgr.set_step_gate_info(
        &step_id,
        &node.gate_type.to_string(),
        node.prompt.as_deref(),
        &format!("{}s", node.timeout_secs),
    )?;

    state.wf_mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Waiting,
        None,
        None,
        None,
        None,
        None,
    )?;

    // Update workflow run to waiting status
    state.wf_mgr.update_workflow_status(
        &state.workflow_run_id,
        WorkflowRunStatus::Waiting,
        None,
    )?;

    match node.gate_type {
        GateType::HumanApproval | GateType::HumanReview => {
            tracing::info!("Gate '{}' waiting for human action:", node.name);
            if let Some(ref p) = node.prompt {
                tracing::info!("  Prompt: {p}");
            }
            tracing::info!(
                "  Approve:  conductor workflow gate-approve {}",
                state.workflow_run_id
            );
            tracing::info!(
                "  Reject:   conductor workflow gate-reject {}",
                state.workflow_run_id
            );
            if node.gate_type == GateType::HumanReview {
                tracing::info!(
                    "  Feedback: conductor workflow gate-feedback {} \"<text>\"",
                    state.workflow_run_id
                );
            }

            // Poll DB for approval
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                // Check if gate has been approved/rejected.
                // Use find_waiting_gate as a fast path, fall back to reading the
                // step directly when our gate is no longer the active waiting gate.
                let resolved_step =
                    if let Some(step) = state.wf_mgr.find_waiting_gate(&state.workflow_run_id)? {
                        if step.id == step_id {
                            Some(step)
                        } else {
                            // Another gate is now waiting — ours must have been resolved
                            state.wf_mgr.get_step_by_id(&step_id)?
                        }
                    } else {
                        // No waiting gate — ours must have been resolved
                        state.wf_mgr.get_step_by_id(&step_id)?
                    };

                if let Some(ref step) = resolved_step {
                    if step.gate_approved_at.is_some()
                        || step.status == WorkflowStepStatus::Completed
                    {
                        tracing::info!("Gate '{}' approved", node.name);
                        if let Some(ref feedback) = step.gate_feedback {
                            state.last_gate_feedback = Some(feedback.clone());
                        }
                        state.wf_mgr.update_workflow_status(
                            &state.workflow_run_id,
                            WorkflowRunStatus::Running,
                            None,
                        )?;
                        return Ok(());
                    }
                    if step.status == WorkflowStepStatus::Failed {
                        tracing::warn!("Gate '{}' rejected", node.name);
                        state.all_succeeded = false;
                        state.wf_mgr.update_workflow_status(
                            &state.workflow_run_id,
                            WorkflowRunStatus::Running,
                            None,
                        )?;
                        return Err(ConductorError::Workflow(format!(
                            "Gate '{}' rejected",
                            node.name
                        )));
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::PrApproval => {
            tracing::info!("Gate '{}' polling for PR approvals...", node.name);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                // Poll gh pr view for approvals
                let output = Command::new("gh")
                    .args(["pr", "view", "--json", "reviews"])
                    .current_dir(&state.working_dir)
                    .output();

                if let Ok(out) = output {
                    if out.status.success() {
                        let json_str = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let approvals = val["reviews"]
                                .as_array()
                                .map(|reviews| {
                                    reviews
                                        .iter()
                                        .filter(|r| r["state"].as_str() == Some("APPROVED"))
                                        .count() as u32
                                })
                                .unwrap_or(0);
                            if approvals >= node.min_approvals {
                                tracing::info!(
                                    "Gate '{}': {} approvals (required {})",
                                    node.name,
                                    approvals,
                                    node.min_approvals
                                );
                                state.wf_mgr.approve_gate(&step_id, "gh", None)?;
                                state.wf_mgr.update_workflow_status(
                                    &state.workflow_run_id,
                                    WorkflowRunStatus::Running,
                                    None,
                                )?;
                                return Ok(());
                            }
                        }
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::PrChecks => {
            tracing::info!("Gate '{}' polling for PR checks...", node.name);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                let output = Command::new("gh")
                    .args(["pr", "checks", "--json", "state"])
                    .current_dir(&state.working_dir)
                    .output();

                if let Ok(out) = output {
                    if out.status.success() {
                        let json_str = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            if let Some(checks) = val.as_array() {
                                let all_pass = !checks.is_empty()
                                    && checks.iter().all(|c| {
                                        c["state"].as_str() == Some("SUCCESS")
                                            || c["state"].as_str() == Some("SKIPPED")
                                    });
                                if all_pass {
                                    tracing::info!("Gate '{}': all checks passing", node.name);
                                    state.wf_mgr.approve_gate(&step_id, "gh", None)?;
                                    state.wf_mgr.update_workflow_status(
                                        &state.workflow_run_id,
                                        WorkflowRunStatus::Running,
                                        None,
                                    )?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
    }
}

fn handle_gate_timeout(
    state: &mut ExecutionState<'_>,
    step_id: &str,
    node: &GateNode,
) -> Result<()> {
    tracing::warn!("Gate '{}' timed out", node.name);
    match node.on_timeout {
        OnTimeout::Fail => {
            state.wf_mgr.update_step_status(
                step_id,
                WorkflowStepStatus::Failed,
                None,
                Some("gate timed out"),
                None,
                None,
                None,
            )?;
            state.all_succeeded = false;
            state.wf_mgr.update_workflow_status(
                &state.workflow_run_id,
                WorkflowRunStatus::Running,
                None,
            )?;
            Err(ConductorError::Workflow(format!(
                "Gate '{}' timed out",
                node.name
            )))
        }
        OnTimeout::Continue => {
            state.wf_mgr.update_step_status(
                step_id,
                WorkflowStepStatus::TimedOut,
                None,
                Some("gate timed out (continuing)"),
                None,
                None,
                None,
            )?;
            state.wf_mgr.update_workflow_status(
                &state.workflow_run_id,
                WorkflowRunStatus::Running,
                None,
            )?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_workflow_summary(state: &ExecutionState<'_>) -> String {
    let steps = state
        .wf_mgr
        .get_workflow_steps(&state.workflow_run_id)
        .unwrap_or_default();

    let total = steps.len();
    let count_status =
        |status: WorkflowStepStatus| steps.iter().filter(|s| s.status == status).count();
    let completed = count_status(WorkflowStepStatus::Completed);
    let failed = count_status(WorkflowStepStatus::Failed);
    let skipped = count_status(WorkflowStepStatus::Skipped);
    let timed_out = count_status(WorkflowStepStatus::TimedOut);

    let mut lines = Vec::new();
    lines.push(format!(
        "Workflow '{}': {completed}/{total} steps completed{}{}{}",
        state.workflow_name,
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        },
        if skipped > 0 {
            format!(", {skipped} skipped")
        } else {
            String::new()
        },
        if timed_out > 0 {
            format!(", {timed_out} timed out")
        } else {
            String::new()
        },
    ));

    for step in &steps {
        let marker = step.status.short_label();
        let iter_label = if step.iteration > 0 {
            format!(" (iter {})", step.iteration)
        } else {
            String::new()
        };
        lines.push(format!("  [{marker}] {}{iter_label}", step.step_name));
    }

    if state.all_succeeded {
        lines.push("Status: SUCCESS".to_string());
    } else {
        lines.push("Status: FAILED".to_string());
    }

    lines.join("\n")
}

/// Sanitize a string for use as a tmux window name.
/// Removes characters that tmux treats specially (`.`, `:`, `\`).
fn sanitize_tmux_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '.' | ':' | '\\' | '\'' | '"' => '-',
            c if c.is_ascii_control() => '-',
            _ => c,
        })
        .collect()
}

/// Extract all leaf-node step keys from a workflow node.
///
/// Recurses into control-flow nodes (if/unless/while/always) and collects
/// keys from all trackable leaves: `Call`, `Parallel` agents, `Gate`, and
/// `CallWorkflow`.
fn collect_leaf_step_keys(node: &WorkflowNode) -> Vec<String> {
    match node {
        WorkflowNode::Call(c) => vec![c.agent.step_key()],
        WorkflowNode::Parallel(p) => p.calls.iter().map(|a| a.step_key()).collect(),
        WorkflowNode::Gate(g) => vec![g.name.clone()],
        WorkflowNode::CallWorkflow(cw) => vec![format!("workflow:{}", cw.workflow)],
        WorkflowNode::If(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Unless(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::While(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::DoWhile(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Do(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Always(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
    }
}

/// Find the starting iteration for a while loop on resume.
///
/// Looks at the skip_completed set for step keys that match body nodes of the
/// while loop. Returns the max iteration that has all body nodes completed,
/// so the loop resumes from the iteration where it failed.
fn find_max_completed_while_iteration(state: &ExecutionState<'_>, node: &WhileNode) -> u32 {
    let skip_set = match state.resume_ctx {
        Some(ref ctx) => &ctx.skip_completed,
        None => return 0,
    };

    // Collect step keys from all trackable body nodes
    let body_keys: Vec<String> = node.body.iter().flat_map(collect_leaf_step_keys).collect();

    if body_keys.is_empty() {
        return 0;
    }

    // Find the highest iteration where all body nodes are completed
    let mut iter = 0u32;
    loop {
        let all_done = body_keys
            .iter()
            .all(|k| skip_set.contains(&(k.clone(), iter)));
        if !all_done {
            break;
        }
        iter += 1;
    }
    // iter is now the first incomplete iteration — start there
    iter
}

/// Check whether a step should be skipped on resume.
fn should_skip(state: &ExecutionState<'_>, step_name: &str, iteration: u32) -> bool {
    state.resume_ctx.as_ref().is_some_and(|ctx| {
        ctx.skip_completed
            .contains(&(step_name.to_owned(), iteration))
    })
}

/// Temporarily take the `ResumeContext` out of `state` so we can borrow `state`
/// mutably while reading from the context's maps.
fn restore_step(state: &mut ExecutionState<'_>, key: &str, iteration: u32) {
    let ctx = state.resume_ctx.take();
    if let Some(ref ctx) = ctx {
        restore_completed_step(state, ctx, key, iteration);
    }
    state.resume_ctx = ctx;
}

/// Restore a completed step's results from the resume context into the
/// execution state.
///
/// Rebuilds `step_results` and `contexts` for completed steps so that
/// downstream variable substitution (e.g. `{{prior_context}}`) works correctly.
fn restore_completed_step(
    state: &mut ExecutionState<'_>,
    ctx: &ResumeContext,
    step_key: &str,
    iteration: u32,
) {
    let completed_step = ctx.step_map.get(&(step_key.to_owned(), iteration));

    let Some(step) = completed_step else {
        tracing::warn!(
            "resume: step '{step_key}:{iteration}' in skip set but not found in resume context \
             — downstream variable substitution may be incorrect"
        );
        return;
    };

    let markers: Vec<String> = step
        .markers_out
        .as_deref()
        .and_then(|m| {
            serde_json::from_str(m)
                .map_err(|e| {
                    tracing::warn!(
                        "resume: failed to deserialize markers for step '{}': {e}",
                        step_key
                    );
                    e
                })
                .ok()
        })
        .unwrap_or_default();
    let context = step.context_out.clone().unwrap_or_default();

    // Accumulate costs from the pre-loaded child agent run
    if let Some(ref child_run_id) = step.child_run_id {
        if let Some(run) = ctx.child_runs.get(child_run_id) {
            if let Some(cost) = run.cost_usd {
                state.total_cost += cost;
            }
            if let Some(turns) = run.num_turns {
                state.total_turns += turns;
            }
            if let Some(dur) = run.duration_ms {
                state.total_duration_ms += dur;
            }
        } else {
            tracing::warn!(
                "resume: child agent run '{child_run_id}' for step '{step_key}' not found \
                 — cost/turns/duration will be excluded from resumed run totals"
            );
        }
    }

    // Restore gate feedback if this was a gate step
    if let Some(ref feedback) = step.gate_feedback {
        state.last_gate_feedback = Some(feedback.clone());
    }

    if step.structured_output.is_some() {
        state.last_structured_output = step.structured_output.clone();
    }

    let step_result = StepResult {
        step_name: step_key.to_string(),
        status: WorkflowStepStatus::Completed,
        result_text: step.result_text.clone(),
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers,
        context: context.clone(),
        child_run_id: step.child_run_id.clone(),
        structured_output: step.structured_output.clone(),
    };
    state.step_results.insert(step_key.to_string(), step_result);

    state.contexts.push(ContextEntry {
        step: step_key.to_string(),
        iteration,
        context,
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    #[test]
    fn test_create_workflow_run() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run(
                "test-coverage",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                None,
            )
            .unwrap();

        assert_eq!(run.workflow_name, "test-coverage");
        assert_eq!(run.status, WorkflowRunStatus::Pending);
        assert!(!run.dry_run);
    }

    #[test]
    fn test_create_workflow_run_with_snapshot() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run(
                "test",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some(r#"{"name":"test"}"#),
            )
            .unwrap();

        let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.definition_snapshot.as_deref(),
            Some(r#"{"name":"test"}"#)
        );
    }

    #[test]
    fn test_insert_step_with_iteration() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 2)
            .unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, step_id);
        assert_eq!(steps[0].step_name, "review");
        assert_eq!(steps[0].iteration, 2);
    }

    #[test]
    fn test_update_step_with_markers() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 0)
            .unwrap();

        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("Found issues"),
            Some("2 issues in lib.rs"),
            Some(r#"["has_review_issues"]"#),
            Some(0),
        )
        .unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].context_out.as_deref(), Some("2 issues in lib.rs"));
        assert_eq!(
            steps[0].markers_out.as_deref(),
            Some(r#"["has_review_issues"]"#)
        );
    }

    #[test]
    fn test_update_step_status_full_with_structured_output() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 0)
            .unwrap();

        let structured_json = r#"{"approved":true,"summary":"All good"}"#;
        mgr.update_step_status_full(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("result text"),
            Some("All good"),
            Some(r#"[]"#),
            Some(0),
            Some(structured_json),
        )
        .unwrap();

        let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
        assert_eq!(step.structured_output.as_deref(), Some(structured_json));
        assert_eq!(step.context_out.as_deref(), Some("All good"));
        assert_eq!(step.result_text.as_deref(), Some("result text"));
    }

    #[test]
    fn test_update_step_status_full_without_structured_output() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "review", "reviewer", false, 0, 0)
            .unwrap();

        mgr.update_step_status_full(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("result text"),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let step = mgr.get_step_by_id(&step_id).unwrap().unwrap();
        assert!(step.structured_output.is_none());
    }

    #[test]
    fn test_gate_approve() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "human_review", "reviewer", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human_review", Some("Review?"), "48h")
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Waiting,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // Find waiting gate
        let waiting = mgr.find_waiting_gate(&run.id).unwrap();
        assert!(waiting.is_some());
        assert_eq!(waiting.unwrap().id, step_id);

        // Approve
        mgr.approve_gate(&step_id, "user", Some("Looks good!"))
            .unwrap();

        // Verify
        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
        assert!(steps[0].gate_approved_at.is_some());
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("user"));
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("Looks good!"));
    }

    #[test]
    fn test_gate_reject() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = mgr
            .insert_step(&run.id, "human_approval", "reviewer", false, 0, 0)
            .unwrap();
        mgr.set_step_gate_info(&step_id, "human_approval", Some("Approve?"), "24h")
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Waiting,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        mgr.reject_gate(&step_id, "user").unwrap();

        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
    }

    fn make_gate_node(gate_type: GateType, on_timeout: OnTimeout) -> GateNode {
        GateNode {
            name: "test_gate".to_string(),
            gate_type,
            prompt: None,
            min_approvals: 1,
            timeout_secs: 1,
            on_timeout,
        }
    }

    fn make_state_with_run<'a>(
        conn: &'a Connection,
        config: &'static Config,
    ) -> (ExecutionState<'a>, String) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Waiting, None)
            .unwrap();
        let run_id = run.id.clone();
        let state = ExecutionState {
            conn,
            config,
            workflow_run_id: run_id.clone(),
            workflow_name: "test".to_string(),
            worktree_id: Some("w1".to_string()),
            working_dir: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            agent_mgr: AgentManager::new(conn),
            wf_mgr: WorkflowManager::new(conn),
            parent_run_id: parent.id,
            depth: 0,
            step_results: HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            last_structured_output: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
        };
        (state, run_id)
    }

    #[test]
    fn test_gate_timeout_fail() {
        let conn = setup_db();
        let config: &'static Config = Box::leak(Box::new(Config::default()));
        let (mut state, run_id) = make_state_with_run(&conn, config);

        let wf_mgr = WorkflowManager::new(&conn);
        let step_id = wf_mgr
            .insert_step(&run_id, "test_gate", "gate", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Waiting,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let node = make_gate_node(GateType::HumanApproval, OnTimeout::Fail);
        let result = handle_gate_timeout(&mut state, &step_id, &node);

        assert!(result.is_err());
        let steps = wf_mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_gate_timeout_continue() {
        let conn = setup_db();
        let config: &'static Config = Box::leak(Box::new(Config::default()));
        let (mut state, run_id) = make_state_with_run(&conn, config);

        let wf_mgr = WorkflowManager::new(&conn);
        let step_id = wf_mgr
            .insert_step(&run_id, "test_gate", "gate", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Waiting,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let node = make_gate_node(GateType::HumanApproval, OnTimeout::Continue);
        let result = handle_gate_timeout(&mut state, &step_id, &node);

        assert!(result.is_ok(), "on_timeout=continue should return Ok");
        let steps = wf_mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::TimedOut);
        assert!(
            state.all_succeeded,
            "on_timeout=continue should not set all_succeeded=false"
        );
    }

    #[test]
    fn test_parse_conductor_output() {
        let text = r#"Here is my analysis...

<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_review_issues", "has_critical_issues"], "context": "Found 2 issues in src/lib.rs"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let output = parse_conductor_output(text).unwrap();
        assert_eq!(
            output.markers,
            vec!["has_review_issues", "has_critical_issues"]
        );
        assert_eq!(output.context, "Found 2 issues in src/lib.rs");
    }

    #[test]
    fn test_parse_conductor_output_missing() {
        assert!(parse_conductor_output("no output block here").is_none());
    }

    #[test]
    fn test_parse_conductor_output_no_markers() {
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [], \"context\": \"All good\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let output = parse_conductor_output(text).unwrap();
        assert!(output.markers.is_empty());
        assert_eq!(output.context, "All good");
    }

    #[test]
    fn test_parse_conductor_output_last_occurrence() {
        // Should find the LAST occurrence (the real one), not a false positive in a code block
        let text = r#"Here's an example of the output format:
```
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["fake"], "context": "This is a code example"}
<<<END_CONDUCTOR_OUTPUT>>>
```

And here is my actual output:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["real"], "context": "This is the real output"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let output = parse_conductor_output(text).unwrap();
        assert_eq!(output.markers, vec!["real"]);
        assert_eq!(output.context, "This is the real output");
    }

    #[test]
    fn test_substitute_variables() {
        let mut vars = HashMap::new();
        vars.insert("ticket_id", "FEAT-123".to_string());
        vars.insert("prior_context", "Created PLAN.md".to_string());

        let prompt = "Fix ticket {{ticket_id}}. Context: {{prior_context}}. Unknown: {{unknown}}.";
        let result = substitute_variables(prompt, &vars);
        assert_eq!(
            result,
            "Fix ticket FEAT-123. Context: Created PLAN.md. Unknown: {{unknown}}."
        );
    }

    #[test]
    fn test_workflow_run_status_roundtrip() {
        for status in [
            WorkflowRunStatus::Pending,
            WorkflowRunStatus::Running,
            WorkflowRunStatus::Completed,
            WorkflowRunStatus::Failed,
            WorkflowRunStatus::Cancelled,
            WorkflowRunStatus::Waiting,
        ] {
            let s = status.to_string();
            let parsed: WorkflowRunStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_workflow_step_status_roundtrip() {
        for status in [
            WorkflowStepStatus::Pending,
            WorkflowStepStatus::Running,
            WorkflowStepStatus::Completed,
            WorkflowStepStatus::Failed,
            WorkflowStepStatus::Skipped,
            WorkflowStepStatus::Waiting,
        ] {
            let s = status.to_string();
            let parsed: WorkflowStepStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_poll_child_completion_already_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        mgr.update_run_completed(&run.id, None, Some("done"), Some(0.05), Some(3), Some(5000))
            .unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, AgentRunStatus::Completed);
    }

    #[test]
    fn test_poll_child_completion_timeout() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_millis(50),
            None,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_runtime::PollError::Timeout(_)
        ));
    }

    #[test]
    fn test_poll_child_completion_shutdown() {
        use std::sync::{atomic::AtomicBool, Arc};

        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        // run stays in Running; flag is already set
        let flag = Arc::new(AtomicBool::new(true));

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(5),
            Some(&flag),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_runtime::PollError::Shutdown
        ));
    }

    #[test]
    fn test_recover_stuck_steps_syncs_completed() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let wf_mgr = WorkflowManager::new(&conn);

        // Create a parent agent run and a workflow run
        let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let wf_run = wf_mgr
            .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert a step stuck in 'running' with a child_run_id
        let step_id = wf_mgr
            .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
            .unwrap();
        let child = agent_mgr
            .create_run(Some("w1"), "child-agent", None, None)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Running,
                Some(&child.id),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // Mark child run as completed
        agent_mgr
            .update_run_completed(&child.id, None, Some("great output"), None, None, None)
            .unwrap();

        let recovered = wf_mgr.recover_stuck_steps().unwrap();
        assert_eq!(recovered, 1);

        let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
        assert_eq!(steps[0].result_text.as_deref(), Some("great output"));
    }

    #[test]
    fn test_recover_stuck_steps_skips_still_running() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let wf_mgr = WorkflowManager::new(&conn);

        let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let wf_run = wf_mgr
            .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = wf_mgr
            .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
            .unwrap();
        let child = agent_mgr
            .create_run(Some("w1"), "child-agent", None, None)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Running,
                Some(&child.id),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        // child run stays in 'running' — should NOT be recovered

        let recovered = wf_mgr.recover_stuck_steps().unwrap();
        assert_eq!(recovered, 0);

        let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Running);
    }

    #[test]
    fn test_recover_stuck_steps_failed_child_marks_step_failed() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let wf_mgr = WorkflowManager::new(&conn);

        let parent = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let wf_run = wf_mgr
            .create_workflow_run("flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = wf_mgr
            .insert_step(&wf_run.id, "agent-step", "actor", false, 0, 0)
            .unwrap();
        let child = agent_mgr
            .create_run(Some("w1"), "child-agent", None, None)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step_id,
                WorkflowStepStatus::Running,
                Some(&child.id),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        agent_mgr
            .update_run_failed(&child.id, "agent crashed")
            .unwrap();

        let recovered = wf_mgr.recover_stuck_steps().unwrap();
        assert_eq!(recovered, 1);

        let steps = wf_mgr.get_workflow_steps(&wf_run.id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Failed);
        assert_eq!(steps[0].result_text.as_deref(), Some("agent crashed"));
    }

    #[test]
    fn test_list_workflow_runs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w1"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("test-a", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("test-b", Some("w1"), &p2.id, true, "pr", None)
            .unwrap();

        let runs = mgr.list_workflow_runs("w1").unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn test_list_all_workflow_runs_cross_worktree() {
        let conn = setup_db();
        // Insert a second worktree so we can test cross-worktree aggregation.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("flow-a", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("flow-b", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        // list_all returns both runs regardless of worktree
        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 2);
        let names: Vec<&str> = all.iter().map(|r| r.workflow_name.as_str()).collect();
        assert!(names.contains(&"flow-a"));
        assert!(names.contains(&"flow-b"));
    }

    #[test]
    fn test_list_all_workflow_runs_respects_limit() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);

        let mgr = WorkflowManager::new(&conn);
        for i in 0..5 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(
                &format!("flow-{i}"),
                Some("w1"),
                &p.id,
                false,
                "manual",
                None,
            )
            .unwrap();
        }

        let limited = mgr.list_all_workflow_runs(3).unwrap();
        assert_eq!(limited.len(), 3);
    }

    #[test]
    fn test_list_all_workflow_runs_empty() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let runs = mgr.list_all_workflow_runs(50).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn test_list_all_workflow_runs_includes_ephemeral() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        // Create a normal run (with worktree)
        let parent1 = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        mgr.create_workflow_run("normal-wf", Some("w1"), &parent1.id, false, "manual", None)
            .unwrap();

        // Create an ephemeral run (no worktree)
        let parent2 = agent_mgr
            .create_run(None, "ephemeral workflow", None, None)
            .unwrap();
        let ephemeral = mgr
            .create_workflow_run("ephemeral-wf", None, &parent2.id, false, "manual", None)
            .unwrap();

        let all = mgr.list_all_workflow_runs(100).unwrap();
        assert_eq!(all.len(), 2);

        // Verify the ephemeral run has None worktree_id
        let found = all.iter().find(|r| r.id == ephemeral.id).unwrap();
        assert!(found.worktree_id.is_none());
    }

    #[test]
    fn test_list_workflow_runs_for_scope_scoped() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run(Some("w1"), "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run(Some("w2"), "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("only-w1", Some("w1"), &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("only-w2", Some("w2"), &p2.id, false, "manual", None)
            .unwrap();

        // Scoped: only w1's run
        let scoped = mgr.list_workflow_runs_for_scope(Some("w1"), 50).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].workflow_name, "only-w1");

        // Global: both runs
        let global = mgr.list_workflow_runs_for_scope(None, 50).unwrap();
        assert_eq!(global.len(), 2);
    }

    #[test]
    fn test_list_workflow_runs_for_scope_global_limit() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);
        for i in 0..5 {
            let p = agent_mgr
                .create_run(Some("w1"), &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(
                &format!("flow-{i}"),
                Some("w1"),
                &p.id,
                false,
                "manual",
                None,
            )
            .unwrap();
        }
        let limited = mgr.list_workflow_runs_for_scope(None, 2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn test_get_workflow_run_not_found() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let result = mgr.get_workflow_run("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_step_by_id() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "build", "actor", false, 0, 0)
            .unwrap();

        let step = mgr.get_step_by_id(&step_id).unwrap();
        assert!(step.is_some());
        let step = step.unwrap();
        assert_eq!(step.id, step_id);
        assert_eq!(step.step_name, "build");
        assert_eq!(step.role, "actor");

        let missing = mgr.get_step_by_id("nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_metadata_fields_basic() {
        let step = WorkflowRunStep {
            id: "s1".into(),
            workflow_run_id: "r1".into(),
            step_name: "lint".into(),
            role: "reviewer".into(),
            can_commit: false,
            condition_expr: None,
            status: WorkflowStepStatus::Completed,
            child_run_id: None,
            position: 1,
            started_at: Some("2025-01-01T00:00:00Z".into()),
            ended_at: Some("2025-01-01T00:01:00Z".into()),
            result_text: None,
            condition_met: None,
            iteration: 1,
            parallel_group_id: None,
            context_out: None,
            markers_out: None,
            retry_count: 0,
            gate_type: None,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: None,
        };
        let entries = step.metadata_fields();
        assert_eq!(entries.len(), 6); // 4 always-present + Started + Ended
        assert_eq!(
            entries[0],
            MetadataEntry::Field {
                label: "Status",
                value: "completed".into()
            }
        );
        assert_eq!(
            entries[1],
            MetadataEntry::Field {
                label: "Role",
                value: "reviewer".into()
            }
        );
        assert_eq!(
            entries[2],
            MetadataEntry::Field {
                label: "Can commit",
                value: "false".into()
            }
        );
        assert_eq!(
            entries[3],
            MetadataEntry::Field {
                label: "Iteration",
                value: "1".into()
            }
        );
        assert_eq!(
            entries[4],
            MetadataEntry::Field {
                label: "Started",
                value: "2025-01-01T00:00:00Z".into()
            }
        );
        assert_eq!(
            entries[5],
            MetadataEntry::Field {
                label: "Ended",
                value: "2025-01-01T00:01:00Z".into()
            }
        );
        // No gate or section entries
        assert!(!entries
            .iter()
            .any(|e| matches!(e, MetadataEntry::Section { .. })));
    }

    #[test]
    fn test_metadata_fields_optional_sections() {
        let step = WorkflowRunStep {
            id: "s2".into(),
            workflow_run_id: "r1".into(),
            step_name: "review".into(),
            role: "reviewer".into(),
            can_commit: false,
            condition_expr: None,
            status: WorkflowStepStatus::Running,
            child_run_id: None,
            position: 2,
            started_at: None,
            ended_at: None,
            result_text: Some("All good".into()),
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: Some("ctx data".into()),
            markers_out: Some("marker1".into()),
            retry_count: 0,
            gate_type: Some("approval".into()),
            gate_prompt: Some("Please approve".into()),
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: Some("Looks good".into()),
            structured_output: None,
        };
        let entries = step.metadata_fields();
        assert!(entries.contains(&MetadataEntry::Field {
            label: "Gate type",
            value: "approval".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Gate Prompt",
            body: "Please approve".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Gate Feedback",
            body: "Looks good".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Result",
            body: "All good".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Context Out",
            body: "ctx data".into()
        }));
        assert!(entries.contains(&MetadataEntry::Section {
            heading: "Markers Out",
            body: "marker1".into()
        }));
    }

    // -----------------------------------------------------------------------
    // fetch_child_final_output tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_fetch_child_final_output_returns_last_completed_step() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert two completed steps; the second (position=1) should be returned
        let step1_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step1_id,
            WorkflowStepStatus::Completed,
            None,
            Some("step-a done"),
            Some("context-a"),
            Some(r#"["marker_a"]"#),
            Some(0),
        )
        .unwrap();

        let step2_id = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        mgr.update_step_status(
            &step2_id,
            WorkflowStepStatus::Completed,
            None,
            Some("step-b done"),
            Some("context-b"),
            Some(r#"["marker_b1","marker_b2"]"#),
            Some(0),
        )
        .unwrap();

        let (markers, context) = fetch_child_final_output(&mgr, &run.id);
        assert_eq!(markers, vec!["marker_b1", "marker_b2"]);
        assert_eq!(context, "context-b");
    }

    #[test]
    fn test_fetch_child_final_output_no_completed_steps() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert a failed step only
        let step_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Failed,
            None,
            Some("failed"),
            None,
            None,
            Some(0),
        )
        .unwrap();

        let (markers, context) = fetch_child_final_output(&mgr, &run.id);
        assert!(markers.is_empty());
        assert!(context.is_empty());
    }

    #[test]
    fn test_fetch_child_final_output_malformed_markers_json() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        let step_id = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("done"),
            Some("some context"),
            Some("not valid json {{{"),
            Some(0),
        )
        .unwrap();

        let (markers, context) = fetch_child_final_output(&mgr, &run.id);
        assert!(markers.is_empty()); // malformed JSON falls back to empty
        assert_eq!(context, "some context");
    }

    #[test]
    fn test_fetch_child_final_output_nonexistent_run() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let (markers, context) = fetch_child_final_output(&mgr, "nonexistent-run-id");
        assert!(markers.is_empty());
        assert!(context.is_empty());
    }

    // -----------------------------------------------------------------------
    // build_variable_map tests
    // -----------------------------------------------------------------------

    /// Helper to create a minimal ExecutionState for testing build_variable_map.
    fn make_test_state(conn: &Connection) -> ExecutionState<'_> {
        let config = Config::default();
        // We need a config that lives long enough — use a leaked Box for test simplicity.
        let config: &'static Config = Box::leak(Box::new(config));
        ExecutionState {
            conn,
            config,
            workflow_run_id: String::new(),
            workflow_name: String::new(),
            worktree_id: None,
            working_dir: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            agent_mgr: AgentManager::new(conn),
            wf_mgr: WorkflowManager::new(conn),
            parent_run_id: String::new(),
            depth: 0,
            step_results: HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            last_structured_output: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
        }
    }

    #[test]
    fn test_build_variable_map_includes_inputs_and_prior_context() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);
        state
            .inputs
            .insert("branch".to_string(), "main".to_string());
        state.contexts.push(ContextEntry {
            step: "step-a".to_string(),
            iteration: 0,
            context: "previous output".to_string(),
        });

        let vars = build_variable_map(&state);
        assert_eq!(vars.get("branch").unwrap(), "main");
        assert_eq!(vars.get("prior_context").unwrap(), "previous output");
        assert!(vars.get("prior_contexts").unwrap().contains("step-a"));
    }

    #[test]
    fn test_parallel_contexts_included_in_prior_contexts() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Simulate multiple parallel agents completing and pushing contexts
        // (this is the pattern now used in execute_parallel's success branch)
        state.contexts.push(ContextEntry {
            step: "reviewer-a".to_string(),
            iteration: 0,
            context: "LGTM from reviewer A".to_string(),
        });
        state.contexts.push(ContextEntry {
            step: "reviewer-b".to_string(),
            iteration: 0,
            context: "Needs changes from reviewer B".to_string(),
        });

        let vars = build_variable_map(&state);

        // prior_context should be the last context pushed
        assert_eq!(
            vars.get("prior_context").unwrap(),
            "Needs changes from reviewer B"
        );

        // prior_contexts should contain both parallel agent entries
        let prior_contexts = vars.get("prior_contexts").unwrap();
        assert!(prior_contexts.contains("reviewer-a"));
        assert!(prior_contexts.contains("reviewer-b"));
        assert!(prior_contexts.contains("LGTM from reviewer A"));
        assert!(prior_contexts.contains("Needs changes from reviewer B"));
    }

    #[test]
    fn test_build_variable_map_includes_gate_feedback() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);
        state.last_gate_feedback = Some("looks good".to_string());

        let vars = build_variable_map(&state);
        assert_eq!(vars.get("gate_feedback").unwrap(), "looks good");
    }

    #[test]
    fn test_build_variable_map_no_gate_feedback() {
        let conn = setup_db();
        let state = make_test_state(&conn);
        let vars = build_variable_map(&state);
        assert!(!vars.contains_key("gate_feedback"));
        // prior_context should be empty string when no contexts
        assert_eq!(vars.get("prior_context").unwrap(), "");
        // prior_output should be absent when no structured output
        assert!(!vars.contains_key("prior_output"));
    }

    #[test]
    fn test_build_variable_map_includes_prior_output() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);
        let json = r#"{"approved":true,"summary":"All clear"}"#.to_string();
        state.last_structured_output = Some(json.clone());

        let vars = build_variable_map(&state);
        assert_eq!(vars.get("prior_output").unwrap(), &json);
    }

    // -----------------------------------------------------------------------
    // resolve_child_inputs tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_child_inputs_substitutes_variables() {
        use crate::workflow_dsl::InputDecl;

        let mut raw = HashMap::new();
        raw.insert("msg".to_string(), "Hello {{name}}!".to_string());

        let mut vars: HashMap<&str, String> = HashMap::new();
        vars.insert("name", "World".to_string());

        let decls = vec![InputDecl {
            name: "msg".to_string(),
            required: true,
            default: None,
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("msg").unwrap(), "Hello World!");
    }

    #[test]
    fn test_resolve_child_inputs_applies_defaults() {
        use crate::workflow_dsl::InputDecl;

        let raw = HashMap::new(); // no inputs provided

        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "mode".to_string(),
            required: false,
            default: Some("fast".to_string()),
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("mode").unwrap(), "fast");
    }

    #[test]
    fn test_resolve_child_inputs_missing_required() {
        use crate::workflow_dsl::InputDecl;

        let raw = HashMap::new();
        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "pr_url".to_string(),
            required: true,
            default: None,
        }];

        let err = resolve_child_inputs(&raw, &vars, &decls).unwrap_err();
        assert_eq!(err, "pr_url");
    }

    #[test]
    fn test_resolve_child_inputs_provided_overrides_default() {
        use crate::workflow_dsl::InputDecl;

        let mut raw = HashMap::new();
        raw.insert("mode".to_string(), "slow".to_string());

        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "mode".to_string(),
            required: false,
            default: Some("fast".to_string()),
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert_eq!(result.get("mode").unwrap(), "slow");
    }

    #[test]
    fn test_resolve_child_inputs_optional_without_default_omitted() {
        use crate::workflow_dsl::InputDecl;

        let raw = HashMap::new();
        let vars: HashMap<&str, String> = HashMap::new();
        let decls = vec![InputDecl {
            name: "optional_field".to_string(),
            required: false,
            default: None,
        }];

        let result = resolve_child_inputs(&raw, &vars, &decls).unwrap();
        assert!(!result.contains_key("optional_field"));
    }

    // -----------------------------------------------------------------------
    // execute_unless tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_unless_marker_absent_runs_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Step "build" exists but does NOT have the "has_errors" marker
        state.step_results.insert(
            "build".to_string(),
            StepResult {
                step_name: "build".to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec!["build_ok".to_string()],
                context: String::new(),
                child_run_id: None,
                structured_output: None,
            },
        );

        let node = UnlessNode {
            step: "build".to_string(),
            marker: "has_errors".to_string(),
            body: vec![], // empty body — just verify it enters the branch without error
        };

        // Should succeed (marker absent → body executes, empty body is fine)
        execute_unless(&mut state, &node).unwrap();
    }

    #[test]
    fn test_execute_unless_marker_present_skips_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Step "build" has the "has_errors" marker
        state.step_results.insert(
            "build".to_string(),
            StepResult {
                step_name: "build".to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec!["has_errors".to_string()],
                context: String::new(),
                child_run_id: None,
                structured_output: None,
            },
        );

        let node = UnlessNode {
            step: "build".to_string(),
            marker: "has_errors".to_string(),
            body: vec![], // empty body
        };

        // Should succeed (marker present → body skipped)
        execute_unless(&mut state, &node).unwrap();
    }

    #[test]
    fn test_execute_unless_step_not_found_runs_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // No step results at all — step "build" not in step_results
        let node = UnlessNode {
            step: "build".to_string(),
            marker: "has_errors".to_string(),
            body: vec![], // empty body
        };

        // Should succeed (step not found → unwrap_or(false) → !false → body runs)
        execute_unless(&mut state, &node).unwrap();
    }

    // -----------------------------------------------------------------------
    // interpret_agent_output tests
    // -----------------------------------------------------------------------

    fn make_test_schema() -> OutputSchema {
        schema_config::parse_schema_content(
            "fields:\n  approved: boolean\n  summary: string\n",
            "test",
        )
        .unwrap()
    }

    #[test]
    fn test_interpret_agent_output_schema_valid() {
        let schema = make_test_schema();
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"approved\": true, \"summary\": \"all good\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let (markers, context, json) =
            interpret_agent_output(Some(text), Some(&schema), true).unwrap();
        assert_eq!(context, "all good");
        assert!(json.is_some());
        // approved=true → no not_approved marker
        assert!(!markers.contains(&"not_approved".to_string()));
    }

    #[test]
    fn test_interpret_agent_output_schema_validation_fails_succeeded() {
        let schema = make_test_schema();
        // Missing required field "approved"
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"summary\": \"oops\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let result = interpret_agent_output(Some(text), Some(&schema), true);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("structured output validation"));
    }

    #[test]
    fn test_interpret_agent_output_schema_validation_fails_not_succeeded_falls_back() {
        let schema = make_test_schema();
        // Missing required field — but succeeded=false so it falls back
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"summary\": \"oops\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let (markers, context, json) =
            interpret_agent_output(Some(text), Some(&schema), false).unwrap();
        // Falls back to generic parse_conductor_output which doesn't find markers/context
        assert!(json.is_none());
        assert!(markers.is_empty());
        assert!(context.is_empty());
    }

    #[test]
    fn test_interpret_agent_output_no_schema_generic_parsing() {
        let text = "<<<CONDUCTOR_OUTPUT>>>\n{\"markers\": [\"done\"], \"context\": \"finished\"}\n<<<END_CONDUCTOR_OUTPUT>>>";
        let (markers, context, json) = interpret_agent_output(Some(text), None, true).unwrap();
        assert_eq!(markers, vec!["done"]);
        assert_eq!(context, "finished");
        assert!(json.is_none());
    }

    #[test]
    fn test_interpret_agent_output_no_text() {
        let schema = make_test_schema();
        // result_text is None with schema — falls back
        let (markers, context, json) = interpret_agent_output(None, Some(&schema), false).unwrap();
        assert!(markers.is_empty());
        assert!(context.is_empty());
        assert!(json.is_none());
    }

    // -----------------------------------------------------------------------
    // execute_do_while tests
    // -----------------------------------------------------------------------

    fn make_step_result(step_name: &str, markers: Vec<&str>) -> StepResult {
        StepResult {
            step_name: step_name.into(),
            status: WorkflowStepStatus::Completed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            markers: markers.into_iter().map(String::from).collect(),
            context: String::new(),
            child_run_id: None,
            structured_output: None,
        }
    }

    /// Helper to build an `ExecutionState` suitable for testing loop functions
    /// (no real agents or worktrees needed).
    fn make_loop_test_state<'a>(conn: &'a Connection, config: &'a Config) -> ExecutionState<'a> {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        ExecutionState {
            conn,
            config,
            workflow_run_id: run.id,
            workflow_name: "test".into(),
            worktree_id: Some("w1".into()),
            working_dir: "/tmp/test".into(),
            worktree_slug: "test".into(),
            repo_path: "/tmp/repo".into(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            agent_mgr: AgentManager::new(conn),
            wf_mgr: WorkflowManager::new(conn),
            parent_run_id: parent.id,
            depth: 0,
            step_results: HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            last_structured_output: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
        }
    }

    #[test]
    fn test_do_while_body_runs_once_when_condition_absent() {
        // The defining semantic: body executes before condition check,
        // so even with no marker set the body runs once.
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 3,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![], // empty body — still runs the loop once
        };

        // No step_results set → marker absent → loop exits after 1 iteration
        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(state.all_succeeded);
    }

    #[test]
    fn test_do_while_max_iterations_fail() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        // Pre-set a marker that stays true forever (body is empty so nothing clears it)
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 2,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_iterations"));
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_do_while_max_iterations_continue() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 2,
            stuck_after: None,
            on_max_iter: OnMaxIter::Continue,
            body: vec![],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(state.all_succeeded);
    }

    #[test]
    fn test_do_while_stuck_detection() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        // Marker stays the same every iteration → stuck after 2
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 10,
            stuck_after: Some(2),
            on_max_iter: OnMaxIter::Fail,
            body: vec![],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("stuck"));
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_do_while_iterates_body_multiple_times() {
        // Verify the body actually executes on each iteration by tracking
        // state.position, which Gate nodes increment in dry_run mode.
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Marker present → loop keeps iterating until max_iterations
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        let initial_position = state.position;

        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 3,
            stuck_after: None,
            on_max_iter: OnMaxIter::Continue,
            body: vec![WorkflowNode::Gate(GateNode {
                name: "counter".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
            })],
        };

        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        // Gate node increments position once per iteration; 3 iterations expected
        assert_eq!(state.position - initial_position, 3);
    }

    // NOTE: Testing the natural-exit path (marker transitions from true→false
    // mid-loop) is not feasible in a unit test because no WorkflowNode type
    // modifies step_results without running a real agent. The `!has_marker → break`
    // branch after body execution IS covered when the marker is absent from the
    // start (see test_do_while_body_runs_once_when_condition_absent). The
    // transition case (marker present → body clears marker → loop exits) requires
    // integration testing with actual agent execution.

    #[test]
    fn test_do_while_fail_fast_exits_early() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.fail_fast = true;

        // Marker is set so the loop would keep iterating if not for fail_fast
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        // Simulate a prior failure — all_succeeded is already false
        state.all_succeeded = false;

        // Body has a no-op If node (condition never true → body skipped, returns Ok)
        let node = DoWhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 10,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![WorkflowNode::If(IfNode {
                step: "nonexistent".into(),
                marker: "nope".into(),
                body: vec![],
            })],
        };

        // fail_fast should cause early exit with Ok(()) instead of looping to max_iterations
        let result = execute_do_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_while_fail_fast_exits_early() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.fail_fast = true;

        // Marker is set so the loop would keep iterating if not for fail_fast
        state.step_results.insert(
            "check".into(),
            make_step_result("check", vec!["needs_work"]),
        );

        // Simulate a prior failure — all_succeeded is already false
        state.all_succeeded = false;

        // Body has a no-op If node (condition never true → body skipped, returns Ok)
        let node = WhileNode {
            step: "check".into(),
            marker: "needs_work".into(),
            max_iterations: 10,
            stuck_after: None,
            on_max_iter: OnMaxIter::Fail,
            body: vec![WorkflowNode::If(IfNode {
                step: "nonexistent".into(),
                marker: "nope".into(),
                body: vec![],
            })],
        };

        // fail_fast should cause early exit with Ok(()) instead of looping to max_iterations
        let result = execute_while(&mut state, &node);
        assert!(result.is_ok());
        assert!(!state.all_succeeded);
    }

    #[test]
    fn test_get_active_run_for_worktree_none_when_empty() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_get_active_run_for_worktree_returns_active() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("my-flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        // Set status to running
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().workflow_name, "my-flow");
    }

    #[test]
    fn test_get_active_run_for_worktree_none_after_completion() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("my-flow", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_get_active_run_for_worktree_ignores_other_worktree() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w2"), "workflow", None, None)
            .unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("other-flow", Some("w2"), &parent.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        // w1 should see no active runs
        let active = mgr.get_active_run_for_worktree("w1").unwrap();
        assert!(active.is_none());
    }

    // -----------------------------------------------------------------------
    // execute_workflow guard tests (depth == 0 only)
    // -----------------------------------------------------------------------

    /// Minimal workflow with no agents or steps — used to exercise the
    /// execute_workflow guard without touching real agent infrastructure.
    fn make_empty_workflow() -> WorkflowDef {
        WorkflowDef {
            name: "test-wf".into(),
            description: "test".into(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: "test.wf".into(),
        }
    }

    #[test]
    fn test_cannot_start_workflow_run_when_active() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("running-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let workflow = make_empty_workflow();
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"),
            working_dir: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let err = execute_workflow(&input).unwrap_err();
        assert!(
            matches!(err, ConductorError::WorkflowRunAlreadyActive { .. }),
            "expected WorkflowRunAlreadyActive, got: {err}"
        );
    }

    #[test]
    fn test_can_start_workflow_run_after_completion() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("done-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let workflow = make_empty_workflow();
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"),
            working_dir: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        // Guard should pass; empty workflow completes successfully.
        let result = execute_workflow(&input);
        assert!(
            !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
            "should not be blocked by completed run"
        );
    }

    #[test]
    fn test_child_workflow_not_blocked_by_parent() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("parent-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let workflow = make_empty_workflow();
        // depth = 1 means this is a child workflow — guard must be skipped.
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: Some("w1"),
            working_dir: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 1,
        };
        let result = execute_workflow(&input);
        assert!(
            !matches!(result, Err(ConductorError::WorkflowRunAlreadyActive { .. })),
            "child workflow should not be blocked by active parent run"
        );
    }

    // -----------------------------------------------------------------------
    // execute_do tests (plain do {} block)
    // -----------------------------------------------------------------------

    #[test]
    fn test_execute_do_empty_body() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        let node = DoNode {
            output: None,
            with: vec![],
            body: vec![],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        assert!(state.all_succeeded);
    }

    #[test]
    fn test_execute_do_sets_and_restores_block_state() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Set some outer block state that should be saved and restored
        state.block_output = Some("outer-schema".into());
        state.block_with = vec!["outer-snippet".into()];

        let node = DoNode {
            output: Some("inner-schema".into()),
            with: vec!["inner-snippet".into()],
            // Use a Gate in dry_run mode as a no-op body node
            body: vec![WorkflowNode::Gate(GateNode {
                name: "noop".into(),
                gate_type: GateType::HumanApproval,
                prompt: None,
                min_approvals: 1,
                timeout_secs: 1,
                on_timeout: OnTimeout::Fail,
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());

        // After execute_do, outer state must be restored
        assert_eq!(state.block_output.as_deref(), Some("outer-schema"));
        assert_eq!(state.block_with, vec!["outer-snippet".to_string()]);
    }

    #[test]
    fn test_execute_do_restores_state_on_error() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.block_output = Some("outer-schema".into());
        state.block_with = vec!["outer-snippet".into()];

        // A call node without dry_run and no real agent will error
        let node = DoNode {
            output: Some("inner-schema".into()),
            with: vec!["inner-snippet".into()],
            body: vec![WorkflowNode::Call(CallNode {
                agent: AgentRef::Name("nonexistent-agent".into()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_err());

        // Block state must be restored even after error
        assert_eq!(state.block_output.as_deref(), Some("outer-schema"));
        assert_eq!(state.block_with, vec!["outer-snippet".to_string()]);
    }

    #[test]
    fn test_execute_do_fail_fast_exits_early() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.fail_fast = true;
        state.exec_config.dry_run = true;
        state.all_succeeded = false; // simulate prior failure

        let initial_position = state.position;

        let node = DoNode {
            output: None,
            with: vec![],
            body: vec![
                WorkflowNode::Gate(GateNode {
                    name: "g1".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                }),
                WorkflowNode::Gate(GateNode {
                    name: "g2".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                }),
            ],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        // fail_fast should skip after first node — only 1 position increment
        assert_eq!(state.position - initial_position, 1);
    }

    #[test]
    fn test_execute_do_nested_with_combination() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Outer do sets with=["a"], inner do sets with=["b"].
        // After inner do runs, inner block_with should have been ["b", "a"].
        // After both do blocks complete, state should be fully restored.
        let node = DoNode {
            output: Some("outer-schema".into()),
            with: vec!["a".into()],
            body: vec![WorkflowNode::Do(DoNode {
                output: None,
                with: vec!["b".into()],
                body: vec![WorkflowNode::Gate(GateNode {
                    name: "noop".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                })],
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        // Outer state fully restored
        assert!(state.block_output.is_none());
        assert!(state.block_with.is_empty());
    }

    #[test]
    fn test_execute_do_nested_inner_output_overrides_outer() {
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);
        state.exec_config.dry_run = true;

        // Outer do sets output="outer", inner do sets output="inner".
        // Inner body should see block_output="inner".
        // Verify state restoration after nested execution.
        let node = DoNode {
            output: Some("outer".into()),
            with: vec![],
            body: vec![WorkflowNode::Do(DoNode {
                output: Some("inner".into()),
                with: vec![],
                body: vec![WorkflowNode::Gate(GateNode {
                    name: "noop".into(),
                    gate_type: GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    timeout_secs: 1,
                    on_timeout: OnTimeout::Fail,
                })],
            })],
        };

        let result = execute_do(&mut state, &node);
        assert!(result.is_ok());
        // Outer state fully restored
        assert!(state.block_output.is_none());
        assert!(state.block_with.is_empty());
    }

    #[test]
    fn test_execute_call_merges_block_state() {
        // Verify execute_call picks up block_output and block_with from state.
        // The call will fail (no agent file on disk) but it should attempt to
        // load with the effective values rather than panicking.
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.block_output = Some("block-schema".into());
        state.block_with = vec!["block-snippet".into()];

        let node = CallNode {
            agent: AgentRef::Name("nonexistent".into()),
            retries: 0,
            on_fail: None,
            output: None,
            with: vec!["call-snippet".into()],
        };

        // Call will error on load_agent, but the merging logic should execute
        // without panics and the error should be from agent loading, not from
        // the effective_output/effective_with computation.
        let result = execute_call(&mut state, &node, 0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("agent") || err.contains("nonexistent"),
            "expected agent load error, got: {err}"
        );
    }

    #[test]
    fn test_execute_call_node_output_overrides_block_output() {
        // When a CallNode has its own output, it should take precedence
        // over block_output. Verify the call attempts to use "call-schema".
        let conn = setup_db();
        let config = Config::default();
        let mut state = make_loop_test_state(&conn, &config);

        state.block_output = Some("block-schema".into());

        let node = CallNode {
            agent: AgentRef::Name("nonexistent".into()),
            retries: 0,
            on_fail: None,
            output: Some("call-schema".into()),
            with: vec![],
        };

        let result = execute_call(&mut state, &node, 0);
        assert!(result.is_err());
        // The error is from agent loading, not from the merging logic
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("agent") || err.contains("nonexistent"),
            "expected agent load error, got: {err}"
        );
    }

    // ---------------------------------------------------------------------------
    // bubble_up_child_step_results tests
    // ---------------------------------------------------------------------------

    fn create_child_run(conn: &Connection) -> (WorkflowManager<'_>, String) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("child-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        (wf_mgr, run.id)
    }

    #[test]
    fn test_bubble_up_child_step_results_basic() {
        let conn = setup_db();
        let (wf_mgr, run_id) = create_child_run(&conn);

        // Insert two completed steps with markers
        let step1 = wf_mgr
            .insert_step(&run_id, "review-aggregator", "reviewer", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step1,
                WorkflowStepStatus::Completed,
                None,
                Some("done"),
                Some("some context"),
                Some(r#"["has_review_issues"]"#),
                None,
            )
            .unwrap();

        let step2 = wf_mgr
            .insert_step(&run_id, "lint-checker", "reviewer", false, 1, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step2,
                WorkflowStepStatus::Completed,
                None,
                Some("done"),
                Some("lint ok"),
                Some(r#"["lint_passed"]"#),
                None,
            )
            .unwrap();

        let result = bubble_up_child_step_results(&wf_mgr, &run_id);

        assert_eq!(result.len(), 2);
        let agg = result.get("review-aggregator").unwrap();
        assert!(agg.markers.contains(&"has_review_issues".to_string()));
        let lint = result.get("lint-checker").unwrap();
        assert!(lint.markers.contains(&"lint_passed".to_string()));
    }

    #[test]
    fn test_bubble_up_child_step_results_parent_wins() {
        let conn = setup_db();
        let config: &'static Config = Box::leak(Box::new(Config::default()));
        let (mut state, _run_id) = make_state_with_run(&conn, config);

        // Parent already has a step result for "review-aggregator"
        state.step_results.insert(
            "review-aggregator".to_string(),
            StepResult {
                step_name: "review-aggregator".to_string(),
                status: WorkflowStepStatus::Completed,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers: vec!["parent_marker".to_string()],
                context: "parent context".to_string(),
                child_run_id: None,
                structured_output: None,
            },
        );

        // Child run with same step name but different marker
        let (child_wf_mgr, child_run_id) = create_child_run(&conn);
        let step1 = child_wf_mgr
            .insert_step(&child_run_id, "review-aggregator", "reviewer", false, 0, 0)
            .unwrap();
        child_wf_mgr
            .update_step_status(
                &step1,
                WorkflowStepStatus::Completed,
                None,
                Some("done"),
                Some("child context"),
                Some(r#"["child_marker"]"#),
                None,
            )
            .unwrap();

        let child_steps = bubble_up_child_step_results(&child_wf_mgr, &child_run_id);
        for (key, value) in child_steps {
            state.step_results.entry(key).or_insert(value);
        }

        // Parent's value should win
        let result = state.step_results.get("review-aggregator").unwrap();
        assert!(result.markers.contains(&"parent_marker".to_string()));
        assert!(!result.markers.contains(&"child_marker".to_string()));
    }

    #[test]
    fn test_bubble_up_child_step_results_no_completed_steps() {
        let conn = setup_db();
        let (wf_mgr, run_id) = create_child_run(&conn);

        // Insert a failed step — should not be bubbled up
        let step1 = wf_mgr
            .insert_step(&run_id, "some-step", "reviewer", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &step1,
                WorkflowStepStatus::Failed,
                None,
                Some("failed"),
                None,
                None,
                None,
            )
            .unwrap();

        let result = bubble_up_child_step_results(&wf_mgr, &run_id);
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // Resume-related tests
    // -----------------------------------------------------------------------

    /// Helper: create a workflow run with steps in various statuses.
    fn setup_run_with_steps(conn: &Connection) -> (String, WorkflowManager<'_>) {
        let agent_mgr = AgentManager::new(conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(conn);
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Step 0: completed
        let s0 = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &s0,
            WorkflowStepStatus::Completed,
            None,
            Some("result-a"),
            Some("ctx-a"),
            Some(r#"["marker_a"]"#),
            Some(0),
        )
        .unwrap();

        // Step 1: failed
        let s1 = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        mgr.update_step_status(
            &s1,
            WorkflowStepStatus::Failed,
            None,
            Some("error"),
            None,
            None,
            Some(0),
        )
        .unwrap();

        // Step 2: running (stalled)
        let s2 = mgr
            .insert_step(&run.id, "step-c", "actor", false, 2, 0)
            .unwrap();
        mgr.update_step_status(
            &s2,
            WorkflowStepStatus::Running,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        (run.id, mgr)
    }

    #[test]
    fn test_reset_failed_steps() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        let count = mgr.reset_failed_steps(&run_id).unwrap();
        // Should reset both 'failed' and 'running' steps
        assert_eq!(count, 2);

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed); // unchanged
        assert_eq!(steps[1].status, WorkflowStepStatus::Pending); // was failed
        assert!(steps[1].result_text.is_none()); // cleared
        assert_eq!(steps[2].status, WorkflowStepStatus::Pending); // was running
    }

    #[test]
    fn test_reset_completed_steps() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        let count = mgr.reset_completed_steps(&run_id).unwrap();
        assert_eq!(count, 1);

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Pending); // was completed
        assert!(steps[0].result_text.is_none()); // cleared
        assert!(steps[0].context_out.is_none()); // cleared
    }

    #[test]
    fn test_reset_steps_from_position() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        // Reset from position 1 onwards
        let count = mgr.reset_steps_from_position(&run_id, 1).unwrap();
        assert_eq!(count, 2); // positions 1 and 2

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed); // position 0 unchanged
        assert_eq!(steps[1].status, WorkflowStepStatus::Pending);
        assert_eq!(steps[2].status, WorkflowStepStatus::Pending);
    }

    #[test]
    fn test_get_completed_step_keys() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        let keys = mgr.get_completed_step_keys(&run_id).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&("step-a".to_string(), 0)));
        // Failed/running steps should not be in the set
        assert!(!keys.contains(&("step-b".to_string(), 0)));
        assert!(!keys.contains(&("step-c".to_string(), 0)));
    }

    // -----------------------------------------------------------------------
    // find_max_completed_while_iteration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_max_completed_while_iteration_none_completed() {
        let conn = setup_db();
        let state = make_test_state(&conn);

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
            })],
        };

        // No resume context → returns 0
        assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
    }

    #[test]
    fn test_find_max_completed_while_iteration_two_completed() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let skip: HashSet<StepKey> = [("step-a".to_string(), 0), ("step-a".to_string(), 1)]
            .into_iter()
            .collect();
        state.resume_ctx = Some(ResumeContext {
            skip_completed: skip,
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![WorkflowNode::Call(CallNode {
                agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                retries: 0,
                on_fail: None,
                output: None,
                with: vec![],
            })],
        };

        // Iterations 0 and 1 completed → start from 2
        assert_eq!(find_max_completed_while_iteration(&state, &node), 2);
    }

    #[test]
    fn test_find_max_completed_while_iteration_empty_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        state.resume_ctx = Some(ResumeContext {
            skip_completed: HashSet::new(),
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![], // no call nodes
        };

        // Empty body → returns 0
        assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
    }

    #[test]
    fn test_find_max_completed_while_iteration_partial_body() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        // Two body nodes, but only one completed for iteration 0
        let skip: HashSet<StepKey> = [("step-a".to_string(), 0)].into_iter().collect();
        state.resume_ctx = Some(ResumeContext {
            skip_completed: skip,
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });
        // step-b:0 is NOT in skip_completed

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![
                WorkflowNode::Call(CallNode {
                    agent: crate::workflow_dsl::AgentRef::Name("step-a".to_string()),
                    retries: 0,
                    on_fail: None,
                    output: None,
                    with: vec![],
                }),
                WorkflowNode::Call(CallNode {
                    agent: crate::workflow_dsl::AgentRef::Name("step-b".to_string()),
                    retries: 0,
                    on_fail: None,
                    output: None,
                    with: vec![],
                }),
            ],
        };

        // Only partial completion → start from 0
        assert_eq!(find_max_completed_while_iteration(&state, &node), 0);
    }

    #[test]
    fn test_find_max_completed_while_iteration_with_parallel_and_gate() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let skip: HashSet<StepKey> = [
            ("agent-a".to_string(), 0),
            ("agent-b".to_string(), 0),
            ("approval".to_string(), 0),
        ]
        .into_iter()
        .collect();
        state.resume_ctx = Some(ResumeContext {
            skip_completed: skip,
            step_map: HashMap::new(),
            child_runs: HashMap::new(),
        });

        let node = WhileNode {
            step: "check".to_string(),
            marker: "needs_work".to_string(),
            max_iterations: 5,
            stuck_after: None,
            on_max_iter: crate::workflow_dsl::OnMaxIter::Fail,
            body: vec![
                WorkflowNode::Parallel(ParallelNode {
                    fail_fast: true,
                    min_success: None,
                    calls: vec![
                        crate::workflow_dsl::AgentRef::Name("agent-a".to_string()),
                        crate::workflow_dsl::AgentRef::Name("agent-b".to_string()),
                    ],
                    output: None,
                    call_outputs: HashMap::new(),
                    with: vec![],
                    call_with: HashMap::new(),
                }),
                WorkflowNode::Gate(GateNode {
                    name: "approval".to_string(),
                    gate_type: crate::workflow_dsl::GateType::HumanApproval,
                    prompt: None,
                    min_approvals: 1,
                    timeout_secs: 300,
                    on_timeout: crate::workflow_dsl::OnTimeout::Fail,
                }),
            ],
        };

        // Iteration 0 fully completed → start from 1
        assert_eq!(find_max_completed_while_iteration(&state, &node), 1);
    }

    // -----------------------------------------------------------------------
    // restore_completed_step tests
    // -----------------------------------------------------------------------

    /// Helper to build a WorkflowRunStep for testing without listing every field.
    fn make_test_step(
        step_name: &str,
        status: WorkflowStepStatus,
        result_text: Option<&str>,
        context_out: Option<&str>,
        markers_out: Option<&str>,
        child_run_id: Option<&str>,
        structured_output: Option<&str>,
    ) -> WorkflowRunStep {
        WorkflowRunStep {
            id: "s1".to_string(),
            workflow_run_id: "run1".to_string(),
            step_name: step_name.to_string(),
            role: "actor".to_string(),
            can_commit: false,
            condition_expr: None,
            status,
            child_run_id: child_run_id.map(String::from),
            position: 0,
            started_at: None,
            ended_at: None,
            result_text: result_text.map(String::from),
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: context_out.map(String::from),
            markers_out: markers_out.map(String::from),
            retry_count: 0,
            gate_type: None,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: structured_output.map(String::from),
        }
    }

    /// Helper to build a ResumeContext from a step map.
    fn make_resume_ctx(
        step_map: HashMap<StepKey, WorkflowRunStep>,
        child_runs: HashMap<String, crate::agent::AgentRun>,
    ) -> ResumeContext {
        let skip_completed = step_map.keys().cloned().collect();
        ResumeContext {
            skip_completed,
            step_map,
            child_runs,
        }
    }

    #[test]
    fn test_restore_completed_step_basic() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let step = make_test_step(
            "review",
            WorkflowStepStatus::Completed,
            Some("looks good"),
            Some("reviewed code"),
            Some(r#"["approved"]"#),
            None,
            Some(r#"{"verdict":"approve"}"#),
        );
        let ctx = make_resume_ctx(
            [(("review".to_string(), 0), step)].into_iter().collect(),
            HashMap::new(),
        );

        restore_completed_step(&mut state, &ctx, "review", 0);

        // Verify step_results populated
        let result = state.step_results.get("review").unwrap();
        assert_eq!(result.status, WorkflowStepStatus::Completed);
        assert_eq!(result.result_text.as_deref(), Some("looks good"));
        assert_eq!(result.markers, vec!["approved"]);
        assert_eq!(result.context, "reviewed code");

        // Verify contexts populated
        assert_eq!(state.contexts.len(), 1);
        assert_eq!(state.contexts[0].step, "review");
        assert_eq!(state.contexts[0].context, "reviewed code");

        // Verify structured output updated
        assert_eq!(
            state.last_structured_output.as_deref(),
            Some(r#"{"verdict":"approve"}"#)
        );
    }

    #[test]
    fn test_restore_completed_step_not_found() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let ctx = make_resume_ctx(HashMap::new(), HashMap::new());
        restore_completed_step(&mut state, &ctx, "nonexistent", 0);

        // Should be a no-op (with warning logged)
        assert!(state.step_results.is_empty());
        assert!(state.contexts.is_empty());
    }

    #[test]
    fn test_restore_completed_step_accumulates_costs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);

        // Create a child agent run with cost data
        let child_run = agent_mgr
            .create_run(Some("w1"), "test agent", None, None)
            .unwrap();
        agent_mgr
            .update_run_completed(
                &child_run.id,
                None,
                Some("done"),
                Some(0.05),
                Some(3),
                Some(5000),
            )
            .unwrap();

        let mut state = make_test_state(&conn);
        state.total_cost = 0.10;
        state.total_turns = 5;
        state.total_duration_ms = 10000;

        // Re-fetch the child run so we have the full AgentRun with costs
        let loaded_run = agent_mgr.get_run(&child_run.id).unwrap().unwrap();

        let step = make_test_step(
            "build",
            WorkflowStepStatus::Completed,
            Some("built"),
            Some("build output"),
            None,
            Some(&child_run.id),
            None,
        );
        let ctx = make_resume_ctx(
            [(("build".to_string(), 0), step)].into_iter().collect(),
            [(child_run.id.clone(), loaded_run)].into_iter().collect(),
        );

        restore_completed_step(&mut state, &ctx, "build", 0);

        // Costs should be accumulated from the child run
        assert!((state.total_cost - 0.15).abs() < 0.001);
        assert_eq!(state.total_turns, 8);
        assert_eq!(state.total_duration_ms, 15000);
    }

    #[test]
    fn test_restore_completed_step_restores_gate_feedback() {
        let conn = setup_db();
        let mut state = make_test_state(&conn);

        let mut step = make_test_step(
            "approval-gate",
            WorkflowStepStatus::Completed,
            Some("approved"),
            None,
            None,
            None,
            None,
        );
        step.gate_feedback = Some("LGTM, ship it".to_string());

        let ctx = make_resume_ctx(
            [(("approval-gate".to_string(), 0), step)]
                .into_iter()
                .collect(),
            HashMap::new(),
        );

        restore_completed_step(&mut state, &ctx, "approval-gate", 0);

        // Gate feedback should be restored for downstream steps
        assert_eq!(state.last_gate_feedback.as_deref(), Some("LGTM, ship it"));
    }

    // -----------------------------------------------------------------------
    // resume_workflow validation tests
    // -----------------------------------------------------------------------

    /// Helper: create a Config suitable for resume tests.
    fn make_resume_config() -> &'static Config {
        Box::leak(Box::new(Config::default()))
    }

    #[test]
    fn test_resume_rejects_completed_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("Cannot resume a completed"),
            "Expected completed-run error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_cancelled_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Cancelled, None)
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("Cannot resume a cancelled"),
            "Expected cancelled-run error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_running_run() {
        let err =
            validate_resume_preconditions(&WorkflowRunStatus::Running, false, None).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "Expected running-run error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_restart_and_from_step_together() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: Some("step-one"),
            restart: true,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string()
                .contains("--restart and --from-step together"),
            "Expected conflict error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_missing_snapshot() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        // Create run with no definition_snapshot
        let run = wf_mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"))
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("no definition snapshot"),
            "Expected missing-snapshot error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_nonexistent_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: "nonexistent-id",
            model: None,
            from_step: None,
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "Expected not-found error, got: {err}"
        );
    }

    #[test]
    fn test_resume_rejects_nonexistent_from_step() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("error"))
            .unwrap();

        // Add a step so the run has steps to search through
        let s0 = wf_mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        wf_mgr
            .update_step_status(
                &s0,
                WorkflowStepStatus::Completed,
                None,
                Some("ok"),
                None,
                None,
                Some(0),
            )
            .unwrap();

        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: Some("nonexistent-step"),
            restart: false,
        };
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("not found in workflow run"),
            "Expected step-not-found error, got: {err}"
        );
    }

    #[test]
    fn test_set_workflow_run_inputs_round_trip() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();

        // Initially inputs should be empty (no inputs set yet)
        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert!(fetched.inputs.is_empty(), "Expected no inputs initially");

        // Write inputs and read back
        let mut inputs = HashMap::new();
        inputs.insert("key1".to_string(), "value1".to_string());
        inputs.insert("key2".to_string(), "value2".to_string());
        wf_mgr.set_workflow_run_inputs(&run.id, &inputs).unwrap();

        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.inputs.get("key1").map(String::as_str),
            Some("value1")
        );
        assert_eq!(
            fetched.inputs.get("key2").map(String::as_str),
            Some("value2")
        );
        assert_eq!(fetched.inputs.len(), 2);
    }

    #[test]
    fn test_row_to_workflow_run_malformed_inputs_json_returns_empty() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();

        // Directly write invalid JSON into the inputs column to simulate corruption
        conn.execute(
            "UPDATE workflow_runs SET inputs = ?1 WHERE id = ?2",
            rusqlite::params!["not-valid-json", &run.id],
        )
        .unwrap();

        // Reading back should return an empty HashMap (not panic), matching the
        // unwrap_or_else + warn fallback in row_to_workflow_run.
        let fetched = wf_mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert!(
            fetched.inputs.is_empty(),
            "Expected empty inputs on malformed JSON, got: {:?}",
            fetched.inputs
        );
    }

    #[test]
    fn test_restart_resets_all_steps() {
        let conn = setup_db();
        let (run_id, mgr) = setup_run_with_steps(&conn);

        // Verify initial state: 1 completed, 1 failed, 1 running
        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(steps[0].status, WorkflowStepStatus::Completed);
        assert_eq!(steps[1].status, WorkflowStepStatus::Failed);
        assert_eq!(steps[2].status, WorkflowStepStatus::Running);

        // Restart resets both failed+running and completed steps
        mgr.reset_failed_steps(&run_id).unwrap();
        mgr.reset_completed_steps(&run_id).unwrap();

        let steps = mgr.get_workflow_steps(&run_id).unwrap();
        assert_eq!(
            steps[0].status,
            WorkflowStepStatus::Pending,
            "completed step should be reset"
        );
        assert!(steps[0].result_text.is_none(), "result should be cleared");
        assert!(steps[0].context_out.is_none(), "context should be cleared");
        assert!(steps[0].markers_out.is_none(), "markers should be cleared");
        assert_eq!(
            steps[1].status,
            WorkflowStepStatus::Pending,
            "failed step should be reset"
        );
        assert_eq!(
            steps[2].status,
            WorkflowStepStatus::Pending,
            "running step should be reset"
        );

        // skip set should be empty after restart
        let keys = mgr.get_completed_step_keys(&run_id).unwrap();
        assert!(
            keys.is_empty(),
            "no completed steps should remain after restart"
        );
    }

    /// Exercises the full --from-step DB orchestration path:
    /// - skip-set pruning (keys at/after pos removed)
    /// - step_map filtered to only surviving skip keys
    /// - DB reset: steps at/after the target step become Pending
    /// - steps before the target step remain Completed
    #[test]
    fn test_from_step_skip_set_and_step_map() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();

        // Insert 3 completed steps at positions 0, 1, 2
        let s0 = mgr
            .insert_step(&run.id, "step-a", "actor", false, 0, 0)
            .unwrap();
        mgr.update_step_status(
            &s0,
            WorkflowStepStatus::Completed,
            None,
            Some("result-a"),
            Some("ctx-a"),
            None,
            Some(0),
        )
        .unwrap();

        let s1 = mgr
            .insert_step(&run.id, "step-b", "actor", false, 1, 0)
            .unwrap();
        mgr.update_step_status(
            &s1,
            WorkflowStepStatus::Completed,
            None,
            Some("result-b"),
            Some("ctx-b"),
            None,
            Some(0),
        )
        .unwrap();

        let s2 = mgr
            .insert_step(&run.id, "step-c", "actor", false, 2, 0)
            .unwrap();
        mgr.update_step_status(
            &s2,
            WorkflowStepStatus::Completed,
            None,
            Some("result-c"),
            Some("ctx-c"),
            None,
            Some(0),
        )
        .unwrap();

        // Snapshot all_steps before any resets (mirrors resume_workflow: load once upfront)
        let all_steps = mgr.get_workflow_steps(&run.id).unwrap();

        // Simulate the --from-step "step-b" (position 1) branch of resume_workflow
        let mut keys = completed_keys_from_steps(&all_steps);
        assert_eq!(
            keys.len(),
            3,
            "all three steps should be in completed keys initially"
        );

        let pos = all_steps
            .iter()
            .find(|s| s.step_name == "step-b")
            .unwrap()
            .position;
        assert_eq!(pos, 1);

        // Prune keys at/after the from-step position (mirrors resume_workflow)
        let to_remove: Vec<StepKey> = all_steps
            .iter()
            .filter(|s| s.position >= pos && s.status == WorkflowStepStatus::Completed)
            .map(|s| (s.step_name.clone(), s.iteration as u32))
            .collect();
        for key in to_remove {
            keys.remove(&key);
        }

        // Reset DB state for steps at/after pos, then reset any failed/running steps
        mgr.reset_steps_from_position(&run.id, pos).unwrap();
        mgr.reset_failed_steps(&run.id).unwrap();

        // skip_completed should contain only ("step-a", 0)
        assert_eq!(keys.len(), 1, "only step-a:0 should survive pruning");
        assert!(
            keys.contains(&("step-a".to_string(), 0)),
            "step-a:0 must be in skip set"
        );
        assert!(
            !keys.contains(&("step-b".to_string(), 0)),
            "step-b:0 must be pruned from skip set"
        );
        assert!(
            !keys.contains(&("step-c".to_string(), 0)),
            "step-c:0 must be pruned from skip set"
        );

        // DB state: step-a stays Completed, step-b and step-c are reset to Pending
        let updated = mgr.get_workflow_steps(&run.id).unwrap();
        assert_eq!(
            updated[0].status,
            WorkflowStepStatus::Completed,
            "step-a (pos 0) must remain Completed"
        );
        assert_eq!(
            updated[1].status,
            WorkflowStepStatus::Pending,
            "step-b (pos 1, the from-step) must be reset to Pending"
        );
        assert_eq!(
            updated[2].status,
            WorkflowStepStatus::Pending,
            "step-c (pos 2) must be reset to Pending"
        );

        // step_map built from all_steps filtered by surviving skip keys
        // (mirrors resume_workflow)
        let step_map: HashMap<StepKey, WorkflowRunStep> = all_steps
            .into_iter()
            .filter(|s| s.status == WorkflowStepStatus::Completed)
            .map(|s| {
                let key = (s.step_name.clone(), s.iteration as u32);
                (key, s)
            })
            .filter(|(key, _)| keys.contains(key))
            .collect();

        assert!(
            step_map.contains_key(&("step-a".to_string(), 0)),
            "step_map must include step-a:0 (will be skipped on resume)"
        );
        assert!(
            !step_map.contains_key(&("step-b".to_string(), 0)),
            "step_map must not include step-b:0 (will be re-executed)"
        );
        assert!(
            !step_map.contains_key(&("step-c".to_string(), 0)),
            "step_map must not include step-c:0 (will be re-executed)"
        );
    }

    #[test]
    fn test_resume_allows_restart_on_completed_run() {
        let conn = setup_db();
        let config = make_resume_config();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run(
                "test-wf",
                Some("w1"),
                &parent.id,
                false,
                "manual",
                Some("{}"),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        // Without restart, completed run should be rejected
        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        };
        assert!(resume_workflow(&input).is_err());

        // With restart, completed run should pass the status check
        // (will fail later due to missing worktree, but the status check should pass)
        let input = WorkflowResumeInput {
            conn: &conn,
            config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: true,
        };
        let err = resume_workflow(&input).unwrap_err();
        // Should fail on worktree resolution, NOT on "Cannot resume a completed"
        assert!(
            !err.to_string().contains("Cannot resume a completed"),
            "restart=true should bypass the completed-run check, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // parallel min_success with skipped-on-resume agents
    // -----------------------------------------------------------------------

    /// Validates that the min_success calculation in execute_parallel correctly
    /// counts skipped-on-resume agents as successes for both the warning logic
    /// and the synthetic step status.
    ///
    /// This is a logic-level regression test: execute_parallel uses
    /// `effective_successes = successes + skipped_count` and
    /// `total_agents = children.len() + skipped_count`, and the synthetic step
    /// status must use `effective_successes >= min_required` (not raw `successes`).
    #[test]
    fn test_parallel_min_success_with_skipped_resume_agents() {
        // Scenario: 3 agents in a parallel block, min_success = 3.
        // On resume, 2 agents were already completed (skipped), 1 new agent succeeds.
        let successes: u32 = 1; // newly succeeded
        let skipped_count: u32 = 2; // completed on previous run
        let children_len: u32 = 1; // only the non-skipped agent was spawned

        let effective_successes = successes + skipped_count; // 3
        let total_agents = children_len + skipped_count; // 3
        let min_required: u32 = 3; // all must succeed

        // The synthetic step should be Completed, not Failed
        let status = if effective_successes >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        };
        assert_eq!(
            status,
            WorkflowStepStatus::Completed,
            "skipped agents must count toward min_success"
        );

        // Verify the all_succeeded flag would NOT be set to false
        let all_succeeded = effective_successes >= min_required;
        assert!(
            all_succeeded,
            "effective_successes ({effective_successes}) should meet min_required ({min_required})"
        );

        // Verify default min_success (None → total_agents) also works
        let default_min = total_agents;
        assert!(
            effective_successes >= default_min,
            "default min_success should be met when all agents (including skipped) succeed"
        );

        // Edge case: one new agent fails, only skipped agents succeeded
        let successes_fail: u32 = 0;
        let effective_fail = successes_fail + skipped_count; // 2
        let status_fail = if effective_fail >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        };
        assert_eq!(
            status_fail,
            WorkflowStepStatus::Failed,
            "should fail when effective successes don't meet min_required"
        );
    }

    // ---------------------------------------------------------------------------
    // apply_workflow_input_defaults tests
    // ---------------------------------------------------------------------------

    fn make_workflow_def_with_inputs(
        inputs: Vec<crate::workflow_dsl::InputDecl>,
    ) -> crate::workflow_dsl::WorkflowDef {
        crate::workflow_dsl::WorkflowDef {
            name: "test-wf".to_string(),
            description: String::new(),
            trigger: crate::workflow_dsl::WorkflowTrigger::Manual,
            targets: vec![],
            inputs,
            body: vec![],
            always: vec![],
            source_path: String::new(),
        }
    }

    #[test]
    fn test_apply_workflow_input_defaults_fills_missing_default() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "skip_tests".to_string(),
            required: false,
            default: Some("false".to_string()),
        }]);

        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("false"));
    }

    #[test]
    fn test_apply_workflow_input_defaults_does_not_overwrite_provided_value() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "skip_tests".to_string(),
            required: false,
            default: Some("false".to_string()),
        }]);

        let mut inputs = HashMap::new();
        inputs.insert("skip_tests".to_string(), "true".to_string());
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        // Provided value must not be replaced by the default.
        assert_eq!(inputs.get("skip_tests").map(String::as_str), Some("true"));
    }

    #[test]
    fn test_apply_workflow_input_defaults_errors_on_missing_required() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "ticket_id".to_string(),
            required: true,
            default: None,
        }]);

        let mut inputs = HashMap::new();
        let result = apply_workflow_input_defaults(&workflow, &mut inputs);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("ticket_id"),
            "error message should name the missing input, got: {msg}"
        );
    }

    #[test]
    fn test_apply_workflow_input_defaults_required_input_provided_succeeds() {
        use crate::workflow_dsl::InputDecl;

        let workflow = make_workflow_def_with_inputs(vec![InputDecl {
            name: "ticket_id".to_string(),
            required: true,
            default: None,
        }]);

        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "TKT-1".to_string());
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert_eq!(inputs.get("ticket_id").map(String::as_str), Some("TKT-1"));
    }

    #[test]
    fn test_apply_workflow_input_defaults_no_inputs_is_noop() {
        let workflow = make_workflow_def_with_inputs(vec![]);
        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert!(inputs.is_empty());
    }

    #[test]
    fn test_execute_workflow_ephemeral_skips_concurrent_guard() {
        // Verify that when worktree_id is None (ephemeral run), a second concurrent
        // call at depth==0 does NOT return WorkflowRunAlreadyActive — the guard is
        // intentionally skipped for ephemeral runs which have no registered worktree.
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();

        let workflow = make_empty_workflow();

        // First ephemeral call — must succeed (empty workflow, no agents to spawn).
        let input1 = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let result1 = execute_workflow(&input1);
        assert!(
            !matches!(
                result1,
                Err(ConductorError::WorkflowRunAlreadyActive { .. })
            ),
            "first ephemeral call should not be blocked by the concurrent guard"
        );

        // Second ephemeral call — must also not be blocked by the guard, even though
        // the first run's record now exists in the DB (it has no worktree_id, so the
        // guard is skipped entirely for ephemeral runs).
        let input2 = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let result2 = execute_workflow(&input2);
        assert!(
            !matches!(
                result2,
                Err(ConductorError::WorkflowRunAlreadyActive { .. })
            ),
            "second ephemeral call should not be blocked by the concurrent guard"
        );
    }

    // ---------------------------------------------------------------------------
    // purge / purge_count tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_purge_all_terminal_statuses() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a3 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a4 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let r_completed = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let r_failed = mgr
            .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();
        let r_cancelled = mgr
            .create_workflow_run("t", Some("w1"), &a3.id, false, "manual", None)
            .unwrap();
        let r_running = mgr
            .create_workflow_run("t", Some("w1"), &a4.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&r_completed.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&r_failed.id, WorkflowRunStatus::Failed, None)
            .unwrap();
        mgr.update_workflow_status(&r_cancelled.id, WorkflowRunStatus::Cancelled, None)
            .unwrap();
        mgr.update_workflow_status(&r_running.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let deleted = mgr
            .purge(None, &["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(deleted, 3);

        // running run must still exist
        assert!(mgr.get_workflow_run(&r_running.id).unwrap().is_some());
        // terminal runs must be gone
        assert!(mgr.get_workflow_run(&r_completed.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&r_failed.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&r_cancelled.id).unwrap().is_none());
    }

    #[test]
    fn test_purge_single_status_filter() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let r_completed = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let r_failed = mgr
            .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&r_completed.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&r_failed.id, WorkflowRunStatus::Failed, None)
            .unwrap();

        // only purge completed
        let deleted = mgr.purge(None, &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        assert!(mgr.get_workflow_run(&r_completed.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&r_failed.id).unwrap().is_some());
    }

    #[test]
    fn test_purge_repo_scoped() {
        let conn = setup_db();
        // Add a second repo + worktree
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r2', 'other-repo', '/tmp/r2', '', 'main', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r2', 'feat-other', 'feat/other', '/tmp/ws2/feat-other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w2"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run_r1 = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let run_r2 = mgr
            .create_workflow_run("t", Some("w2"), &a2.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&run_r1.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&run_r2.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        // scope to r1 only
        let deleted = mgr.purge(Some("r1"), &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        assert!(mgr.get_workflow_run(&run_r1.id).unwrap().is_none());
        assert!(mgr.get_workflow_run(&run_r2.id).unwrap().is_some());
    }

    #[test]
    fn test_purge_cascade_deletes_steps() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        mgr.insert_step(&run.id, "step1", "actor", true, 0, 0)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        let deleted = mgr.purge(None, &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        // steps must be gone (cascade)
        let steps = mgr.get_workflow_steps(&run.id).unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn test_purge_count_matches_purge() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();
        let a2 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let r1 = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        let r2 = mgr
            .create_workflow_run("t", Some("w1"), &a2.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&r1.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&r2.id, WorkflowRunStatus::Failed, None)
            .unwrap();

        let statuses = &["completed", "failed", "cancelled"];
        let count = mgr.purge_count(None, statuses).unwrap();
        assert_eq!(count, 2);

        let deleted = mgr.purge(None, statuses).unwrap();
        assert_eq!(deleted, count);
    }

    #[test]
    fn test_purge_noop_when_no_matches() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let a1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("t", Some("w1"), &a1.id, false, "manual", None)
            .unwrap();
        mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let count = mgr
            .purge_count(None, &["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(count, 0);

        let deleted = mgr
            .purge(None, &["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(deleted, 0);

        assert!(mgr.get_workflow_run(&run.id).unwrap().is_some());
    }

    #[test]
    fn test_purge_empty_statuses_is_noop() {
        let conn = setup_db();
        let mgr = WorkflowManager::new(&conn);
        assert_eq!(mgr.purge(None, &[]).unwrap(), 0);
        assert_eq!(mgr.purge_count(None, &[]).unwrap(), 0);
    }

    /// Repo-scoped purge must NOT delete global workflow runs (worktree_id IS NULL).
    #[test]
    fn test_purge_repo_scoped_does_not_delete_global_runs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);

        // Create a global run (no worktree) and a run scoped to w1.
        let a_global = agent_mgr.create_run(None, "wf", None, None).unwrap();
        let a_w1 = agent_mgr.create_run(Some("w1"), "wf", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run_global = mgr
            .create_workflow_run("t", None, &a_global.id, false, "manual", None)
            .unwrap();
        let run_w1 = mgr
            .create_workflow_run("t", Some("w1"), &a_w1.id, false, "manual", None)
            .unwrap();

        mgr.update_workflow_status(&run_global.id, WorkflowRunStatus::Completed, None)
            .unwrap();
        mgr.update_workflow_status(&run_w1.id, WorkflowRunStatus::Completed, None)
            .unwrap();

        // Scope purge to r1 — must only delete the worktree-bound run.
        assert_eq!(mgr.purge_count(Some("r1"), &["completed"]).unwrap(), 1);
        let deleted = mgr.purge(Some("r1"), &["completed"]).unwrap();
        assert_eq!(deleted, 1);

        // Global run must survive.
        assert!(mgr.get_workflow_run(&run_global.id).unwrap().is_some());
        // w1 run must be gone.
        assert!(mgr.get_workflow_run(&run_w1.id).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Implicit variable injection tests
    // -----------------------------------------------------------------------

    /// Insert a minimal ticket row into the test DB and return its id.
    fn insert_test_ticket(conn: &Connection, id: &str, repo_id: &str) {
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, \
             labels, url, synced_at, raw_json) \
             VALUES (?1, ?2, 'github', ?3, 'Test ticket title', '', 'open', '[]', \
             'https://github.com/test/repo/issues/1', '2024-01-01T00:00:00Z', '{}')",
            rusqlite::params![id, repo_id, id],
        )
        .unwrap();
    }

    #[test]
    fn test_execute_workflow_injects_repo_variables() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        // repo `r1` with local_path `/tmp/repo` is inserted by setup_db()
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: Some("r1"),
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(&result.workflow_run_id)
            .unwrap()
            .unwrap();

        assert_eq!(run.inputs.get("repo_id").map(String::as_str), Some("r1"));
        assert_eq!(
            run.inputs.get("repo_path").map(String::as_str),
            Some("/tmp/repo")
        );
        assert_eq!(
            run.inputs.get("repo_name").map(String::as_str),
            Some("test-repo")
        );
    }

    #[test]
    fn test_execute_workflow_injects_ticket_variables() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        insert_test_ticket(&conn, "tkt-1", "r1");

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: Some("tkt-1"),
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(&result.workflow_run_id)
            .unwrap()
            .unwrap();

        assert_eq!(
            run.inputs.get("ticket_id").map(String::as_str),
            Some("tkt-1")
        );
        assert_eq!(
            run.inputs.get("ticket_title").map(String::as_str),
            Some("Test ticket title")
        );
        assert!(
            run.inputs.contains_key("ticket_url"),
            "ticket_url should be injected"
        );
    }

    #[test]
    fn test_execute_workflow_existing_input_not_overwritten_by_injection() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let mut explicit_inputs = HashMap::new();
        explicit_inputs.insert("repo_name".to_string(), "my-override".to_string());

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: Some("r1"),
            model: None,
            exec_config: &exec_config,
            inputs: explicit_inputs,
            depth: 0,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(&result.workflow_run_id)
            .unwrap()
            .unwrap();

        // Caller-supplied repo_name must not be overwritten by the injected value.
        assert_eq!(
            run.inputs.get("repo_name").map(String::as_str),
            Some("my-override")
        );
    }

    #[test]
    fn test_execute_workflow_unknown_ticket_id_returns_error() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "",
            repo_path: "",
            ticket_id: Some("nonexistent-ticket"),
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        assert!(
            execute_workflow(&input).is_err(),
            "referencing a nonexistent ticket_id must return an error"
        );
    }

    #[test]
    fn test_resume_workflow_ephemeral_run_rejected() {
        let conn = setup_db();
        let config = Config::default();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let snapshot = serde_json::to_string(&make_empty_workflow()).unwrap();
        let run = wf_mgr
            .create_workflow_run(
                "ephemeral-wf",
                None,
                &parent.id,
                false,
                "manual",
                Some(&snapshot),
            )
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Failed, Some("step failed"))
            .unwrap();

        let result = resume_workflow(&WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &run.id,
            model: None,
            from_step: None,
            restart: false,
        });
        assert!(result.is_err(), "ephemeral run resume should be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ephemeral PR run"),
            "error should mention ephemeral PR run, got: {err}"
        );
    }

    #[test]
    fn test_resume_workflow_repo_target() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: None,
            repo_id: Some("r1"),
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        wf_mgr
            .update_workflow_status(
                &result.workflow_run_id,
                WorkflowRunStatus::Failed,
                Some("step failed"),
            )
            .unwrap();

        let resume_result = resume_workflow(&WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &result.workflow_run_id,
            model: None,
            from_step: None,
            restart: false,
        });
        assert!(
            resume_result.is_ok(),
            "resume of repo-targeted run should succeed: {:?}",
            resume_result.err()
        );
    }

    #[test]
    fn test_resume_workflow_ticket_target() {
        let conn = setup_db();
        let config = Config::default();
        let exec_config = WorkflowExecConfig::default();
        let workflow = make_empty_workflow();

        insert_test_ticket(&conn, "tkt-1", "r1");

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: None,
            working_dir: "/tmp/repo",
            repo_path: "/tmp/repo",
            ticket_id: Some("tkt-1"),
            repo_id: None,
            model: None,
            exec_config: &exec_config,
            inputs: HashMap::new(),
            depth: 0,
        };
        let result = execute_workflow(&input).unwrap();

        let wf_mgr = WorkflowManager::new(&conn);
        wf_mgr
            .update_workflow_status(
                &result.workflow_run_id,
                WorkflowRunStatus::Failed,
                Some("step failed"),
            )
            .unwrap();

        let resume_result = resume_workflow(&WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &result.workflow_run_id,
            model: None,
            from_step: None,
            restart: false,
        });
        assert!(
            resume_result.is_ok(),
            "resume of ticket-targeted run should succeed: {:?}",
            resume_result.err()
        );
    }
}
