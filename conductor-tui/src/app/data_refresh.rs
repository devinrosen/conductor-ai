use conductor_core::agent::{AgentManager, AgentRun, AgentRunEvent};
use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use super::App;

/// Build fallback `AgentRunEvent`s by parsing log files for runs that lack DB event records.
fn build_fallback_events(runs: &[AgentRun]) -> Vec<AgentRunEvent> {
    use conductor_core::agent::parse_agent_log;

    let mut fallback = Vec::new();
    for run in runs {
        if let Some(ref path) = run.log_file {
            let events = parse_agent_log(path);
            for ev in events {
                fallback.push(AgentRunEvent {
                    id: conductor_core::new_id(),
                    run_id: run.id.clone(),
                    kind: ev.kind,
                    summary: ev.summary,
                    started_at: run.started_at.clone(),
                    ended_at: None,
                    metadata: None,
                });
            }
        }
    }
    fallback
}

impl App {
    pub(super) fn refresh_data(&mut self) {
        let repo_mgr = RepoManager::new(&self.conn, &self.config);
        let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
        let ticket_syncer = TicketSyncer::new(&self.conn);
        let agent_mgr = AgentManager::new(&self.conn);

        self.state.data.repos = repo_mgr.list().unwrap_or_default();
        self.state.data.worktrees = wt_mgr.list(None, true).unwrap_or_default();
        self.state.data.tickets = ticket_syncer.list(None).unwrap_or_default();

        self.state.data.latest_agent_runs = agent_mgr.latest_runs_by_worktree().unwrap_or_default();

        self.refresh_pending_feedback();

        self.state.data.rebuild_maps();
        self.reload_agent_events();

        // If in repo detail, refresh scoped data before rebuilding filtered vecs
        if let Some(ref repo_id) = self.state.selected_repo_id.clone() {
            self.state.rebuild_detail_worktree_tree(repo_id);
            self.state.detail_tickets = self
                .state
                .data
                .tickets
                .iter()
                .filter(|t| &t.repo_id == repo_id)
                .cloned()
                .collect();
            self.rebuild_detail_gates();
        }

        self.state.rebuild_filtered_tickets();
        self.clamp_indices();
    }

    pub(super) fn rebuild_detail_gates(&mut self) {
        use conductor_core::workflow::WorkflowManager;
        if let Some(ref repo_id) = self.state.selected_repo_id.clone() {
            let wf_mgr = WorkflowManager::new(&self.conn);
            self.state.detail_gates = wf_mgr
                .list_waiting_gate_steps_for_repo(repo_id)
                .unwrap_or_else(|e| {
                    tracing::warn!("failed to load pending gates for repo {repo_id}: {e}");
                    Vec::new()
                });
        } else {
            self.state.detail_gates = Vec::new();
        }
        self.state.detail_gate_index = 0;
    }

