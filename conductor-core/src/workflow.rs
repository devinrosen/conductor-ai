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
use crate::workflow_dsl::{
    self, CallNode, GateNode, GateType, IfNode, OnMaxIter, OnTimeout, ParallelNode, WhileNode,
    WorkflowNode,
};

// Re-export DSL types so consumers go through `workflow::` instead of `workflow_dsl::` directly.
pub use crate::workflow_dsl::{
    collect_agent_names, AgentRef, InputDecl, WorkflowDef, WorkflowTrigger,
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
     gate_type, gate_prompt, gate_timeout, gate_approved_by, gate_approved_at, gate_feedback";

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
        };
        write!(f, "{s}")
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
        let now = Utc::now().to_rfc3339();
        let is_starting = status == WorkflowStepStatus::Running;
        let is_terminal = matches!(
            status,
            WorkflowStepStatus::Completed
                | WorkflowStepStatus::Failed
                | WorkflowStepStatus::Skipped
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
                 result_text = ?4, context_out = ?5, markers_out = ?6, retry_count = COALESCE(?7, retry_count) \
                 WHERE id = ?8",
                params![
                    status,
                    child_run_id,
                    now,
                    result_text,
                    context_out,
                    markers_out,
                    retry_count,
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

/// Build a fully-substituted agent prompt from the execution state and agent definition.
///
/// Handles: input variables, prior_context, prior_contexts, gate_feedback,
/// dry-run prefix for committing agents, and CONDUCTOR_OUTPUT instruction.
fn build_agent_prompt(state: &ExecutionState<'_>, agent_def: &agent_config::AgentDef) -> String {
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

    let mut prompt = substitute_variables(&agent_def.prompt, &vars);

    if agent_def.can_commit && state.exec_config.dry_run {
        prompt = format!("DO NOT commit or push any changes. This is a dry run.\n\n{prompt}");
    }

    prompt.push_str(CONDUCTOR_OUTPUT_INSTRUCTION);
    prompt
}

// ---------------------------------------------------------------------------
// Execution state
// ---------------------------------------------------------------------------

/// Mutable runtime state for a workflow execution.
struct ExecutionState<'a> {
    conn: &'a Connection,
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
    // Runtime
    step_results: HashMap<String, StepResult>,
    contexts: Vec<ContextEntry>,
    position: i64,
    all_succeeded: bool,
    total_cost: f64,
    total_turns: i64,
    total_duration_ms: i64,
    last_gate_feedback: Option<String>,
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

    let mut missing_agents = Vec::new();
    for agent_ref in &all_agents {
        if agent_config::load_agent(
            input.worktree_path,
            input.repo_path,
            &AgentSpec::from(agent_ref),
            Some(&workflow.name),
        )
        .is_err()
        {
            missing_agents.push(agent_ref.label().to_string());
        }
    }
    if !missing_agents.is_empty() {
        return Err(ConductorError::Workflow(format!(
            "Missing agent definitions: {}. Run 'conductor workflow validate' for details.",
            missing_agents.join(", ")
        )));
    }

    // Snapshot the definition
    let snapshot_json = serde_json::to_string(workflow).map_err(|e| {
        ConductorError::Workflow(format!("Failed to serialize workflow definition: {e}"))
    })?;

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
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
    };

    // Execute main body
    let body_result = execute_nodes(&mut state, &workflow.body);
    if let Err(ref e) = body_result {
        eprintln!("[workflow] Body execution error: {e}");
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
            eprintln!("[workflow] Always block error (non-fatal): {e}");
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
        eprintln!(
            "[workflow] Workflow '{}' completed successfully",
            workflow.name
        );
    } else {
        agent_mgr.update_run_failed(&parent_run.id, &summary)?;
        wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Failed, Some(&summary))?;
        eprintln!(
            "[workflow] Workflow '{}' finished with failures",
            workflow.name
        );
    }

    eprintln!(
        "[workflow] Total: ${:.4}, {} turns, {:.1}s",
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
    };

    execute_workflow(&input)
}

