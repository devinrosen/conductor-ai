use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A single step in an agent's two-phase execution plan.
/// Stored as individual records in the `agent_run_steps` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// ULID primary key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub description: String,
    /// Backward-compat flag derived from `status == "completed"`.
    #[serde(default)]
    pub done: bool,
    /// One of: pending, in_progress, completed, failed.
    #[serde(default = "default_step_status")]
    pub status: String,
    /// Ordering within the run's plan (0-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

fn default_step_status() -> String {
    "pending".to_string()
}

impl Default for PlanStep {
    fn default() -> Self {
        Self {
            id: None,
            description: String::new(),
            done: false,
            status: default_step_status(),
            position: None,
            started_at: None,
            completed_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub worktree_id: String,
    pub claude_session_id: Option<String>,
    pub prompt: String,
    pub status: String,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub tmux_window: Option<String>,
    pub log_file: Option<String>,
    /// The model used for this run (e.g. "claude-sonnet-4-6"). None means claude's default.
    pub model: Option<String>,
    /// Two-phase execution plan: JSON-serialized list of steps with completion state.
    pub plan: Option<Vec<PlanStep>>,
    /// If this is a child run, the ID of the parent (supervisor) run.
    pub parent_run_id: Option<String>,
}

impl AgentRun {
    /// Returns true if this run ended (failed/cancelled) with incomplete plan steps
    /// and has a session_id available for resume.
    pub fn needs_resume(&self) -> bool {
        matches!(self.status.as_str(), "failed" | "cancelled")
            && self.claude_session_id.is_some()
            && self.has_incomplete_plan_steps()
    }

    /// Returns true if the run has a plan with at least one incomplete step.
    pub fn has_incomplete_plan_steps(&self) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|steps| steps.iter().any(|s| !s.done))
    }

    /// Returns the incomplete plan steps (not yet done).
    pub fn incomplete_plan_steps(&self) -> Vec<&PlanStep> {
        self.plan
            .as_ref()
            .map(|steps| steps.iter().filter(|s| !s.done).collect())
            .unwrap_or_default()
    }

    /// Build a resume prompt from the remaining plan steps.
    pub fn build_resume_prompt(&self) -> String {
        let incomplete = self.incomplete_plan_steps();
        if incomplete.is_empty() {
            return "Continue where you left off.".to_string();
        }

        let mut prompt = String::from(
            "Continue where you left off. The following plan steps remain incomplete:\n",
        );
        for (i, step) in incomplete.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", i + 1, step.description));
        }
        prompt.push_str("\nPlease complete these remaining steps.");
        prompt
    }
}

/// Parsed JSON result from `claude -p --output-format json`.
#[derive(Debug, Deserialize)]
pub struct ClaudeJsonResult {
    pub session_id: Option<String>,
    pub result: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub is_error: Option<bool>,
}

/// A parsed display event from a stream-json agent log.
#[derive(Debug, Clone)]
pub struct AgentEvent {
    pub kind: String,
    pub summary: String,
}

/// A persisted agent run event (trace/span model) stored in `agent_run_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunEvent {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    pub summary: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub metadata: Option<String>,
}

impl AgentRunEvent {
    /// Duration in milliseconds, if both timestamps are present and parseable.
    pub fn duration_ms(&self) -> Option<i64> {
        let start = chrono::DateTime::parse_from_rfc3339(&self.started_at).ok()?;
        let end = chrono::DateTime::parse_from_rfc3339(self.ended_at.as_ref()?).ok()?;
        Some((end - start).num_milliseconds().max(0))
    }
}