    pub(super) fn reload_agent_events(&mut self) {
        use crate::state::AgentTotals;

        let Some(ref wt_id) = self.state.selected_worktree_id else {
            self.state.data.agent_events = Vec::new();
            self.state.data.agent_run_info = std::collections::HashMap::new();
            self.state.data.agent_totals = AgentTotals::default();
            self.state.data.child_runs = Vec::new();
            self.state.data.agent_created_issues = Vec::new();
            return;
        };

        let mgr = AgentManager::new(&self.conn);
        // list_for_worktree returns DESC order; reverse for chronological
        let mut runs = mgr.list_for_worktree(wt_id).unwrap_or_default();
        runs.reverse();

        // Compute aggregate stats
        let mut totals = AgentTotals {
            run_count: runs.len(),
            ..Default::default()
        };
        for run in &runs {
            totals.total_cost += run.cost_usd.unwrap_or(0.0);
            totals.total_turns += run.num_turns.unwrap_or(0);
            totals.total_duration_ms += run.duration_ms.unwrap_or(0);
            totals.total_input_tokens += run.input_tokens.unwrap_or(0);
            totals.total_output_tokens += run.output_tokens.unwrap_or(0);
        }

        // For running agents, use the live turn count from the background poller
        if let Some(run) = runs.last() {
            if run.status == conductor_core::agent::AgentRunStatus::Running {
                if let Some(turns) = self.state.data.live_turns_by_worktree.get(wt_id) {
                    totals.live_turns = *turns;
                }
            }
        }

        self.state.data.agent_totals = totals;

        // Load events: prefer DB records, fall back to log file parsing for older runs
        let db_events = mgr.list_events_for_worktree(wt_id).unwrap_or_default();
        let all_events = if !db_events.is_empty() {
            db_events
        } else {
            build_fallback_events(&runs)
        };

        // Build run_id -> (run_number, model, started_at) map for boundary headers
        let mut run_info = std::collections::HashMap::new();
        for (i, run) in runs.iter().enumerate() {
            run_info.insert(
                run.id.clone(),
                (i + 1, run.model.clone(), run.started_at.clone()),
            );
        }
        self.state.data.agent_run_info = run_info;

        self.state.data.agent_events = all_events;

        // Load child runs for the latest root run (for run tree display)
        if let Some(latest) = runs.last() {
            if latest.parent_run_id.is_none() {
                self.state.data.child_runs = mgr.list_child_runs(&latest.id).unwrap_or_default();
            } else {
                self.state.data.child_runs = Vec::new();
            }
        } else {
            self.state.data.child_runs = Vec::new();
        }

        // Clamp ListState selection to valid range after events reload.
        // ratatui also clamps during render, but we keep it tidy here.
        // Use agent_activity_len() (which includes run-separator rows) so the
        // cursor isn't clamped below the last visual row when multiple runs exist.
        let len = self.state.data.agent_activity_len();
        let cur = self.state.agent_list_state.borrow().selected();
        if let Some(idx) = cur {
            if len == 0 {
                self.state.agent_list_state.borrow_mut().select(None);
            } else if idx >= len {
                self.state
                    .agent_list_state
                    .borrow_mut()
                    .select(Some(len - 1));
            }
        }

        // Load issues created by agents for this worktree
        self.state.data.agent_created_issues = mgr
            .list_created_issues_for_worktree(wt_id)
            .unwrap_or_default();
    }

    /// Reload repo-scoped agent events for the currently selected repo.
    pub(super) fn reload_repo_agent_events(&mut self) {

        let Some(ref repo_id) = self.state.selected_repo_id else {
            self.state.data.repo_agent_events = Vec::new();
            self.state.data.repo_agent_run_info = std::collections::HashMap::new();
            return;
        };

        let mgr = AgentManager::new(&self.conn);
        // list_repo_scoped returns DESC order; reverse for chronological
        let mut runs = mgr.list_repo_scoped(repo_id).unwrap_or_default();
        runs.reverse();

        // Load events: single query for all repo-scoped runs (avoids N+1)
        let db_events = mgr.list_events_for_repo(repo_id).unwrap_or_default();
        let all_events = if !db_events.is_empty() {
            db_events
        } else {
            build_fallback_events(&runs)
        };

        // Build run_id -> (run_number, model, started_at) map for boundary headers
        let mut run_info = std::collections::HashMap::new();
        for (i, run) in runs.iter().enumerate() {
            run_info.insert(
                run.id.clone(),
                (i + 1, run.model.clone(), run.started_at.clone()),
            );
        }
        self.state.data.repo_agent_run_info = run_info;
        self.state.data.repo_agent_events = all_events;

        // Clamp ListState selection to valid range
        let len = self.state.data.repo_agent_activity_len();
        let cur = self.state.repo_agent_list_state.borrow().selected();
        if let Some(idx) = cur {
            if len == 0 {
                self.state.repo_agent_list_state.borrow_mut().select(None);
            } else if idx >= len {
                self.state
                    .repo_agent_list_state
                    .borrow_mut()
                    .select(Some(len - 1));
            }
        }
    }

    /// Refresh pending feedback for the currently selected repo's repo-scoped agent.
    pub(super) fn refresh_pending_repo_feedback(&mut self) {
        use conductor_core::agent::AgentManager;

        let Some(ref repo_id) = self.state.selected_repo_id else {
            self.state.data.pending_repo_feedback = None;
            return;
        };

        let mgr = AgentManager::new(&self.conn);
        let latest = mgr.latest_repo_scoped(repo_id).unwrap_or_else(|e| {
            tracing::warn!("failed to load latest repo-scoped run for {repo_id}: {e}");
            None
        });
        self.state.data.pending_repo_feedback = latest.and_then(|run| {
            mgr.pending_feedback_for_run(&run.id).unwrap_or_else(|e| {
                tracing::warn!("failed to load pending repo feedback for {}: {e}", run.id);
                None
            })
        });
    }
}
