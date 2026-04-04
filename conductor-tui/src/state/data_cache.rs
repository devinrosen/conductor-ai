use std::collections::HashMap;

use conductor_core::agent::{
    AgentCreatedIssue, AgentRun, AgentRunEvent, FeedbackRequest, TicketAgentTotals,
};
use conductor_core::feature::FeatureRow;
use conductor_core::repo::Repo;
use conductor_core::tickets::{Ticket, TicketLabel};
use conductor_core::workflow::{
    InputDecl, LiveEstimate, WorkflowDef, WorkflowRun, WorkflowRunStep, WorkflowStepSummary,
};
use conductor_core::worktree::Worktree;

#[derive(Debug, Clone, Default)]
pub struct DataCache {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
    pub tickets: Vec<Ticket>,
    /// ticket_id -> labels with colors (populated by DB poller)
    pub ticket_labels: HashMap<String, Vec<TicketLabel>>,
    /// repo_id -> slug for display
    pub repo_slug_map: HashMap<String, String>,
    /// ticket_id -> Ticket for lookups
    pub ticket_map: HashMap<String, Ticket>,
    /// repo_id -> worktree count
    pub repo_worktree_count: HashMap<String, usize>,
    /// worktree_id -> latest AgentRun (populated by DB poller)
    pub latest_agent_runs: HashMap<String, AgentRun>,
    /// Persisted agent events for the currently viewed worktree (from DB)
    pub agent_events: Vec<AgentRunEvent>,
    /// run_id -> (run_number, model, started_at) for per-run boundary headers
    pub agent_run_info: HashMap<String, (usize, Option<String>, String)>,
    /// Aggregate stats across all agent runs for the currently viewed worktree
    pub agent_totals: AgentTotals,
    /// Child runs of the latest root run (for run tree display)
    pub child_runs: Vec<AgentRun>,
    /// ticket_id -> aggregated agent stats across all linked worktrees
    pub ticket_agent_totals: HashMap<String, TicketAgentTotals>,
    /// ticket_id -> linked worktrees (most recently created first)
    pub ticket_worktrees: HashMap<String, Vec<Worktree>>,
    /// Issues created by agents for the currently viewed worktree
    pub agent_created_issues: Vec<AgentCreatedIssue>,
    /// Pending feedback request for the currently viewed worktree (if any)
    pub pending_feedback: Option<FeedbackRequest>,
    /// Most recent workflow run per worktree (worktree_id → run), for inline indicators.
    pub latest_workflow_runs_by_worktree: HashMap<String, WorkflowRun>,
    /// Currently-running step summary per workflow_run_id, for inline step indicators.
    pub workflow_step_summaries: HashMap<String, WorkflowStepSummary>,
    /// Active root workflow runs with no associated worktree (repo/ticket-targeted).
    pub active_non_worktree_workflow_runs: Vec<WorkflowRun>,
    /// Workflow definitions for the currently viewed worktree
    pub workflow_defs: Vec<WorkflowDef>,
    /// Pre-computed repo slug per def (parallel to `workflow_defs`).
    /// Populated by the background thread in global mode; empty in worktree-scoped mode.
    pub workflow_def_slugs: Vec<String>,
    /// Workflow runs for the currently viewed worktree (or all worktrees in global mode)
    pub workflow_runs: Vec<WorkflowRun>,
    /// Steps for the currently viewed workflow run
    pub workflow_steps: Vec<WorkflowRunStep>,
    /// Agent events for the currently selected workflow step's child_run_id
    pub step_agent_events: Vec<AgentRunEvent>,
    /// Agent run metadata for the currently selected step's child_run_id
    pub step_agent_run: Option<AgentRun>,
    /// Steps for every leaf run in the current scope (run_id → ordered steps).
    /// Populated by the background poller on every tick.
    pub workflow_run_steps: HashMap<String, Vec<WorkflowRunStep>>,
    /// Declared inputs per workflow run, pre-parsed from definition_snapshot.
    /// Keyed by run_id; populated when workflow_runs is refreshed to avoid
    /// re-parsing the DSL on every render frame.
    pub workflow_run_declared_inputs: HashMap<String, Vec<InputDecl>>,
    /// Live turn counts for running agents, keyed by worktree_id.
    /// Populated by the background poller each tick.
    pub live_turns_by_worktree: HashMap<String, i64>,
    /// Active features per repo (repo_id → active FeatureRows).
    /// Populated by the background poller each tick.
    pub features_by_repo: HashMap<String, Vec<FeatureRow>>,
    /// repo_id -> latest repo-scoped AgentRun (populated by DB poller)
    pub latest_repo_agent_runs: HashMap<String, AgentRun>,
    /// Persisted agent events for the currently viewed repo's repo-scoped agent (from DB)
    pub repo_agent_events: Vec<AgentRunEvent>,
    /// run_id -> (run_number, model, started_at) for repo agent run boundary headers
    pub repo_agent_run_info: HashMap<String, (usize, Option<String>, String)>,
    /// Pending feedback request for the currently viewed repo's repo agent (if any)
    pub pending_repo_feedback: Option<FeedbackRequest>,
    /// All worktree-scoped agent events keyed by worktree_id; populated by background poller.
    pub all_worktree_agent_events: HashMap<String, Vec<AgentRunEvent>>,
    /// All repo-scoped agent events keyed by repo_id; populated by background poller.
    pub all_repo_agent_events: HashMap<String, Vec<AgentRunEvent>>,
    /// Estimated remaining time for active workflow runs, keyed by run_id.
    pub workflow_run_estimates: HashMap<String, LiveEstimate>,
}