/// Walk a list of workflow nodes, dispatching to the appropriate handler.
fn execute_nodes(state: &mut ExecutionState<'_>, nodes: &[WorkflowNode]) -> Result<()> {
    for node in nodes {
        if !state.all_succeeded && state.exec_config.fail_fast {
            break;
        }
        match node {
            WorkflowNode::Call(n) => execute_call(state, n, 0)?,
            WorkflowNode::If(n) => execute_if(state, n)?,
            WorkflowNode::While(n) => execute_while(state, n)?,
            WorkflowNode::Parallel(n) => execute_parallel(state, n, 0)?,
            WorkflowNode::Gate(n) => execute_gate(state, n, 0)?,
            WorkflowNode::Always(n) => {
                // Nested always — just execute body
                execute_nodes(state, &n.body)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Node executors
// ---------------------------------------------------------------------------

fn execute_call(state: &mut ExecutionState<'_>, node: &CallNode, iteration: u32) -> Result<()> {
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
    // step_key is the short name used as the step_results map key so that
    // if/while conditions can reference this step by name regardless of whether
    // the agent was referenced by name or by explicit path.
    let step_key = node.agent.step_key();

    let prompt = build_agent_prompt(state, &agent_def);
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

        eprintln!(
            "[workflow] Step '{}' (attempt {}/{}): spawning in '{}'",
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
            eprintln!("[workflow] Failed to spawn child: {e}");
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

                // Parse CONDUCTOR_OUTPUT
                let output = completed_run
                    .result_text
                    .as_deref()
                    .and_then(parse_conductor_output)
                    .unwrap_or_default();

                let markers_json = serde_json::to_string(&output.markers).unwrap_or_default();

                if succeeded {
                    eprintln!(
                        "[workflow] Step '{}' completed: cost=${:.4}, {} turns, markers={:?}",
                        agent_label,
                        completed_run.cost_usd.unwrap_or(0.0),
                        completed_run.num_turns.unwrap_or(0),
                        output.markers,
                    );

                    state.wf_mgr.update_step_status(
                        &step_id,
                        WorkflowStepStatus::Completed,
                        Some(&completed_run.id),
                        completed_run.result_text.as_deref(),
                        Some(&output.context),
                        Some(&markers_json),
                        Some(attempt as i64),
                    )?;

                    // Update state
                    if let Some(cost) = completed_run.cost_usd {
                        state.total_cost += cost;
                    }
                    if let Some(turns) = completed_run.num_turns {
                        state.total_turns += turns;
                    }
                    if let Some(dur) = completed_run.duration_ms {
                        state.total_duration_ms += dur;
                    }

                    let step_result = StepResult {
                        step_name: agent_label.to_string(),
                        status: WorkflowStepStatus::Completed,
                        result_text: completed_run.result_text,
                        cost_usd: completed_run.cost_usd,
                        num_turns: completed_run.num_turns,
                        duration_ms: completed_run.duration_ms,
                        markers: output.markers,
                        context: output.context.clone(),
                        child_run_id: Some(completed_run.id),
                    };
                    state.step_results.insert(step_key.clone(), step_result);

                    state.contexts.push(ContextEntry {
                        step: agent_label.to_string(),
                        iteration,
                        context: output.context,
                    });

                    return Ok(());
                } else {
                    eprintln!(
                        "[workflow] Step '{}' failed (attempt {}/{}): {}",
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
                        Some(&output.context),
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
                eprintln!("[workflow] Step '{}' poll error: {e}", agent_label);
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
        eprintln!(
            "[workflow] All retries exhausted for '{}', running on_fail agent '{}'",
            agent_label,
            on_fail_agent.label(),
        );
        // Inject failure variables
        state
            .inputs
            .insert("failed_step".to_string(), agent_label.to_string());
        state
            .inputs
            .insert("failure_reason".to_string(), last_error.clone());
        state
            .inputs
            .insert("retry_count".to_string(), node.retries.to_string());

        let on_fail_node = CallNode {
            agent: on_fail_agent.clone(),
            retries: 0,
            on_fail: None,
        };
        if let Err(e) = execute_call(state, &on_fail_node, iteration) {
            eprintln!(
                "[workflow] on_fail agent '{}' also failed: {e}",
                on_fail_agent.label(),
            );
        }

        // Clean up injected vars
        state.inputs.remove("failed_step");
        state.inputs.remove("failure_reason");
        state.inputs.remove("retry_count");
    }

    state.all_succeeded = false;
    let step_result = StepResult {
        step_name: agent_label.to_string(),
        status: WorkflowStepStatus::Failed,
        result_text: Some(last_error),
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: Vec::new(),
        context: String::new(),
        child_run_id: None,
    };
    state.step_results.insert(step_key, step_result);

    if state.exec_config.fail_fast {
        return Err(ConductorError::Workflow(format!(
            "Step '{}' failed after {} attempts",
            agent_label, max_attempts
        )));
    }

    Ok(())
}

fn execute_if(state: &mut ExecutionState<'_>, node: &IfNode) -> Result<()> {
    let has_marker = state
        .step_results
        .get(&node.step)
        .map(|r| r.markers.iter().any(|m| m == &node.marker))
        .unwrap_or(false);

    if has_marker {
        eprintln!(
            "[workflow] if {}.{} — condition met, executing body",
            node.step, node.marker
        );
        execute_nodes(state, &node.body)?;
    } else {
        eprintln!(
            "[workflow] if {}.{} — condition not met, skipping",
            node.step, node.marker
        );
    }

    Ok(())
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
            eprintln!(
                "[workflow] while {}.{} — condition no longer met after {} iterations",
                node.step, node.marker, iteration
            );
            break;
        }

        if iteration >= node.max_iterations {
            eprintln!(
                "[workflow] while {}.{} — reached max_iterations ({})",
                node.step, node.marker, node.max_iterations
            );
            match node.on_max_iter {
                OnMaxIter::Fail => {
                    state.all_succeeded = false;
                    return Err(ConductorError::Workflow(format!(
                        "while {}.{} reached max_iterations ({})",
                        node.step, node.marker, node.max_iterations
                    )));
                }
                OnMaxIter::Continue => break,
            }
        }

        eprintln!(
            "[workflow] while {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body
        for body_node in &node.body {
            match body_node {
                WorkflowNode::Call(n) => execute_call(state, n, iteration)?,
                WorkflowNode::If(n) => execute_if(state, n)?,
                WorkflowNode::While(n) => execute_while(state, n)?,
                WorkflowNode::Parallel(n) => execute_parallel(state, n, iteration)?,
                WorkflowNode::Gate(n) => execute_gate(state, n, iteration)?,
                WorkflowNode::Always(n) => execute_nodes(state, &n.body)?,
            }

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            let current_markers: HashSet<String> = state
                .step_results
                .get(&node.step)
                .map(|r| r.markers.iter().cloned().collect())
                .unwrap_or_default();

            prev_marker_sets.push(current_markers.clone());

            if prev_marker_sets.len() >= stuck_after as usize {
                let window = &prev_marker_sets[prev_marker_sets.len() - stuck_after as usize..];
                if window.iter().all(|s| s == &current_markers) {
                    eprintln!(
                        "[workflow] while {}.{} — stuck: identical markers for {} consecutive iterations",
                        node.step, node.marker, stuck_after
                    );
                    state.all_succeeded = false;
                    return Err(ConductorError::Workflow(format!(
                        "while {}.{} stuck after {} iterations with identical markers",
                        node.step, node.marker, stuck_after
                    )));
                }
            }
        }

        iteration += 1;
    }

    Ok(())
}

fn execute_parallel(
    state: &mut ExecutionState<'_>,
    node: &ParallelNode,
    iteration: u32,
) -> Result<()> {
    let group_id = ulid::Ulid::new().to_string();
    let pos_base = state.position;

    eprintln!(
        "[workflow] parallel: spawning {} agents (fail_fast={}, min_success={:?})",
        node.calls.len(),
        node.fail_fast,
        node.min_success,
    );

    // Spawn all agents
    struct ParallelChild {
        agent_name: String,
        child_run_id: String,
        step_id: String,
        window_name: String,
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

        let prompt = build_agent_prompt(state, &agent_def);
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
            eprintln!("[workflow] Failed to spawn parallel agent '{agent_label}': {e}");
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
            eprintln!("[workflow] parallel: timeout reached");
            // Cancel remaining
            for (i, child) in children.iter().enumerate() {
                if !completed.contains(&i) {
                    if let Err(e) = state.agent_mgr.update_run_cancelled(&child.child_run_id) {
                        eprintln!(
                            "[workflow] parallel: failed to cancel run for '{}': {e}",
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
                        eprintln!(
                            "[workflow] parallel: failed to update timed-out step for '{}': {e}",
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

                        let output = run
                            .result_text
                            .as_deref()
                            .and_then(parse_conductor_output)
                            .unwrap_or_default();
                        let markers_json =
                            serde_json::to_string(&output.markers).unwrap_or_default();

                        let step_status = if succeeded {
                            successes += 1;
                            merged_markers.extend(output.markers.iter().cloned());
                            WorkflowStepStatus::Completed
                        } else {
                            failures += 1;
                            WorkflowStepStatus::Failed
                        };

                        if let Err(e) = state.wf_mgr.update_step_status(
                            &child.step_id,
                            step_status,
                            Some(&child.child_run_id),
                            run.result_text.as_deref(),
                            Some(&output.context),
                            Some(&markers_json),
                            None,
                        ) {
                            eprintln!(
                                "[workflow] parallel: failed to update step status for '{}': {e}",
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

                        eprintln!(
                            "[workflow] parallel: '{}' {} (cost=${:.4})",
                            child.agent_name,
                            if succeeded { "completed" } else { "failed" },
                            run.cost_usd.unwrap_or(0.0),
                        );

                        // fail_fast: cancel remaining on first failure
                        if !succeeded && node.fail_fast {
                            eprintln!("[workflow] parallel: fail_fast — cancelling remaining");
                            for (j, other) in children.iter().enumerate() {
                                if !completed.contains(&j) {
                                    if let Err(e) =
                                        state.agent_mgr.update_run_cancelled(&other.child_run_id)
                                    {
                                        eprintln!(
                                            "[workflow] parallel: failed to cancel run for '{}': {e}",
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
                                        eprintln!(
                                            "[workflow] parallel: failed to update step for '{}': {e}",
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
    eprintln!(
        "[workflow] parallel: {successes} succeeded, {failures} failed out of {} agents",
        children.len()
    );
    if successes < min_required {
        eprintln!(
            "[workflow] parallel: only {}/{} succeeded (min_success={})",
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
        eprintln!("[workflow] gate '{}': dry-run auto-approved", node.name);
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
            eprintln!("[workflow] Gate '{}' waiting for human action:", node.name);
            if let Some(ref p) = node.prompt {
                eprintln!("  Prompt: {p}");
            }
            eprintln!(
                "  Approve:  conductor workflow gate-approve {}",
                state.workflow_run_id
            );
            eprintln!(
                "  Reject:   conductor workflow gate-reject {}",
                state.workflow_run_id
            );
            if node.gate_type == GateType::HumanReview {
                eprintln!(
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
                        eprintln!("[workflow] Gate '{}' approved", node.name);
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
                        eprintln!("[workflow] Gate '{}' rejected", node.name);
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
            eprintln!(
                "[workflow] Gate '{}' polling for PR approvals...",
                node.name
            );
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
                                eprintln!(
                                    "[workflow] Gate '{}': {} approvals (required {})",
                                    node.name, approvals, node.min_approvals
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
            eprintln!("[workflow] Gate '{}' polling for PR checks...", node.name);
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
                                    eprintln!(
                                        "[workflow] Gate '{}': all checks passing",
                                        node.name
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
    eprintln!("[workflow] Gate '{}' timed out", node.name);
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
                WorkflowStepStatus::Completed,
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
    let completed = steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .count();
    let failed = steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Failed)
        .count();
    let skipped = steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Skipped)
        .count();

    let mut lines = Vec::new();
    lines.push(format!(
        "Workflow '{}': {completed}/{total} steps completed{}{}",
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
    ));

    for step in &steps {
        let marker = match step.status {
            WorkflowStepStatus::Completed => "ok",
            WorkflowStepStatus::Failed => "FAIL",
            WorkflowStepStatus::Skipped => "skip",
            WorkflowStepStatus::Running => "...",
            WorkflowStepStatus::Pending => "-",
            WorkflowStepStatus::Waiting => "wait",
        };
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
}