/// Parse a single stream-json log line into zero or more display events.
pub fn parse_events_from_line(line: &str) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "system" => {
            let subtype = value.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            if subtype == "init" {
                let model = value
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                events.push(AgentEvent {
                    kind: "system".to_string(),
                    summary: format!("Session started (model: {model})"),
                });
            }
        }
        "assistant" => {
            let content = value
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());

            if let Some(blocks) = content {
                for block in blocks {
                    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match block_type {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                for text_line in text.lines() {
                                    let trimmed = text_line.trim();
                                    if !trimmed.is_empty() {
                                        events.push(AgentEvent {
                                            kind: "text".to_string(),
                                            summary: trimmed.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                        "tool_use" => {
                            let tool_name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let input = block.get("input");
                            let desc = tool_summary(tool_name, input);
                            events.push(AgentEvent {
                                kind: "tool".to_string(),
                                summary: desc,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        "result" => {
            let cost = value
                .get("total_cost_usd")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let turns = value.get("num_turns").and_then(|v| v.as_i64()).unwrap_or(0);
            let dur_ms = value
                .get("duration_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let dur_s = dur_ms as f64 / 1000.0;
            let is_error = value
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_error {
                let err_text = value
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                events.push(AgentEvent {
                    kind: "error".to_string(),
                    summary: format!("Error: {err_text}"),
                });
            } else {
                events.push(AgentEvent {
                    kind: "result".to_string(),
                    summary: format!("${cost:.4} · {turns} turns · {dur_s:.1}s"),
                });
            }
        }
        // Skip "user" and "rate_limit_event" — noise
        _ => {}
    }

    events
}

/// Parse a stream-json agent log file into displayable events.
/// Each line is a JSON object with a `type` field.
pub fn parse_agent_log(path: &str) -> Vec<AgentEvent> {
    let Ok(contents) = fs::read_to_string(Path::new(path)) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for line in contents.lines() {
        events.extend(parse_events_from_line(line));
    }
    events
}

/// Count the number of assistant turns in a stream-json agent log file.
/// Each JSON line with `"type": "assistant"` counts as one turn.
pub fn count_turns_in_log(path: &str) -> i64 {
    let Ok(contents) = fs::read_to_string(Path::new(path)) else {
        return 0;
    };

    let mut count: i64 = 0;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("assistant") {
            count += 1;
        }
    }
    count
}

/// Extract a human-readable summary for a tool_use event.
fn tool_summary(tool_name: &str, input: Option<&serde_json::Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return format!("[{tool_name}]"),
    };

    // Try description first (Bash always has this)
    if let Some(d) = input.get("description").and_then(|v| v.as_str()) {
        return format!("[{tool_name}] {d}");
    }

    // Try command (Bash fallback)
    if let Some(c) = input.get("command").and_then(|v| v.as_str()) {
        // Commands can be multi-line; take just the first line
        let first = c.lines().next().unwrap_or(c);
        return format!("[{tool_name}] {first}");
    }

    // Tool-specific field extraction
    let detail = match tool_name {
        "Read" | "Write" => input.get("file_path").and_then(|v| v.as_str()),
        "Edit" => input.get("file_path").and_then(|v| v.as_str()),
        "Glob" => input.get("pattern").and_then(|v| v.as_str()),
        "Grep" => input.get("pattern").and_then(|v| v.as_str()),
        "Agent" => input
            .get("description")
            .or_else(|| input.get("prompt"))
            .and_then(|v| v.as_str()),
        "WebFetch" => input.get("url").and_then(|v| v.as_str()),
        "WebSearch" => input.get("query").and_then(|v| v.as_str()),
        _ => None,
    };

    match detail {
        Some(d) => format!("[{tool_name}] {d}"),
        None => format!("[{tool_name}]"),
    }
}

/// A GitHub issue (or other tracker issue) created by an agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreatedIssue {
    pub id: String,
    pub agent_run_id: String,
    pub repo_id: String,
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub url: String,
    pub created_at: String,
}

/// Aggregated agent stats for a ticket (across all linked worktrees).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TicketAgentTotals {
    pub ticket_id: String,
    pub total_runs: i64,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
}

/// Aggregated stats for a run tree (parent + all descendants).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunTreeTotals {
    pub total_runs: i64,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
}

pub struct AgentManager<'a> {
    conn: &'a Connection,
}

impl<'a> AgentManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_run(
        &self,
        worktree_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(worktree_id, prompt, tmux_window, model, None)
    }

    pub fn create_child_run(
        &self,
        worktree_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
        parent_run_id: &str,
    ) -> Result<AgentRun> {
        self.create_run_with_parent(worktree_id, prompt, tmux_window, model, Some(parent_run_id))
    }

    fn create_run_with_parent(
        &self,
        worktree_id: &str,
        prompt: &str,
        tmux_window: Option<&str>,
        model: Option<&str>,
        parent_run_id: Option<&str>,
    ) -> Result<AgentRun> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();

        let run = AgentRun {
            id: id.clone(),
            worktree_id: worktree_id.to_string(),
            claude_session_id: None,
            prompt: prompt.to_string(),
            status: "running".to_string(),
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: now.clone(),
            ended_at: None,
            tmux_window: tmux_window.map(String::from),
            log_file: None,
            model: model.map(String::from),
            plan: None,
            parent_run_id: parent_run_id.map(String::from),
        };

        self.conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at, tmux_window, model, parent_run_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.id,
                run.worktree_id,
                run.prompt,
                run.status,
                run.started_at,
                run.tmux_window,
                run.model,
                run.parent_run_id
            ],
        )?;

        Ok(run)
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window, log_file, \
             model, plan, parent_run_id \
             FROM agent_runs WHERE id = ?1",
            params![run_id],
            row_to_agent_run,
        );

        match result {
            Ok(mut run) => {
                let steps = self.get_run_steps(&run.id)?;
                run.plan = if steps.is_empty() { None } else { Some(steps) };
                Ok(Some(run))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_run_completed(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        result_text: Option<&str>,
        cost_usd: Option<f64>,
        num_turns: Option<i64>,
        duration_ms: Option<i64>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'completed', claude_session_id = ?1, \
             result_text = ?2, cost_usd = ?3, num_turns = ?4, duration_ms = ?5, \
             ended_at = ?6 WHERE id = ?7",
            params![
                session_id,
                result_text,
                cost_usd,
                num_turns,
                duration_ms,
                now,
                run_id
            ],
        )?;
        Ok(())
    }

    pub fn update_run_failed(&self, run_id: &str, error: &str) -> Result<()> {
        self.update_run_failed_with_session(run_id, error, None)
    }

    /// Mark a run as failed, optionally preserving the session_id for resume.
    pub fn update_run_failed_with_session(
        &self,
        run_id: &str,
        error: &str,
        session_id: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'failed', result_text = ?1, ended_at = ?2, \
             claude_session_id = COALESCE(?3, claude_session_id) \
             WHERE id = ?4",
            params![error, now, session_id, run_id],
        )?;
        Ok(())
    }

    pub fn update_run_cancelled(&self, run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_runs SET status = 'cancelled', ended_at = ?1 WHERE id = ?2",
            params![now, run_id],
        )?;
        Ok(())
    }

    /// Save the claude session_id as soon as it's known (before run completes).
    /// This enables resume even if the run fails or is cancelled.
    pub fn update_run_session_id(&self, run_id: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET claude_session_id = ?1 WHERE id = ?2",
            params![session_id, run_id],
        )?;
        Ok(())
    }

    pub fn update_run_log_file(&self, run_id: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_runs SET log_file = ?1 WHERE id = ?2",
            params![path, run_id],
        )?;
        Ok(())
    }

    /// Store the two-phase plan for a run. Replaces any existing plan steps.
    /// Inserts individual records into `agent_run_steps`.
    pub fn update_run_plan(&self, run_id: &str, steps: &[PlanStep]) -> Result<()> {
        // Delete any existing steps for this run.
        self.conn.execute(
            "DELETE FROM agent_run_steps WHERE run_id = ?1",
            params![run_id],
        )?;

        for (i, step) in steps.iter().enumerate() {
            let step_id = ulid::Ulid::new().to_string();
            let status = if step.done { "completed" } else { "pending" };
            self.conn.execute(
                "INSERT INTO agent_run_steps (id, run_id, position, description, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![step_id, run_id, i as i64, step.description, status],
            )?;
        }

        Ok(())
    }

    /// Mark all steps in the plan as completed (called on successful run completion).
    pub fn mark_plan_done(&self, run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_run_steps SET status = 'completed', completed_at = ?1 \
             WHERE run_id = ?2 AND status != 'completed'",
            params![now, run_id],
        )?;
        Ok(())
    }

    /// Update the status of a single plan step.
    pub fn update_step_status(&self, step_id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        match status {
            "in_progress" => {
                self.conn.execute(
                    "UPDATE agent_run_steps SET status = ?1, started_at = ?2 WHERE id = ?3",
                    params![status, now, step_id],
                )?;
            }
            "completed" | "failed" => {
                self.conn.execute(
                    "UPDATE agent_run_steps SET status = ?1, completed_at = ?2 WHERE id = ?3",
                    params![status, now, step_id],
                )?;
            }
            _ => {
                self.conn.execute(
                    "UPDATE agent_run_steps SET status = ?1 WHERE id = ?2",
                    params![status, step_id],
                )?;
            }
        }
        Ok(())
    }

    /// Get all plan steps for a run, ordered by position.
    pub fn get_run_steps(&self, run_id: &str) -> Result<Vec<PlanStep>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, position, description, status, started_at, completed_at \
             FROM agent_run_steps WHERE run_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![run_id], row_to_plan_step)?;
        let steps = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(steps)
    }

    /// Populate the `plan` field on a slice of runs from the steps table.
    fn populate_plans(&self, runs: &mut [AgentRun]) -> Result<()> {
        if runs.is_empty() {
            return Ok(());
        }

        // Build a set of run IDs and fetch all steps at once.
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, run_id, position, description, status, started_at, completed_at \
             FROM agent_run_steps WHERE run_id IN ({placeholders}) ORDER BY position ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&ids), |row| {
            let run_id: String = row.get(1)?;
            let step = row_to_plan_step(row)?;
            Ok((run_id, step))
        })?;

        let mut steps_map: HashMap<String, Vec<PlanStep>> = HashMap::new();
        for row in rows {
            let (run_id, step) = row?;
            steps_map.entry(run_id).or_default().push(step);
        }

        for run in runs.iter_mut() {
            if let Some(steps) = steps_map.remove(&run.id) {
                run.plan = if steps.is_empty() { None } else { Some(steps) };
            }
        }
        Ok(())
    }

    pub fn list_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window, log_file, \
             model, plan, parent_run_id \
             FROM agent_runs WHERE worktree_id = ?1 ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(params![worktree_id], row_to_agent_run)?;
        let mut runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// Returns true if the worktree has any prior agent runs.
    pub fn has_runs_for_worktree(&self, worktree_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE worktree_id = ?1",
            params![worktree_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn latest_for_worktree(&self, worktree_id: &str) -> Result<Option<AgentRun>> {
        let result = self.conn.query_row(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window, log_file, \
             model, plan, parent_run_id \
             FROM agent_runs WHERE worktree_id = ?1 ORDER BY started_at DESC LIMIT 1",
            params![worktree_id],
            row_to_agent_run,
        );

        match result {
            Ok(mut run) => {
                let steps = self.get_run_steps(&run.id)?;
                run.plan = if steps.is_empty() { None } else { Some(steps) };
                Ok(Some(run))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Returns aggregated agent stats per ticket (across all linked worktrees).
    /// Only includes completed runs with recorded metrics.
    pub fn totals_by_ticket_all(&self) -> Result<HashMap<String, TicketAgentTotals>> {
        let mut stmt = self.conn.prepare(
            "SELECT w.ticket_id, \
                    COUNT(*) AS total_runs, \
                    COALESCE(SUM(a.cost_usd), 0.0) AS total_cost, \
                    COALESCE(SUM(a.num_turns), 0) AS total_turns, \
                    COALESCE(SUM(a.duration_ms), 0) AS total_duration_ms \
             FROM agent_runs a \
             JOIN worktrees w ON a.worktree_id = w.id \
             WHERE w.ticket_id IS NOT NULL AND a.status = 'completed' \
             GROUP BY w.ticket_id",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(TicketAgentTotals {
                ticket_id: row.get(0)?,
                total_runs: row.get(1)?,
                total_cost: row.get(2)?,
                total_turns: row.get(3)?,
                total_duration_ms: row.get(4)?,
            })
        })?;

        let mut map = HashMap::new();
        for totals in rows {
            let totals = totals?;
            map.insert(totals.ticket_id.clone(), totals);
        }
        Ok(map)
    }

    /// Persist a new event span for a run. Returns the created event.
    pub fn create_event(
        &self,
        run_id: &str,
        kind: &str,
        summary: &str,
        started_at: &str,
        metadata: Option<&str>,
    ) -> Result<AgentRunEvent> {
        let id = ulid::Ulid::new().to_string();
        let event = AgentRunEvent {
            id: id.clone(),
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            summary: summary.to_string(),
            started_at: started_at.to_string(),
            ended_at: None,
            metadata: metadata.map(String::from),
        };
        self.conn.execute(
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.run_id,
                event.kind,
                event.summary,
                event.started_at,
                event.metadata
            ],
        )?;
        Ok(event)
    }

    /// Set the ended_at timestamp for a previously created event span.
    pub fn update_event_ended_at(&self, event_id: &str, ended_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agent_run_events SET ended_at = ?1 WHERE id = ?2",
            params![ended_at, event_id],
        )?;
        Ok(())
    }

    /// List all events for a run in chronological order.
    pub fn list_events_for_run(&self, run_id: &str) -> Result<Vec<AgentRunEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, kind, summary, started_at, ended_at, metadata \
             FROM agent_run_events WHERE run_id = ?1 ORDER BY started_at ASC",
        )?;
        let rows = stmt.query_map(params![run_id], row_to_agent_run_event)?;
        let events = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    /// List all events across all runs for a worktree, in chronological order.
    pub fn list_events_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRunEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.run_id, e.kind, e.summary, e.started_at, e.ended_at, e.metadata \
             FROM agent_run_events e \
             JOIN agent_runs r ON e.run_id = r.id \
             WHERE r.worktree_id = ?1 \
             ORDER BY e.started_at ASC",
        )?;
        let rows = stmt.query_map(params![worktree_id], row_to_agent_run_event)?;
        let events = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    /// Record a GitHub issue created by an agent run.
    pub fn record_created_issue(
        &self,
        agent_run_id: &str,
        repo_id: &str,
        source_type: &str,
        source_id: &str,
        title: &str,
        url: &str,
    ) -> Result<AgentCreatedIssue> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();

        let issue = AgentCreatedIssue {
            id: id.clone(),
            agent_run_id: agent_run_id.to_string(),
            repo_id: repo_id.to_string(),
            source_type: source_type.to_string(),
            source_id: source_id.to_string(),
            title: title.to_string(),
            url: url.to_string(),
            created_at: now.clone(),
        };

        self.conn.execute(
            "INSERT INTO agent_created_issues \
             (id, agent_run_id, repo_id, source_type, source_id, title, url, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                issue.id,
                issue.agent_run_id,
                issue.repo_id,
                issue.source_type,
                issue.source_id,
                issue.title,
                issue.url,
                issue.created_at,
            ],
        )?;

        Ok(issue)
    }

    /// List all issues created by a specific agent run.
    pub fn list_created_issues_for_run(
        &self,
        agent_run_id: &str,
    ) -> Result<Vec<AgentCreatedIssue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_run_id, repo_id, source_type, source_id, title, url, created_at \
             FROM agent_created_issues WHERE agent_run_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![agent_run_id], row_to_agent_created_issue)?;
        let issues = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(issues)
    }

    /// List all issues created by all runs for a worktree.
    pub fn list_created_issues_for_worktree(
        &self,
        worktree_id: &str,
    ) -> Result<Vec<AgentCreatedIssue>> {
        let mut stmt = self.conn.prepare(
            "SELECT aci.id, aci.agent_run_id, aci.repo_id, aci.source_type, \
             aci.source_id, aci.title, aci.url, aci.created_at \
             FROM agent_created_issues aci \
             JOIN agent_runs ar ON aci.agent_run_id = ar.id \
             WHERE ar.worktree_id = ?1 \
             ORDER BY aci.created_at ASC",
        )?;
        let rows = stmt.query_map(params![worktree_id], row_to_agent_created_issue)?;
        let issues = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(issues)
    }

    /// Returns the latest agent run for each worktree, keyed by worktree_id.
    pub fn latest_runs_by_worktree(&self) -> Result<HashMap<String, AgentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.worktree_id, a.claude_session_id, a.prompt, a.status, \
             a.result_text, a.cost_usd, a.num_turns, a.duration_ms, a.started_at, \
             a.ended_at, a.tmux_window, a.log_file, a.model, a.plan, a.parent_run_id \
             FROM agent_runs a \
             INNER JOIN ( \
                 SELECT worktree_id, MAX(started_at) AS max_started \
                 FROM agent_runs GROUP BY worktree_id \
             ) latest ON a.worktree_id = latest.worktree_id AND a.started_at = latest.max_started",
        )?;

        let rows = stmt.query_map([], row_to_agent_run)?;
        let mut runs: Vec<AgentRun> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        self.populate_plans(&mut runs)?;
        let mut map = HashMap::new();
        for run in runs {
            map.insert(run.worktree_id.clone(), run);
        }
        Ok(map)
    }

    // ── Parent/child run tree queries ─────────────────────────────────

    /// List direct child runs of a parent run (newest first).
    pub fn list_child_runs(&self, parent_run_id: &str) -> Result<Vec<AgentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window, log_file, \
             model, plan, parent_run_id \
             FROM agent_runs WHERE parent_run_id = ?1 ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(params![parent_run_id], row_to_agent_run)?;
        let mut runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// Get a full run tree: the given run plus all descendants (children, grandchildren, etc.).
    /// Returns a flat list ordered by started_at ASC. The caller can reconstruct
    /// the tree using `parent_run_id` references.
    pub fn get_run_tree(&self, root_run_id: &str) -> Result<Vec<AgentRun>> {
        // SQLite supports recursive CTEs, which are perfect for tree traversal.
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM agent_runs WHERE id = ?1 \
                 UNION ALL \
                 SELECT a.id FROM agent_runs a JOIN tree t ON a.parent_run_id = t.id \
             ) \
             SELECT a.id, a.worktree_id, a.claude_session_id, a.prompt, a.status, \
                    a.result_text, a.cost_usd, a.num_turns, a.duration_ms, a.started_at, \
                    a.ended_at, a.tmux_window, a.log_file, a.model, a.plan, a.parent_run_id \
             FROM agent_runs a \
             JOIN tree t ON a.id = t.id \
             ORDER BY a.started_at ASC",
        )?;
        let rows = stmt.query_map(params![root_run_id], row_to_agent_run)?;
        let mut runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// List only top-level (root) runs for a worktree — runs with no parent.
    pub fn list_root_runs_for_worktree(&self, worktree_id: &str) -> Result<Vec<AgentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, worktree_id, claude_session_id, prompt, status, result_text, \
             cost_usd, num_turns, duration_ms, started_at, ended_at, tmux_window, log_file, \
             model, plan, parent_run_id \
             FROM agent_runs WHERE worktree_id = ?1 AND parent_run_id IS NULL \
             ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map(params![worktree_id], row_to_agent_run)?;
        let mut runs = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        self.populate_plans(&mut runs)?;
        Ok(runs)
    }

    /// Compute aggregated cost/turns/duration for a run and all its descendants.
    pub fn aggregate_run_tree(&self, root_run_id: &str) -> Result<RunTreeTotals> {
        let row = self.conn.query_row(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM agent_runs WHERE id = ?1 \
                 UNION ALL \
                 SELECT a.id FROM agent_runs a JOIN tree t ON a.parent_run_id = t.id \
             ) \
             SELECT COUNT(*) AS total_runs, \
                    COALESCE(SUM(a.cost_usd), 0.0) AS total_cost, \
                    COALESCE(SUM(a.num_turns), 0) AS total_turns, \
                    COALESCE(SUM(a.duration_ms), 0) AS total_duration_ms \
             FROM agent_runs a \
             JOIN tree t ON a.id = t.id \
             WHERE a.status = 'completed'",
            params![root_run_id],
            |row| {
                Ok(RunTreeTotals {
                    total_runs: row.get(0)?,
                    total_cost: row.get(1)?,
                    total_turns: row.get(2)?,
                    total_duration_ms: row.get(3)?,
                })
            },
        )?;
        Ok(row)
    }
}