/// Aggregated stats across all agent runs for a worktree.
#[derive(Debug, Clone, Default)]
pub struct AgentTotals {
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub run_count: usize,
    /// Live turn count from the currently running agent's log file.
    pub live_turns: i64,
}

/// A row in the agent activity list: either a run-group separator or an event.
pub enum VisualRow<'a> {
    /// Separator row for a run group: (run_number, model, started_at).
    RunSeparator(usize, Option<&'a str>, &'a str),
    /// An actual agent event.
    Event(&'a AgentRunEvent),
}

/// Shared logic for building visual rows from events + run info.
/// Used by both worktree-agent and repo-agent activity lists.
fn build_visual_rows<'a>(
    events: &'a [AgentRunEvent],
    run_info: &'a HashMap<String, (usize, Option<String>, String)>,
) -> Vec<VisualRow<'a>> {
    let sep_count = count_run_separators(events, run_info);
    let has_multiple_runs = run_info.len() > 1;
    let mut rows = Vec::with_capacity(events.len() + sep_count);
    let mut prev_run_id: Option<&str> = None;

    for ev in events {
        if has_multiple_runs && prev_run_id.is_none_or(|p| p != ev.run_id) {
            if let Some((run_num, model, started_at)) = run_info.get(&ev.run_id) {
                rows.push(VisualRow::RunSeparator(
                    *run_num,
                    model.as_deref(),
                    started_at.as_str(),
                ));
            }
        }
        prev_run_id = Some(&ev.run_id);
        rows.push(VisualRow::Event(ev));
    }
    rows
}

fn count_run_separators(
    events: &[AgentRunEvent],
    run_info: &HashMap<String, (usize, Option<String>, String)>,
) -> usize {
    if run_info.len() <= 1 {
        return 0;
    }
    let mut count = 0;
    let mut prev_run_id: Option<&str> = None;
    for ev in events {
        if prev_run_id.is_none_or(|p| p != ev.run_id) && run_info.contains_key(&ev.run_id) {
            count += 1;
        }
        prev_run_id = Some(&ev.run_id);
    }
    count
}

fn event_at_index<'a>(
    events: &'a [AgentRunEvent],
    run_info: &'a HashMap<String, (usize, Option<String>, String)>,
    visual_target: usize,
) -> Option<&'a AgentRunEvent> {
    match build_visual_rows(events, run_info)
        .into_iter()
        .nth(visual_target)?
    {
        VisualRow::Event(ev) => Some(ev),
        VisualRow::RunSeparator(..) => None,
    }
}

impl DataCache {
    /// Iterate the agent activity list as visual rows, interleaving run-group
    /// separators when there are multiple runs.
    pub fn visual_rows(&self) -> Vec<VisualRow<'_>> {
        build_visual_rows(&self.agent_events, &self.agent_run_info)
    }

    /// Total number of items in the agent activity list, including run boundary
    /// separators.
    pub fn agent_activity_len(&self) -> usize {
        self.agent_events.len() + count_run_separators(&self.agent_events, &self.agent_run_info)
    }

    /// Map a visual index (which may include run-separator rows) back to the
    /// underlying `AgentRunEvent`.
    pub fn event_at_visual_index(&self, visual_target: usize) -> Option<&AgentRunEvent> {
        event_at_index(&self.agent_events, &self.agent_run_info, visual_target)
    }

    // --- Repo agent activity helpers (delegates to shared logic) ---

    pub fn repo_agent_visual_rows(&self) -> Vec<VisualRow<'_>> {
        build_visual_rows(&self.repo_agent_events, &self.repo_agent_run_info)
    }

    pub fn repo_agent_activity_len(&self) -> usize {
        self.repo_agent_events.len()
            + count_run_separators(&self.repo_agent_events, &self.repo_agent_run_info)
    }

    pub fn repo_agent_event_at_visual_index(&self, visual_target: usize) -> Option<&AgentRunEvent> {
        event_at_index(
            &self.repo_agent_events,
            &self.repo_agent_run_info,
            visual_target,
        )
    }

    pub fn rebuild_maps(&mut self) {
        self.repo_slug_map.clear();
        for repo in &self.repos {
            self.repo_slug_map
                .insert(repo.id.clone(), repo.slug.clone());
        }

        self.ticket_map.clear();
        for ticket in &self.tickets {
            self.ticket_map.insert(ticket.id.clone(), ticket.clone());
        }

        // Sort worktrees by (repo_slug, wt_slug) so that state.worktree_index
        // indexes into the same order that the dashboard renders them.
        self.worktrees.sort_by(|a, b| {
            let sa = self
                .repo_slug_map
                .get(&a.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            let sb = self
                .repo_slug_map
                .get(&b.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            sa.cmp(sb).then_with(|| a.slug.cmp(&b.slug))
        });

        self.repo_worktree_count.clear();
        self.ticket_worktrees.clear();
        for wt in &self.worktrees {
            *self
                .repo_worktree_count
                .entry(wt.repo_id.clone())
                .or_insert(0) += 1;
            if let Some(ref tid) = wt.ticket_id {
                self.ticket_worktrees
                    .entry(tid.clone())
                    .or_default()
                    .push(wt.clone());
            }
        }
    }
}
