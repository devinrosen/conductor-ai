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
    collect_agent_names, collect_workflow_refs, detect_workflow_cycles, AgentRef, InputDecl,
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
     started_at, ended_at, result_summary, definition_snapshot";

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
        &state.worktree_path,
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
    pub worktree_id: String,
    pub parent_run_id: String,
    pub status: WorkflowRunStatus,
    pub dry_run: bool,
    pub trigger: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub result_summary: Option<String>,
    pub definition_snapshot: Option<String>,
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
}

impl Default for WorkflowExecConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            step_timeout: Duration::from_secs(30 * 60),
            fail_fast: true,
            dry_run: false,
        }
    }
}

/// Result of executing a workflow.
#[derive(Debug, Clone)]
pub struct WorkflowResult {
    pub workflow_run_id: String,
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
        worktree_id: &str,
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
            worktree_id: worktree_id.to_string(),
            parent_run_id: parent_run_id.to_string(),
            status: WorkflowRunStatus::Pending,
            dry_run,
            trigger: trigger.to_string(),
            started_at: now,
            ended_at: None,
            result_summary: None,
            definition_snapshot: definition_snapshot.map(String::from),
        })
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
}

fn row_to_workflow_run(row: &rusqlite::Row) -> rusqlite::Result<WorkflowRun> {
    let dry_run_int: i64 = row.get(5)?;
    Ok(WorkflowRun {
        id: row.get(0)?,
        workflow_name: row.get(1)?,
        worktree_id: row.get(2)?,
        parent_run_id: row.get(3)?,
        status: row.get(4)?,
        dry_run: dry_run_int != 0,
        trigger: row.get(6)?,
        started_at: row.get(7)?,
        ended_at: row.get(8)?,
        result_summary: row.get(9)?,
        definition_snapshot: row.get(10)?,
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

/// Mutable runtime state for a workflow execution.
struct ExecutionState<'a> {
    conn: &'a Connection,
    config: &'a Config,
    workflow_run_id: String,
    workflow_name: String,
    worktree_id: String,
    worktree_path: String,
    worktree_slug: String,
    repo_path: String,
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
}

// ---------------------------------------------------------------------------
// Execution engine
// ---------------------------------------------------------------------------

/// Input parameters for workflow execution.
pub struct WorkflowExecInput<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub workflow: &'a WorkflowDef,
    pub worktree_id: &'a str,
    pub worktree_path: &'a str,
    pub repo_path: &'a str,
    pub model: Option<&'a str>,
    pub exec_config: &'a WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
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
    let wt_mgr = WorktreeManager::new(conn, config);
    let worktree = wt_mgr.get_by_id(input.worktree_id)?;

    // Validate all referenced agents exist before starting
    let mut all_agents = workflow_dsl::collect_agent_names(&workflow.body);
    all_agents.extend(workflow_dsl::collect_agent_names(&workflow.always));
    all_agents.sort();
    all_agents.dedup();

    let specs: Vec<AgentSpec> = all_agents.iter().map(AgentSpec::from).collect();
    let missing_agents = agent_config::find_missing_agents(
        input.worktree_path,
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
            input.worktree_path,
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
    if input.depth == 0 {
        if let Some(active) = wf_mgr.get_active_run_for_worktree(input.worktree_id)? {
            return Err(ConductorError::WorkflowRunAlreadyActive {
                name: active.workflow_name,
            });
        }
    }

    // Create parent agent run
    let parent_prompt = format!("Workflow: {} — {}", workflow.name, workflow.description);
    let parent_run = agent_mgr.create_run(input.worktree_id, &parent_prompt, None, input.model)?;

    // Create workflow run record with snapshot
    let wf_run = wf_mgr.create_workflow_run(
        &workflow.name,
        input.worktree_id,
        &parent_run.id,
        input.exec_config.dry_run,
        &workflow.trigger.to_string(),
        Some(&snapshot_json),
    )?;

    // Mark as running
    wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None)?;

    let mut state = ExecutionState {
        conn,
        config,
        workflow_run_id: wf_run.id.clone(),
        workflow_name: workflow.name.clone(),
        worktree_id: input.worktree_id.to_string(),
        worktree_path: input.worktree_path.to_string(),
        worktree_slug: worktree.slug.clone(),
        repo_path: input.repo_path.to_string(),
        model: input.model.map(String::from),
        exec_config: input.exec_config.clone(),
        inputs: input.inputs.clone(),
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
    };

    // Execute main body
    let body_result = execute_nodes(&mut state, &workflow.body);
    if let Err(ref e) = body_result {
        tracing::error!("Body execution error: {e}");
        state.all_succeeded = false;
    }

    // Execute always block regardless of outcome
    if !workflow.always.is_empty() {
        let workflow_status = if state.all_succeeded {
            "completed"
        } else {
            "failed"
        };
        // Inject {{workflow_status}} into inputs for always steps
        state
            .inputs
            .insert("workflow_status".to_string(), workflow_status.to_string());
        let always_result = execute_nodes(&mut state, &workflow.always);
        if let Err(ref e) = always_result {
            tracing::warn!("Always block error (non-fatal): {e}");
            // Don't change all_succeeded for always failures
        }
    }

    // Build summary
    let summary = build_workflow_summary(&state);

    // Finalize
    if state.all_succeeded {
        agent_mgr.update_run_completed(
            &parent_run.id,
            None,
            Some(&summary),
            Some(state.total_cost),
            Some(state.total_turns),
            Some(state.total_duration_ms),
        )?;
        wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Completed, Some(&summary))?;
        tracing::info!("Workflow '{}' completed successfully", workflow.name);
    } else {
        agent_mgr.update_run_failed(&parent_run.id, &summary)?;
        wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Failed, Some(&summary))?;
        tracing::warn!("Workflow '{}' finished with failures", workflow.name);
    }

    tracing::info!(
        "Total: ${:.4}, {} turns, {:.1}s",
        state.total_cost,
        state.total_turns,
        state.total_duration_ms as f64 / 1000.0
    );

    Ok(WorkflowResult {
        workflow_run_id: wf_run.id,
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
    pub worktree_id: String,
    pub worktree_path: String,
    pub repo_path: String,
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
        worktree_id: &params.worktree_id,
        worktree_path: &params.worktree_path,
        repo_path: &params.repo_path,
        model: params.model.as_deref(),
        exec_config: &params.exec_config,
        inputs: params.inputs.clone(),
        depth: 0,
    };

    execute_workflow(&input)
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
    let effective_output: Option<String> =
        node.output.clone().or_else(|| state.block_output.clone());
    // Block-level `with` snippets prepended to call-level `with`.
    let effective_with: Vec<String> = state
        .block_with
        .iter()
        .cloned()
        .chain(node.with.iter().cloned())
        .collect();
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

    // Load agent definition
    let agent_def = agent_config::load_agent(
        &state.worktree_path,
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
        &state.worktree_path,
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

        let child_window =
            sanitize_tmux_name(&format!("{}-wf-{}", state.worktree_slug, agent_label));
        let child_run = state.agent_mgr.create_child_run(
            &state.worktree_id,
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
            &state.worktree_path,
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
                state.wf_mgr.update_step_status(
                    &step_id,
                    WorkflowStepStatus::Failed,
                    Some(&child_run.id),
                    Some(&e),
                    None,
                    None,
                    Some(attempt as i64),
                )?;
                last_error = e;
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
        workflow_dsl::load_workflow_by_name(&state.worktree_path, &state.repo_path, &node.workflow)
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
            &format!("workflow:{}", node.workflow),
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
            worktree_id: &state.worktree_id,
            worktree_path: &state.worktree_path,
            repo_path: &state.repo_path,
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
                        Some(result.workflow_run_id),
                        iteration,
                        None,
                    );

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
    let mut iteration = 0u32;
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

    for (i, agent_ref) in node.calls.iter().enumerate() {
        let pos = pos_base + i as i64;
        state.position = pos + 1;
        let agent_label = agent_ref.label();

        let agent_def = agent_config::load_agent(
            &state.worktree_path,
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
            &state.worktree_path,
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

        let window_name =
            sanitize_tmux_name(&format!("{}-wf-{}-{}", state.worktree_slug, agent_label, i));
        let child_run = state.agent_mgr.create_child_run(
            &state.worktree_id,
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
            &state.worktree_path,
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

    // Apply min_success policy
    let min_required = node.min_success.unwrap_or(children.len() as u32);
    tracing::info!(
        "parallel: {successes} succeeded, {failures} failed out of {} agents",
        children.len()
    );
    if successes < min_required {
        tracing::warn!(
            "parallel: only {}/{} succeeded (min_success={})",
            successes,
            children.len(),
            min_required
        );
        state.all_succeeded = false;
    }

    // Store merged markers as a synthetic result
    let synthetic_result = StepResult {
        step_name: format!("parallel:{}", group_id),
        status: if successes >= min_required {
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
                    .current_dir(&state.worktree_path)
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
                    .current_dir(&state.worktree_path)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test-coverage", "w1", &parent.id, false, "manual", None)
            .unwrap();

        assert_eq!(run.workflow_name, "test-coverage");
        assert_eq!(run.status, WorkflowRunStatus::Pending);
        assert!(!run.dry_run);
    }

    #[test]
    fn test_create_workflow_run_with_snapshot() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run(
                "test",
                "w1",
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
            worktree_id: "w1".to_string(),
            worktree_path: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
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

        let run = mgr.create_run("w1", "test", None, None).unwrap();
        mgr.update_run_completed(&run.id, None, Some("done"), Some(0.05), Some(3), Some(5000))
            .unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, AgentRunStatus::Completed);
    }

    #[test]
    fn test_poll_child_completion_timeout() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "test", None, None).unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_millis(50),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[test]
    fn test_list_workflow_runs() {
        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run("w1", "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run("w1", "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("test-a", "w1", &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("test-b", "w1", &p2.id, true, "pr", None)
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
        let p1 = agent_mgr.create_run("w1", "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run("w2", "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("flow-a", "w1", &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("flow-b", "w2", &p2.id, false, "manual", None)
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
                .create_run("w1", &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(&format!("flow-{i}"), "w1", &p.id, false, "manual", None)
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
    fn test_list_workflow_runs_for_scope_scoped() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'feat-other', 'feat/other', '/tmp/ws/other', 'active', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let agent_mgr = AgentManager::new(&conn);
        let p1 = agent_mgr.create_run("w1", "wf1", None, None).unwrap();
        let p2 = agent_mgr.create_run("w2", "wf2", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        mgr.create_workflow_run("only-w1", "w1", &p1.id, false, "manual", None)
            .unwrap();
        mgr.create_workflow_run("only-w2", "w2", &p2.id, false, "manual", None)
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
                .create_run("w1", &format!("wf{i}"), None, None)
                .unwrap();
            mgr.create_workflow_run(&format!("flow-{i}"), "w1", &p.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("child-wf", "w1", &parent.id, false, "manual", None)
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
            worktree_id: String::new(),
            worktree_path: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run("test", "w1", &parent.id, false, "manual", None)
            .unwrap();

        ExecutionState {
            conn,
            config,
            workflow_run_id: run.id,
            workflow_name: "test".into(),
            worktree_id: "w1".into(),
            worktree_path: "/tmp/test".into(),
            worktree_slug: "test".into(),
            repo_path: "/tmp/repo".into(),
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("my-flow", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("my-flow", "w1", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w2", "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("other-flow", "w2", &parent.id, false, "manual", None)
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("running-wf", "w1", &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
            .unwrap();

        let workflow = make_empty_workflow();
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: "w1",
            worktree_path: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("done-wf", "w1", &parent.id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(&run.id, WorkflowRunStatus::Completed, Some("done"))
            .unwrap();

        let workflow = make_empty_workflow();
        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &workflow,
            worktree_id: "w1",
            worktree_path: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
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
        let parent = agent_mgr.create_run("w1", "workflow", None, None).unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("parent-wf", "w1", &parent.id, false, "manual", None)
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
            worktree_id: "w1",
            worktree_path: "/tmp/ws/feat-test",
            repo_path: "/tmp/repo",
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
}