/// Build a startup context block to prepend to the agent prompt.
///
/// Pulls worktree info, linked ticket, prior run plans, recent commits,
/// and prior run summaries from the database. Returns `None` if there is
/// no useful context to inject (e.g. first run with no linked ticket).
pub fn build_startup_context(
    conn: &Connection,
    worktree_id: &str,
    current_run_id: &str,
    worktree_path: &str,
) -> Option<String> {
    let mut sections = Vec::new();

    // 1. Worktree branch
    let branch: Option<String> = conn
        .query_row(
            "SELECT branch FROM worktrees WHERE id = ?1",
            params![worktree_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref branch) = branch {
        sections.push(format!("**Worktree:** {branch}"));
    }

    // 2. Linked ticket
    let ticket_info: Option<(String, String)> = conn
        .query_row(
            "SELECT t.source_id, t.title FROM tickets t \
             JOIN worktrees w ON w.ticket_id = t.id \
             WHERE w.id = ?1",
            params![worktree_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();

    if let Some((source_id, title)) = ticket_info {
        sections.push(format!("**Ticket:** #{source_id} — {title}"));
    }

    // 3. Prior runs (excluding the current run being started)
    let mgr = AgentManager::new(conn);
    if let Ok(runs) = mgr.list_for_worktree(worktree_id) {
        let prior_runs: Vec<&AgentRun> = runs.iter().filter(|r| r.id != current_run_id).collect();

        // Plan steps from the most recent run that has a plan
        if let Some(run_with_plan) = prior_runs.iter().find(|r| r.plan.is_some()) {
            if let Some(ref plan) = run_with_plan.plan {
                let plan_lines: Vec<String> = plan
                    .iter()
                    .enumerate()
                    .map(|(i, step)| {
                        let marker = if step.done { "✅" } else { "⏳" };
                        format!("{}. {} {}", i + 1, marker, step.description)
                    })
                    .collect();
                if !plan_lines.is_empty() {
                    sections.push(format!(
                        "**Plan steps (from prior run):**\n{}",
                        plan_lines.join("\n")
                    ));
                }
            }
        }

        // Prior run summary (from last completed or failed run)
        if let Some(last_run) = prior_runs
            .iter()
            .find(|r| r.status == "completed" || r.status == "failed")
        {
            if let Some(ref result) = last_run.result_text {
                let truncated = if result.len() > 500 {
                    format!("{}…", &result[..500])
                } else {
                    result.clone()
                };
                sections.push(format!(
                    "**Prior run outcome ({}):** {}",
                    last_run.status, truncated
                ));
            }
        }
    }

    // 4. Recent commits via git log
    let commits = Command::new("git")
        .args(["log", "--oneline", "-10"])
        .current_dir(worktree_path)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        });

    if let Some(ref commit_output) = commits {
        let lines: Vec<&str> = commit_output.lines().collect();
        if !lines.is_empty() {
            let commit_lines: Vec<String> = lines.iter().map(|l| format!("- {l}")).collect();
            sections.push(format!(
                "**Recent commits in this worktree:**\n{}",
                commit_lines.join("\n")
            ));
        }
    }

    if sections.is_empty() {
        return None;
    }

    Some(format!("## Session Context\n\n{}", sections.join("\n\n")))
}

fn row_to_agent_run_event(row: &rusqlite::Row) -> rusqlite::Result<AgentRunEvent> {
    Ok(AgentRunEvent {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        summary: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        metadata: row.get(6)?,
    })
}

fn row_to_agent_created_issue(row: &rusqlite::Row) -> rusqlite::Result<AgentCreatedIssue> {
    Ok(AgentCreatedIssue {
        id: row.get(0)?,
        agent_run_id: row.get(1)?,
        repo_id: row.get(2)?,
        source_type: row.get(3)?,
        source_id: row.get(4)?,
        title: row.get(5)?,
        url: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn row_to_agent_run(row: &rusqlite::Row) -> rusqlite::Result<AgentRun> {
    // Plan is populated separately from agent_run_steps table by the caller.
    // Column 14 (plan JSON) is still selected for SQL compatibility but ignored.
    Ok(AgentRun {
        id: row.get(0)?,
        worktree_id: row.get(1)?,
        claude_session_id: row.get(2)?,
        prompt: row.get(3)?,
        status: row.get(4)?,
        result_text: row.get(5)?,
        cost_usd: row.get(6)?,
        num_turns: row.get(7)?,
        duration_ms: row.get(8)?,
        started_at: row.get(9)?,
        ended_at: row.get(10)?,
        tmux_window: row.get(11)?,
        log_file: row.get(12)?,
        model: row.get(13)?,
        plan: None,
        parent_run_id: row.get(15)?,
    })
}

fn row_to_plan_step(row: &rusqlite::Row) -> rusqlite::Result<PlanStep> {
    let status: String = row.get(4)?;
    let done = status == "completed";
    Ok(PlanStep {
        id: Some(row.get(0)?),
        description: row.get(3)?,
        done,
        status,
        position: Some(row.get(2)?),
        started_at: row.get(5)?,
        completed_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db::migrations::run(&conn).unwrap();
        // Insert a repo and worktree for FK constraints
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
             VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('w2', 'r1', 'fix-bug', 'fix/bug', '/tmp/ws/fix-bug', 'active', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn
    }

    #[test]
    fn test_create_and_list() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(run.prompt, "Fix the bug");
        assert!(run.tmux_window.is_none());

        let runs = mgr.list_for_worktree("w1").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
    }

    #[test]
    fn test_create_with_tmux_window() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run("w1", "Fix the bug", Some("feat-test"), None)
            .unwrap();
        assert_eq!(run.tmux_window.as_deref(), Some("feat-test"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.tmux_window.as_deref(), Some("feat-test"));
    }

    #[test]
    fn test_get_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.id, run.id);
        assert_eq!(fetched.prompt, "Fix the bug");

        let missing = mgr.get_run("nonexistent").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_update_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        mgr.update_run_completed(
            &run.id,
            Some("sess-123"),
            Some("Done!"),
            Some(0.05),
            Some(3),
            Some(15000),
        )
        .unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, "completed");
        assert_eq!(latest.claude_session_id.as_deref(), Some("sess-123"));
        assert_eq!(latest.cost_usd, Some(0.05));
    }

    #[test]
    fn test_update_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        mgr.update_run_failed(&run.id, "Something went wrong")
            .unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, "failed");
        assert_eq!(latest.result_text.as_deref(), Some("Something went wrong"));
    }

    #[test]
    fn test_update_cancelled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let latest = mgr.latest_for_worktree("w1").unwrap().unwrap();
        assert_eq!(latest.status, "cancelled");
        assert!(latest.ended_at.is_some());
    }

    #[test]
    fn test_latest_for_worktree_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let result = mgr.latest_for_worktree("w1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_has_runs_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // No runs yet
        assert!(!mgr.has_runs_for_worktree("w1").unwrap());

        // Create a run
        mgr.create_run("w1", "First prompt", None, None).unwrap();
        assert!(mgr.has_runs_for_worktree("w1").unwrap());

        // Different worktree still has no runs
        assert!(!mgr.has_runs_for_worktree("w2").unwrap());
    }

    #[test]
    fn test_latest_runs_by_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create runs for two different worktrees
        let _run1 = mgr.create_run("w1", "First prompt", None, None).unwrap();
        let run2 = mgr.create_run("w1", "Second prompt", None, None).unwrap();
        let run3 = mgr.create_run("w2", "Other prompt", None, None).unwrap();

        let map = mgr.latest_runs_by_worktree().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("w1").unwrap().id, run2.id);
        assert_eq!(map.get("w2").unwrap().id, run3.id);
    }

    #[test]
    fn test_update_log_file() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run("w1", "Fix the bug", Some("feat-test"), None)
            .unwrap();
        assert!(run.log_file.is_none());

        mgr.update_run_log_file(&run.id, "/tmp/agent-logs/test.log")
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            fetched.log_file.as_deref(),
            Some("/tmp/agent-logs/test.log")
        );
    }

    #[test]
    fn test_totals_by_ticket_all() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a ticket and link w1 to it
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'Test ticket', '', 'open', '', 'https://example.com', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w1'", [])
            .unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w2'", [])
            .unwrap();

        // Create completed runs on both worktrees
        let run1 = mgr.create_run("w1", "First task", None, None).unwrap();
        mgr.update_run_completed(&run1.id, None, None, Some(0.10), Some(5), Some(30000))
            .unwrap();
        let run2 = mgr.create_run("w1", "Second task", None, None).unwrap();
        mgr.update_run_completed(&run2.id, None, None, Some(0.05), Some(3), Some(15000))
            .unwrap();
        let run3 = mgr.create_run("w2", "Third task", None, None).unwrap();
        mgr.update_run_completed(&run3.id, None, None, Some(0.08), Some(4), Some(20000))
            .unwrap();

        // Create a running run (should NOT be included)
        let _run4 = mgr.create_run("w1", "In progress", None, None).unwrap();

        let totals = mgr.totals_by_ticket_all().unwrap();
        assert_eq!(totals.len(), 1);

        let t1 = totals.get("t1").unwrap();
        assert_eq!(t1.total_runs, 3);
        assert!((t1.total_cost - 0.23).abs() < 0.001);
        assert_eq!(t1.total_turns, 12);
        assert_eq!(t1.total_duration_ms, 65000);
    }

    #[test]
    fn test_totals_by_ticket_empty() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let totals = mgr.totals_by_ticket_all().unwrap();
        assert!(totals.is_empty());
    }

    #[test]
    fn test_update_run_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        assert!(run.plan.is_none());

        let steps = vec![
            PlanStep {
                description: "Investigate the issue".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Write a fix".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        let plan = fetched.plan.unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].description, "Investigate the issue");
        assert!(!plan[0].done);
        assert_eq!(plan[0].status, "pending");
        assert!(plan[0].id.is_some());
        assert_eq!(plan[0].position, Some(0));
        assert_eq!(plan[1].description, "Write a fix");
        assert!(!plan[1].done);
        assert_eq!(plan[1].position, Some(1));
    }

    #[test]
    fn test_mark_plan_done() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![
            PlanStep {
                description: "Step one".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Step two".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.mark_plan_done(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        let plan = fetched.plan.unwrap();
        assert!(plan[0].done);
        assert_eq!(plan[0].status, "completed");
        assert!(plan[0].completed_at.is_some());
        assert!(plan[1].done);
        assert_eq!(plan[1].status, "completed");
        assert!(plan[1].completed_at.is_some());
    }

    #[test]
    fn test_mark_plan_done_no_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        // Should not error when no plan exists
        mgr.mark_plan_done(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.plan.is_none());
    }

    #[test]
    fn test_plan_roundtrip_in_latest_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![PlanStep {
            description: "Do the thing".to_string(),
            done: true,
            status: "completed".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let map = mgr.latest_runs_by_worktree().unwrap();
        let latest = map.get("w1").unwrap();
        let plan = latest.plan.as_ref().unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].description, "Do the thing");
        assert!(plan[0].done);
    }

    #[test]
    fn test_update_step_status() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![
            PlanStep {
                description: "Step one".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Step two".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        // Get the step IDs
        let stored = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(stored.len(), 2);

        // Mark first step in_progress
        let step_id = stored[0].id.as_ref().unwrap();
        mgr.update_step_status(step_id, "in_progress").unwrap();
        let updated = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(updated[0].status, "in_progress");
        assert!(updated[0].started_at.is_some());
        assert!(!updated[0].done);

        // Mark first step completed
        mgr.update_step_status(step_id, "completed").unwrap();
        let updated = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(updated[0].status, "completed");
        assert!(updated[0].completed_at.is_some());
        assert!(updated[0].done);
        // Second step still pending
        assert_eq!(updated[1].status, "pending");
        assert!(!updated[1].done);
    }

    #[test]
    fn test_update_step_status_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![PlanStep {
            description: "Step one".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let stored = mgr.get_run_steps(&run.id).unwrap();
        let step_id = stored[0].id.as_ref().unwrap();
        mgr.update_step_status(step_id, "failed").unwrap();

        let updated = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(updated[0].status, "failed");
        assert!(updated[0].completed_at.is_some());
        assert!(!updated[0].done);
    }

    #[test]
    fn test_get_run_steps_ordering() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![
            PlanStep {
                description: "First".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Second".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Third".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let stored = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].description, "First");
        assert_eq!(stored[0].position, Some(0));
        assert_eq!(stored[1].description, "Second");
        assert_eq!(stored[1].position, Some(1));
        assert_eq!(stored[2].description, "Third");
        assert_eq!(stored[2].position, Some(2));
    }

    #[test]
    fn test_update_run_plan_replaces_existing() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps1 = vec![PlanStep {
            description: "Old step".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps1).unwrap();

        let steps2 = vec![
            PlanStep {
                description: "New step one".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "New step two".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps2).unwrap();

        let stored = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].description, "New step one");
        assert_eq!(stored[1].description, "New step two");
    }

    #[test]
    fn test_record_and_list_created_issues() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run1 = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let run2 = mgr.create_run("w2", "Other task", None, None).unwrap();

        // No issues yet
        assert!(mgr
            .list_created_issues_for_run(&run1.id)
            .unwrap()
            .is_empty());
        assert!(mgr
            .list_created_issues_for_worktree("w1")
            .unwrap()
            .is_empty());

        // Record two issues on run1
        let issue1 = mgr
            .record_created_issue(
                &run1.id,
                "r1",
                "github",
                "101",
                "Found a memory leak",
                "https://github.com/test/repo/issues/101",
            )
            .unwrap();
        let issue2 = mgr
            .record_created_issue(
                &run1.id,
                "r1",
                "github",
                "102",
                "Needs follow-up refactor",
                "https://github.com/test/repo/issues/102",
            )
            .unwrap();

        // Record one issue on run2
        mgr.record_created_issue(
            &run2.id,
            "r1",
            "github",
            "103",
            "Another issue",
            "https://github.com/test/repo/issues/103",
        )
        .unwrap();

        // list_for_run returns only run1's issues
        let run1_issues = mgr.list_created_issues_for_run(&run1.id).unwrap();
        assert_eq!(run1_issues.len(), 2);
        assert_eq!(run1_issues[0].source_id, "101");
        assert_eq!(run1_issues[1].source_id, "102");
        assert_eq!(run1_issues[0].title, "Found a memory leak");

        // list_for_worktree returns issues from all runs on w1 (only run1 here)
        let w1_issues = mgr.list_created_issues_for_worktree("w1").unwrap();
        assert_eq!(w1_issues.len(), 2);
        assert_eq!(w1_issues[0].id, issue1.id);
        assert_eq!(w1_issues[1].id, issue2.id);

        // w2 has its own issue
        let w2_issues = mgr.list_created_issues_for_worktree("w2").unwrap();
        assert_eq!(w2_issues.len(), 1);
        assert_eq!(w2_issues[0].source_id, "103");
    }

    #[test]
    fn test_claude_json_result_deserialization() {
        let json = r#"{"session_id":"sess-abc","result":"Final output","cost_usd":0.05,"num_turns":3,"duration_ms":15000,"is_error":false}"#;
        let result: ClaudeJsonResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(result.cost_usd, Some(0.05));
        assert_eq!(result.num_turns, Some(3));
        assert_eq!(result.duration_ms, Some(15000));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn test_create_and_list_events() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let t0 = "2024-01-01T00:00:00Z";
        let t1 = "2024-01-01T00:00:02Z";
        let t2 = "2024-01-01T00:00:05Z";

        let ev1 = mgr
            .create_event(&run.id, "system", "Session started", t0, None)
            .unwrap();
        let ev2 = mgr
            .create_event(&run.id, "tool", "[Bash] cargo build", t1, None)
            .unwrap();
        mgr.update_event_ended_at(&ev1.id, t1).unwrap();
        mgr.update_event_ended_at(&ev2.id, t2).unwrap();

        let events = mgr.list_events_for_run(&run.id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "system");
        assert_eq!(events[0].ended_at.as_deref(), Some(t1));
        assert_eq!(events[1].kind, "tool");
        assert_eq!(events[1].summary, "[Bash] cargo build");
        assert_eq!(events[1].ended_at.as_deref(), Some(t2));

        // duration_ms computed from timestamps
        let dur = events[1].duration_ms().unwrap();
        assert_eq!(dur, 3000);
    }

    #[test]
    fn test_list_events_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run1 = mgr.create_run("w1", "First task", None, None).unwrap();
        let run2 = mgr.create_run("w1", "Second task", None, None).unwrap();
        let run3 = mgr.create_run("w2", "Other task", None, None).unwrap();

        let t = "2024-01-01T00:00:00Z";
        mgr.create_event(&run1.id, "text", "Planning", t, None)
            .unwrap();
        mgr.create_event(&run1.id, "tool", "[Read] file.rs", t, None)
            .unwrap();
        mgr.create_event(&run2.id, "result", "$0.0010 · 1 turns · 1.0s", t, None)
            .unwrap();
        // run3 belongs to a different worktree
        mgr.create_event(&run3.id, "text", "Other wt event", t, None)
            .unwrap();

        let w1_events = mgr.list_events_for_worktree("w1").unwrap();
        assert_eq!(w1_events.len(), 3);

        let w2_events = mgr.list_events_for_worktree("w2").unwrap();
        assert_eq!(w2_events.len(), 1);
        assert_eq!(w2_events[0].summary, "Other wt event");
    }

    #[test]
    fn test_prompt_event_appears_first() {
        // Simulates what run_agent does: emit a "prompt" event before spawning claude,
        // then subsequent agent events follow. Verifies the prompt is first in the list.
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let prompt_text = "Fix the login bug";
        let run = mgr.create_run("w1", prompt_text, None, None).unwrap();

        let t0 = "2024-01-01T00:00:00Z";
        let t1 = "2024-01-01T00:00:01Z";
        let t2 = "2024-01-01T00:00:05Z";

        // This mirrors what run_agent now does before spawning claude
        mgr.create_event(&run.id, "prompt", prompt_text, t0, None)
            .unwrap();
        mgr.create_event(&run.id, "system", "Session started", t1, None)
            .unwrap();
        mgr.create_event(&run.id, "tool", "[Bash] cargo test", t2, None)
            .unwrap();

        let events = mgr.list_events_for_run(&run.id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "prompt");
        assert_eq!(events[0].summary, prompt_text);
        assert_eq!(events[1].kind, "system");
        assert_eq!(events[2].kind, "tool");

        // Also verify via list_events_for_worktree
        let wt_events = mgr.list_events_for_worktree("w1").unwrap();
        assert_eq!(wt_events[0].kind, "prompt");
        assert_eq!(wt_events[0].run_id, run.id);
    }

    #[test]
    fn test_events_cascade_delete_on_run_removal() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let t = "2024-01-01T00:00:00Z";
        mgr.create_event(&run.id, "text", "hello", t, None).unwrap();
        mgr.create_event(&run.id, "tool", "[Bash] ls", t, None)
            .unwrap();

        // Deleting the run should cascade to events
        conn.execute(
            "DELETE FROM agent_runs WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();

        let events = mgr.list_events_for_run(&run.id).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_events_from_line_system_init() {
        let line = r#"{"type":"system","subtype":"init","model":"claude-opus-4-5"}"#;
        let events = parse_events_from_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "system");
        assert!(events[0].summary.contains("claude-opus-4-5"));
    }

    #[test]
    fn test_parse_events_from_line_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"description":"run tests"}}]}}"#;
        let events = parse_events_from_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tool");
        assert!(events[0].summary.contains("Bash"));
        assert!(events[0].summary.contains("run tests"));
    }

    #[test]
    fn test_parse_events_from_line_unknown_type() {
        let line = r#"{"type":"rate_limit_event"}"#;
        let events = parse_events_from_line(line);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_agent_log_uses_from_line() {
        // parse_agent_log should produce same results as iterating parse_events_from_line
        let line1 = r#"{"type":"system","subtype":"init","model":"claude-3"}"#;
        let line2 =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#;
        let content = format!("{line1}\n{line2}\n");

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let events = parse_agent_log(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "system");
        assert_eq!(events[1].kind, "text");
        assert_eq!(events[1].summary, "Hello");
    }

    #[test]
    fn test_create_run_with_model() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run("w1", "Fix the bug", None, Some("claude-sonnet-4-6"))
            .unwrap();
        assert_eq!(run.model.as_deref(), Some("claude-sonnet-4-6"));

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn test_create_run_without_model() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        assert!(run.model.is_none());

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.model.is_none());
    }

    // ── Parent/child run tests ────────────────────────────────────────

    #[test]
    fn test_create_child_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run("w1", "Supervisor task", None, None).unwrap();
        assert!(parent.parent_run_id.is_none());

        let child = mgr
            .create_child_run("w1", "Sub-task A", None, None, &parent.id)
            .unwrap();
        assert_eq!(child.parent_run_id.as_deref(), Some(parent.id.as_str()));

        let fetched = mgr.get_run(&child.id).unwrap().unwrap();
        assert_eq!(fetched.parent_run_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn test_list_child_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run("w1", "Supervisor", None, None).unwrap();
        let _child1 = mgr
            .create_child_run("w1", "Child 1", None, None, &parent.id)
            .unwrap();
        let _child2 = mgr
            .create_child_run("w1", "Child 2", None, None, &parent.id)
            .unwrap();

        // Unrelated run should not appear
        let _other = mgr.create_run("w1", "Independent", None, None).unwrap();

        let children = mgr.list_child_runs(&parent.id).unwrap();
        assert_eq!(children.len(), 2);
        assert!(children
            .iter()
            .all(|c| c.parent_run_id.as_deref() == Some(parent.id.as_str())));
    }

    #[test]
    fn test_get_run_tree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Build a tree: parent -> child1, child2 -> grandchild
        let parent = mgr.create_run("w1", "Root task", None, None).unwrap();
        let child1 = mgr
            .create_child_run("w1", "Child 1", None, None, &parent.id)
            .unwrap();
        let _child2 = mgr
            .create_child_run("w2", "Child 2", None, None, &parent.id)
            .unwrap();
        let _grandchild = mgr
            .create_child_run("w1", "Grandchild", None, None, &child1.id)
            .unwrap();

        // Unrelated run
        let _other = mgr.create_run("w1", "Other", None, None).unwrap();

        let tree = mgr.get_run_tree(&parent.id).unwrap();
        assert_eq!(tree.len(), 4); // parent + 2 children + 1 grandchild
        assert_eq!(tree[0].id, parent.id); // root is first (earliest started_at)
    }

    #[test]
    fn test_list_root_runs_for_worktree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run("w1", "Supervisor", None, None).unwrap();
        let _child = mgr
            .create_child_run("w1", "Child", None, None, &parent.id)
            .unwrap();
        let standalone = mgr.create_run("w1", "Standalone", None, None).unwrap();

        let root_runs = mgr.list_root_runs_for_worktree("w1").unwrap();
        assert_eq!(root_runs.len(), 2);
        // Newest first
        assert_eq!(root_runs[0].id, standalone.id);
        assert_eq!(root_runs[1].id, parent.id);
        assert!(root_runs.iter().all(|r| r.parent_run_id.is_none()));
    }

    #[test]
    fn test_aggregate_run_tree() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run("w1", "Supervisor", None, None).unwrap();
        mgr.update_run_completed(&parent.id, None, None, Some(0.10), Some(5), Some(30000))
            .unwrap();

        let child1 = mgr
            .create_child_run("w1", "Child 1", None, None, &parent.id)
            .unwrap();
        mgr.update_run_completed(&child1.id, None, None, Some(0.05), Some(3), Some(15000))
            .unwrap();

        let child2 = mgr
            .create_child_run("w2", "Child 2", None, None, &parent.id)
            .unwrap();
        mgr.update_run_completed(&child2.id, None, None, Some(0.08), Some(4), Some(20000))
            .unwrap();

        // Still-running child should NOT be included in totals
        let _running = mgr
            .create_child_run("w1", "Still running", None, None, &parent.id)
            .unwrap();

        let totals = mgr.aggregate_run_tree(&parent.id).unwrap();
        assert_eq!(totals.total_runs, 3);
        assert!((totals.total_cost - 0.23).abs() < 0.001);
        assert_eq!(totals.total_turns, 12);
        assert_eq!(totals.total_duration_ms, 65000);
    }

    #[test]
    fn test_parent_run_id_set_null_on_delete() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let parent = mgr.create_run("w1", "Parent", None, None).unwrap();
        let child = mgr
            .create_child_run("w1", "Child", None, None, &parent.id)
            .unwrap();

        // Delete the parent — ON DELETE SET NULL should clear child's parent_run_id
        conn.execute(
            "DELETE FROM agent_runs WHERE id = ?1",
            rusqlite::params![parent.id],
        )
        .unwrap();

        let fetched = mgr.get_run(&child.id).unwrap().unwrap();
        assert!(fetched.parent_run_id.is_none());
    }

    // --- build_startup_context tests ---

    #[test]
    fn test_startup_context_returns_none_when_no_useful_data() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a current run (no prior runs, no ticket, non-git path)
        let current = mgr.create_run("w1", "Do stuff", None, None).unwrap();

        // worktree_path is /tmp which has no git repo → commits section will be empty
        // but the branch is still known from the DB
        let ctx = build_startup_context(&conn, "w1", &current.id, "/tmp");
        // Should have at least the worktree branch
        assert!(ctx.is_some());
        let text = ctx.unwrap();
        assert!(text.contains("**Worktree:** feat/test"));
        // No ticket, no prior runs
        assert!(!text.contains("**Ticket:**"));
        assert!(!text.contains("**Plan steps"));
        assert!(!text.contains("**Prior run outcome"));
    }

    #[test]
    fn test_startup_context_includes_ticket() {
        let conn = setup_db();

        // Insert a ticket and link it to worktree w1
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'Fix payment bug', 'Description', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w1'", [])
            .unwrap();

        let mgr = AgentManager::new(&conn);
        let current = mgr.create_run("w1", "Fix it", None, None).unwrap();

        let ctx = build_startup_context(&conn, "w1", &current.id, "/tmp").unwrap();
        assert!(ctx.contains("**Ticket:** #42 — Fix payment bug"));
    }

    #[test]
    fn test_startup_context_includes_prior_plan_steps() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a prior completed run with a plan
        let prior = mgr.create_run("w1", "Prior work", None, None).unwrap();
        let steps = vec![
            PlanStep {
                description: "Read the code".to_string(),
                done: true,
                status: "completed".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Write tests".to_string(),
                done: true,
                status: "completed".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Implement feature".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&prior.id, &steps).unwrap();
        mgr.update_run_completed(&prior.id, None, Some("All done"), None, None, None)
            .unwrap();

        // Create current run
        let current = mgr.create_run("w1", "Continue work", None, None).unwrap();

        let ctx = build_startup_context(&conn, "w1", &current.id, "/tmp").unwrap();
        assert!(ctx.contains("**Plan steps (from prior run):**"));
        assert!(ctx.contains("1. ✅ Read the code"));
        assert!(ctx.contains("2. ✅ Write tests"));
        assert!(ctx.contains("3. ⏳ Implement feature"));
    }

    #[test]
    fn test_startup_context_includes_prior_run_summary() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a prior completed run with a result
        let prior = mgr.create_run("w1", "Prior task", None, None).unwrap();
        mgr.update_run_completed(
            &prior.id,
            None,
            Some("Successfully implemented the payment module"),
            Some(0.15),
            Some(10),
            Some(60000),
        )
        .unwrap();

        // Create current run
        let current = mgr.create_run("w1", "Next task", None, None).unwrap();

        let ctx = build_startup_context(&conn, "w1", &current.id, "/tmp").unwrap();
        assert!(ctx.contains("**Prior run outcome (completed):**"));
        assert!(ctx.contains("Successfully implemented the payment module"));
    }

    #[test]
    fn test_startup_context_excludes_current_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Only the current run exists (no prior runs)
        let current = mgr.create_run("w1", "My prompt", None, None).unwrap();

        let ctx = build_startup_context(&conn, "w1", &current.id, "/tmp").unwrap();
        // Should NOT include any prior run info
        assert!(!ctx.contains("**Plan steps"));
        assert!(!ctx.contains("**Prior run outcome"));
    }

    #[test]
    fn test_startup_context_truncates_long_result() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a prior run with a very long result
        let prior = mgr.create_run("w1", "Prior task", None, None).unwrap();
        let long_result = "x".repeat(1000);
        mgr.update_run_completed(&prior.id, None, Some(&long_result), None, None, None)
            .unwrap();

        let current = mgr.create_run("w1", "Next", None, None).unwrap();

        let ctx = build_startup_context(&conn, "w1", &current.id, "/tmp").unwrap();
        assert!(ctx.contains("**Prior run outcome (completed):**"));
        // Should be truncated to 500 chars + ellipsis
        assert!(ctx.contains(&"x".repeat(500)));
        assert!(ctx.contains('…'));
        assert!(!ctx.contains(&"x".repeat(501)));
    }

    // ── Auto-resume tests ────────────────────────────────────────────

    #[test]
    fn test_needs_resume_failed_with_incomplete_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![
            PlanStep {
                description: "Investigate".to_string(),
                done: true,
                ..Default::default()
            },
            PlanStep {
                description: "Write fix".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Write tests".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_session_id(&run.id, "sess-abc").unwrap();
        mgr.update_run_failed(&run.id, "Context exhausted").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.needs_resume());
        assert!(fetched.has_incomplete_plan_steps());
        assert_eq!(fetched.incomplete_plan_steps().len(), 2);
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn test_needs_resume_cancelled_with_incomplete_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![
            PlanStep {
                description: "Step 1".to_string(),
                done: true,
                ..Default::default()
            },
            PlanStep {
                description: "Step 2".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_session_id(&run.id, "sess-xyz").unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.needs_resume());
        assert_eq!(fetched.incomplete_plan_steps().len(), 1);
    }

    #[test]
    fn test_no_needs_resume_completed_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![PlanStep {
            description: "Step 1".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_completed(&run.id, Some("sess-123"), None, None, None, None)
            .unwrap();
        mgr.mark_plan_done(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(!fetched.needs_resume());
    }

    #[test]
    fn test_no_needs_resume_no_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![PlanStep {
            description: "Step 1".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        // Fail without ever getting a session_id (e.g. spawn failure)
        mgr.update_run_failed(&run.id, "Failed to spawn").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(!fetched.needs_resume()); // No session_id means can't resume
    }

    #[test]
    fn test_no_needs_resume_all_steps_done() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        let steps = vec![PlanStep {
            description: "Step 1".to_string(),
            done: true,
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_session_id(&run.id, "sess-123").unwrap();
        mgr.update_run_failed(&run.id, "Some error").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(!fetched.needs_resume()); // All steps done, nothing to resume
    }

    #[test]
    fn test_build_resume_prompt() {
        let run = AgentRun {
            id: "test".to_string(),
            worktree_id: "w1".to_string(),
            claude_session_id: Some("sess-abc".to_string()),
            prompt: "Fix the bug".to_string(),
            status: "failed".to_string(),
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: Some(vec![
                PlanStep {
                    description: "Investigate".to_string(),
                    done: true,
                    ..Default::default()
                },
                PlanStep {
                    description: "Write fix".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    description: "Write tests".to_string(),
                    ..Default::default()
                },
            ]),
            parent_run_id: None,
        };

        let prompt = run.build_resume_prompt();
        assert!(prompt.contains("Continue where you left off"));
        assert!(prompt.contains("1. Write fix"));
        assert!(prompt.contains("2. Write tests"));
        assert!(!prompt.contains("Investigate")); // Done step should not appear
    }

    #[test]
    fn test_update_run_failed_with_session() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        mgr.update_run_failed_with_session(&run.id, "Context exhausted", Some("sess-456"))
            .unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.status, "failed");
        assert_eq!(fetched.result_text.as_deref(), Some("Context exhausted"));
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-456"));
    }

    #[test]
    fn test_update_run_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        assert!(run.claude_session_id.is_none());

        mgr.update_run_session_id(&run.id, "sess-early").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-early"));
    }

    #[test]
    fn test_failed_with_session_preserves_eager_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run("w1", "Fix the bug", None, None).unwrap();
        // Session ID was saved eagerly during stream
        mgr.update_run_session_id(&run.id, "sess-eager").unwrap();
        // Fail without passing session_id (uses COALESCE to keep existing)
        mgr.update_run_failed(&run.id, "Crashed").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-eager"));
    }
}
