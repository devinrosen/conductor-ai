use ratatui::widgets::ListState;

use crate::state::{
    info_row, repo_info_row, DashboardRow, FormField, Modal, RepoDetailFocus, View,
    WorkflowDefFocus, WorkflowRunDetailFocus, WorkflowsFocus, WorktreeDetailFocus,
};

use super::helpers::{clamp_increment, max_scroll, wrap_decrement, wrap_increment};
use super::App;

impl App {
    pub(super) fn half_page_size(&self) -> usize {
        let (_, height) = crossterm::terminal::size().unwrap_or((80, 24));
        // Agent activity pane is roughly the bottom half of the terminal.
        // Use terminal height / 3 as a reasonable half-page for that pane.
        (height as usize / 3).max(1)
    }

    pub(super) fn clamp_indices(&mut self) {
        let dashboard_len = self.state.dashboard_rows().len();
        if dashboard_len > 0 && self.state.dashboard_index >= dashboard_len {
            self.state.dashboard_index = dashboard_len - 1;
        }

        let t_len = self.state.filtered_tickets.len();
        if t_len > 0 && self.state.ticket_index >= t_len {
            self.state.ticket_index = t_len - 1;
        }

        let dt_len = self.state.filtered_detail_tickets.len();
        if dt_len > 0 && self.state.detail_ticket_index >= dt_len {
            self.state.detail_ticket_index = dt_len - 1;
        }

        let pr_len = self.state.detail_prs.len();
        if pr_len > 0 && self.state.detail_pr_index >= pr_len {
            self.state.detail_pr_index = pr_len - 1;
        }

        let gate_len = self.state.detail_gates.len();
        if gate_len > 0 && self.state.detail_gate_index >= gate_len {
            self.state.detail_gate_index = gate_len - 1;
        }
    }

    pub(super) fn go_back(&mut self) {
        // If the step tree pane is active, Esc exits the pane rather than the view.
        if self.state.column_focus == crate::state::ColumnFocus::Workflow
            && self.state.workflows_focus == WorkflowsFocus::Defs
            && self.state.workflow_def_focus == WorkflowDefFocus::Steps
        {
            self.state.workflow_def_focus = WorkflowDefFocus::List;
            return;
        }
        match self.state.view {
            View::Dashboard => self.show_confirm_quit(),
            View::RepoDetail => {
                self.state.view = View::Dashboard;
                self.state.selected_repo_id = None;
            }
            View::WorktreeDetail => {
                if self.state.selected_repo_id.is_some() {
                    self.state.view = View::RepoDetail;
                } else {
                    self.state.view = View::Dashboard;
                }
                self.state.selected_worktree_id = None;
            }
            View::WorkflowRunDetail => {
                self.state.view = self.state.previous_view.take().unwrap_or(View::Dashboard);
                if let Some(prev_wt_id) = self.state.previous_selected_worktree_id.take() {
                    self.state.selected_worktree_id = prev_wt_id;
                }
                self.state.selected_workflow_run_id = None;
                self.state.column_focus = crate::state::ColumnFocus::Workflow;
                self.state.workflows_focus = WorkflowsFocus::Runs;
                // Re-poll immediately so the workflow column reflects the restored view's
                // context (repo- or worktree-scoped) instead of showing stale global data
                // that was loaded while in WorkflowRunDetail.
                self.poll_workflow_data_async();
            }
            View::WorkflowDefDetail => {
                self.state.view = self.state.previous_view.take().unwrap_or(View::Dashboard);
                self.state.selected_workflow_def = None;
                self.state.workflow_def_detail_scroll = 0;
                self.state.column_focus = crate::state::ColumnFocus::Workflow;
                self.state.workflows_focus = WorkflowsFocus::Defs;
            }
        }
    }

    pub(super) fn next_panel(&mut self) {
        use crate::state::ColumnFocus;
        match self.state.column_focus {
            ColumnFocus::Workflow => {
                // Exit step tree first if active, then cycle Defs → Gates → Runs → Defs.
                if self.state.workflow_def_focus == WorkflowDefFocus::Steps {
                    self.state.workflow_def_focus = WorkflowDefFocus::List;
                } else {
                    let has_gates = !self.state.detail_gates.is_empty();
                    self.state.workflows_focus =
                        self.state.workflows_focus.next_for_gates(has_gates);
                }
            }
            ColumnFocus::Content => match self.state.view {
                View::Dashboard => {} // single panel — Tab is a no-op
                View::RepoDetail => {
                    self.state.repo_detail_focus = self.state.repo_detail_focus.next();
                }
                View::WorkflowRunDetail => {
                    self.toggle_workflow_run_detail_focus();
                }
                View::WorktreeDetail => {
                    self.state.worktree_detail_focus = self.state.worktree_detail_focus.toggle();
                }
                View::WorkflowDefDetail => {} // single panel — Tab is a no-op
            },
        }
    }

    pub(super) fn prev_panel(&mut self) {
        use crate::state::ColumnFocus;
        match self.state.column_focus {
            ColumnFocus::Workflow => {
                if self.state.workflows_focus == WorkflowsFocus::Defs
                    && self.state.workflow_def_focus == WorkflowDefFocus::Steps
                {
                    self.state.workflow_def_focus = WorkflowDefFocus::List;
                } else {
                    let has_gates = !self.state.detail_gates.is_empty();
                    self.state.workflows_focus =
                        self.state.workflows_focus.prev_for_gates(has_gates);
                }
            }
            ColumnFocus::Content => match self.state.view {
                View::Dashboard => {} // single panel — Tab is a no-op
                View::RepoDetail => {
                    self.state.repo_detail_focus = self.state.repo_detail_focus.prev();
                }
                View::WorkflowRunDetail => {
                    self.toggle_workflow_run_detail_focus();
                }
                View::WorktreeDetail => {
                    self.state.worktree_detail_focus = self.state.worktree_detail_focus.toggle();
                }
                View::WorkflowDefDetail => {} // single panel — Tab is a no-op
            },
        }
    }

    /// Toggle focus between Steps and Agent Activity panes, but only if the
    /// selected step has agent activity to show.
    pub(super) fn toggle_workflow_run_detail_focus(&mut self) {
        if self.state.selected_step_has_agent() {
            self.state.workflow_run_detail_focus = self.state.workflow_run_detail_focus.toggle();
        }
    }

    pub(super) fn workflow_column_move_up(&mut self) {
        match self.state.workflows_focus {
            WorkflowsFocus::Defs => {
                if self.state.workflow_def_focus == WorkflowDefFocus::Steps {
                    self.state.workflow_def_step_index =
                        self.state.workflow_def_step_index.saturating_sub(1);
                } else {
                    self.state.workflow_def_index = self.state.workflow_def_index.saturating_sub(1);
                    self.state.workflow_def_step_index = 0;
                    self.state.workflow_def_expanded_calls.clear();
                }
            }
            WorkflowsFocus::Gates => {
                self.state.detail_gate_index = self.state.detail_gate_index.saturating_sub(1);
            }
            WorkflowsFocus::Runs => {
                self.state.workflow_run_index = self.state.workflow_run_index.saturating_sub(1);
            }
        }
    }

    pub(super) fn workflow_column_move_down(&mut self) {
        match self.state.workflows_focus {
            WorkflowsFocus::Defs => {
                if self.state.workflow_def_focus == WorkflowDefFocus::Steps {
                    let step_count = self
                        .state
                        .data
                        .workflow_defs
                        .get(self.state.workflow_def_index)
                        .map(|d| {
                            crate::ui::workflows::build_def_step_lines(
                                &d.body,
                                0,
                                &self.state.theme,
                                &self.state.data.workflow_defs,
                                &self.state.workflow_def_expanded_calls,
                                "",
                                &std::collections::HashSet::new(),
                            )
                            .len()
                        })
                        .unwrap_or(0);
                    clamp_increment(&mut self.state.workflow_def_step_index, step_count);
                } else {
                    clamp_increment(
                        &mut self.state.workflow_def_index,
                        self.state.data.workflow_defs.len(),
                    );
                    self.state.workflow_def_step_index = 0;
                    self.state.workflow_def_expanded_calls.clear();
                }
            }
            WorkflowsFocus::Gates => {
                clamp_increment(
                    &mut self.state.detail_gate_index,
                    self.state.detail_gates.len(),
                );
            }
            WorkflowsFocus::Runs => {
                let visible_len = self.state.visible_workflow_run_rows().len();
                clamp_increment(&mut self.state.workflow_run_index, visible_len);
            }
        }
    }

    pub(super) fn workflow_column_select(&mut self) {
        match self.state.workflows_focus {
            WorkflowsFocus::Defs => {
                if self.state.workflow_def_focus == WorkflowDefFocus::Steps {
                    // Enter on a step row: toggle expansion if it's a CallWorkflow node.
                    if let Some(def) = self
                        .state
                        .data
                        .workflow_defs
                        .get(self.state.workflow_def_index)
                    {
                        let path = crate::ui::workflows::get_def_step_node_at(
                            &def.body,
                            &self.state.data.workflow_defs,
                            &self.state.workflow_def_expanded_calls,
                            "",
                            &std::collections::HashSet::new(),
                            self.state.workflow_def_step_index,
                            &mut 0,
                        );
                        if let Some(p) = path {
                            if self.state.workflow_def_expanded_calls.contains(&p) {
                                self.state.workflow_def_expanded_calls.remove(&p);
                            } else {
                                self.state.workflow_def_expanded_calls.insert(p);
                            }
                        }
                    }
                    return;
                }
                if let Some(def) = self.selected_workflow_def() {
                    self.state.selected_workflow_def = Some(def);
                    self.state.workflow_def_detail_scroll = 0;
                    self.state.previous_view = Some(self.state.view);
                    self.state.view = View::WorkflowDefDetail;
                }
            }
            WorkflowsFocus::Gates => {
                if let Some(gate) = self.state.detail_gates.get(self.state.detail_gate_index) {
                    let run_id = gate.step.workflow_run_id.clone();
                    if let Some(run) = self
                        .state
                        .data
                        .workflow_runs
                        .iter()
                        .find(|r| r.id == run_id)
                    {
                        let worktree_id = run.worktree_id.clone();
                        let run_id = run.id.clone();
                        self.state.previous_selected_worktree_id =
                            Some(self.state.selected_worktree_id.clone());
                        if self.state.selected_worktree_id.is_none() {
                            self.state.selected_worktree_id = worktree_id;
                        }
                        self.state.selected_workflow_run_id = Some(run_id);
                        self.state.previous_view = Some(self.state.view);
                        self.state.view = View::WorkflowRunDetail;
                        self.state.workflow_step_index = 0;
                        self.state.workflow_run_detail_focus = WorkflowRunDetailFocus::Steps;
                        self.state.step_agent_event_index = 0;
                        self.state.column_focus = crate::state::ColumnFocus::Content;
                        self.reload_workflow_steps();
                    } else {
                        self.state.status_message =
                            Some("Workflow run not found — try refreshing".to_string());
                    }
                }
            }
            WorkflowsFocus::Runs => {
                let visible = self.state.visible_workflow_run_rows();
                if let Some(row) = visible.get(self.state.workflow_run_index) {
                    let Some(target_id) = row.run_id().map(|s| s.to_string()) else {
                        return; // header row — Enter is a no-op
                    };
                    if let Some(run) = self
                        .state
                        .data
                        .workflow_runs
                        .iter()
                        .find(|r| r.id == target_id)
                    {
                        let run_id = run.id.clone();
                        let worktree_id = run.worktree_id.clone();
                        self.state.previous_selected_worktree_id =
                            Some(self.state.selected_worktree_id.clone());
                        if self.state.selected_worktree_id.is_none() {
                            self.state.selected_worktree_id = worktree_id;
                        }
                        self.state.selected_workflow_run_id = Some(run_id);
                        self.state.previous_view = Some(self.state.view);
                        self.state.view = View::WorkflowRunDetail;
                        self.state.workflow_step_index = 0;
                        self.state.workflow_run_detail_focus = WorkflowRunDetailFocus::Steps;
                        self.state.step_agent_event_index = 0;
                        self.state.column_focus = crate::state::ColumnFocus::Content;
                        self.reload_workflow_steps();
                    }
                }
            }
        }
    }

    pub(super) fn move_up(&mut self) {
        match self.state.modal {
            Modal::EventDetail {
                ref mut scroll_offset,
                ..
            } => {
                *scroll_offset = scroll_offset.saturating_sub(1);
                return;
            }
            Modal::ModelPicker {
                ref mut selected,
                ref mut custom_active,
                allow_default,
                ..
            } => {
                *custom_active = false;
                // +1 for custom, +1 if allow_default adds a "Default" row
                let total =
                    conductor_core::models::KNOWN_MODELS.len() + 1 + usize::from(allow_default);
                wrap_decrement(selected, total);
                return;
            }
            Modal::BranchPicker {
                ref items,
                ref mut selected,
                ..
            } => {
                wrap_decrement(selected, items.len());
                return;
            }
            Modal::PostCreatePicker {
                ref items,
                ref mut selected,
                ..
            } => {
                wrap_decrement(selected, items.len());
                return;
            }
            Modal::IssueSourceManager {
                ref sources,
                ref mut selected,
                ..
            } => {
                wrap_decrement(selected, sources.len());
                return;
            }
            Modal::PrWorkflowPicker {
                ref workflow_defs,
                ref mut selected,
                ..
            }
            | Modal::WorkflowPicker {
                ref workflow_defs,
                ref mut selected,
                ..
            } => {
                wrap_decrement(selected, workflow_defs.len());
                return;
            }
            Modal::GithubDiscoverOrgs {
                ref orgs,
                ref mut cursor,
                ..
            } => {
                wrap_decrement(cursor, orgs.len());
                return;
            }
            Modal::GithubDiscover {
                ref repos,
                ref mut cursor,
                ..
            } => {
                wrap_decrement(cursor, repos.len());
                return;
            }
            _ => {}
        }
        // When workflow column has focus, navigate workflow panes.
        if self.state.column_focus == crate::state::ColumnFocus::Workflow {
            self.workflow_column_move_up();
            return;
        }
        match self.state.view {
            View::Dashboard => {
                self.state.dashboard_index = self.state.dashboard_index.saturating_sub(1);
            }
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Info => {
                    self.state.repo_detail_info_row =
                        self.state.repo_detail_info_row.saturating_sub(1);
                }
                RepoDetailFocus::Worktrees => {
                    self.state.detail_wt_index = self.state.detail_wt_index.saturating_sub(1);
                }
                RepoDetailFocus::Tickets => {
                    self.state.detail_ticket_index =
                        self.state.detail_ticket_index.saturating_sub(1);
                }
                RepoDetailFocus::Prs => {
                    self.state.detail_pr_index = self.state.detail_pr_index.saturating_sub(1);
                }
            },
            View::WorkflowRunDetail => match self.state.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Steps => {
                    let old = self.state.workflow_step_index;
                    self.state.workflow_step_index = old.saturating_sub(1);
                    if self.state.workflow_step_index != old {
                        self.state.step_agent_event_index = 0;
                        self.poll_workflow_data_async();
                    }
                }
                WorkflowRunDetailFocus::AgentActivity => {
                    self.state.step_agent_event_index =
                        self.state.step_agent_event_index.saturating_sub(1);
                }
            },
            View::WorktreeDetail
                if self.state.worktree_detail_focus == WorktreeDetailFocus::InfoPanel =>
            {
                self.state.worktree_detail_selected_row =
                    self.state.worktree_detail_selected_row.saturating_sub(1);
            }
            View::WorkflowDefDetail => {
                self.state.workflow_def_detail_scroll =
                    self.state.workflow_def_detail_scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    pub(super) fn move_down(&mut self) {
        match self.state.modal {
            Modal::EventDetail {
                ref mut scroll_offset,
                line_count,
                ..
            } => {
                *scroll_offset = scroll_offset.saturating_add(1).min(max_scroll(line_count));
                return;
            }
            Modal::ModelPicker {
                ref mut selected,
                ref mut custom_active,
                allow_default,
                ..
            } => {
                *custom_active = false;
                // +1 for custom, +1 if allow_default adds a "Default" row
                let total =
                    conductor_core::models::KNOWN_MODELS.len() + 1 + usize::from(allow_default);
                wrap_increment(selected, total);
                return;
            }
            Modal::BranchPicker {
                ref items,
                ref mut selected,
                ..
            } => {
                wrap_increment(selected, items.len());
                return;
            }
            Modal::PostCreatePicker {
                ref items,
                ref mut selected,
                ..
            } => {
                wrap_increment(selected, items.len());
                return;
            }
            Modal::IssueSourceManager {
                ref sources,
                ref mut selected,
                ..
            } => {
                wrap_increment(selected, sources.len());
                return;
            }
            Modal::PrWorkflowPicker {
                ref workflow_defs,
                ref mut selected,
                ..
            }
            | Modal::WorkflowPicker {
                ref workflow_defs,
                ref mut selected,
                ..
            } => {
                wrap_increment(selected, workflow_defs.len());
                return;
            }
            Modal::GithubDiscoverOrgs {
                ref orgs,
                ref mut cursor,
                ..
            } => {
                wrap_increment(cursor, orgs.len());
                return;
            }
            Modal::GithubDiscover {
                ref repos,
                ref mut cursor,
                ..
            } => {
                wrap_increment(cursor, repos.len());
                return;
            }
            _ => {}
        }
        // When workflow column has focus, navigate workflow panes.
        if self.state.column_focus == crate::state::ColumnFocus::Workflow {
            self.workflow_column_move_down();
            return;
        }
        match self.state.view {
            View::Dashboard => {
                let len = self.state.dashboard_rows().len();
                clamp_increment(&mut self.state.dashboard_index, len);
            }
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Info => {
                    clamp_increment(&mut self.state.repo_detail_info_row, repo_info_row::COUNT);
                }
                RepoDetailFocus::Worktrees => {
                    clamp_increment(
                        &mut self.state.detail_wt_index,
                        self.state.detail_worktrees.len(),
                    );
                }
                RepoDetailFocus::Tickets => {
                    clamp_increment(
                        &mut self.state.detail_ticket_index,
                        self.state.filtered_detail_tickets.len(),
                    );
                }
                RepoDetailFocus::Prs => {
                    clamp_increment(&mut self.state.detail_pr_index, self.state.detail_prs.len());
                }
            },
            View::WorkflowRunDetail => match self.state.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Steps => {
                    let old = self.state.workflow_step_index;
                    clamp_increment(
                        &mut self.state.workflow_step_index,
                        self.state.data.workflow_steps.len(),
                    );
                    if self.state.workflow_step_index != old {
                        self.state.step_agent_event_index = 0;
                        self.poll_workflow_data_async();
                    }
                }
                WorkflowRunDetailFocus::AgentActivity => {
                    clamp_increment(
                        &mut self.state.step_agent_event_index,
                        self.state.data.step_agent_events.len(),
                    );
                }
            },
            View::WorktreeDetail
                if self.state.worktree_detail_focus == WorktreeDetailFocus::InfoPanel =>
            {
                clamp_increment(
                    &mut self.state.worktree_detail_selected_row,
                    info_row::COUNT,
                );
            }
            View::WorkflowDefDetail => {
                self.state.workflow_def_detail_scroll =
                    self.state.workflow_def_detail_scroll.saturating_add(1);
            }
            _ => {}
        }
    }

    pub(super) fn select(&mut self) {
        // When workflow column has focus, handle workflow selection.
        if self.state.column_focus == crate::state::ColumnFocus::Workflow {
            self.workflow_column_select();
            return;
        }
        match self.state.view {
            View::Dashboard => {
                let rows = self.state.dashboard_rows();
                match rows.get(self.state.dashboard_index) {
                    Some(&DashboardRow::Repo(repo_idx)) => {
                        if let Some(repo) = self.state.data.repos.get(repo_idx).cloned() {
                            let repo_id = repo.id.clone();
                            let remote_url = repo.remote_url.clone();
                            self.state.selected_repo_id = Some(repo_id.clone());
                            self.state.detail_worktrees = self
                                .state
                                .data
                                .worktrees
                                .iter()
                                .filter(|wt| wt.repo_id == repo_id)
                                .cloned()
                                .collect();
                            self.state.detail_tickets = self
                                .state
                                .data
                                .tickets
                                .iter()
                                .filter(|t| t.repo_id == repo_id)
                                .cloned()
                                .collect();
                            self.state.detail_wt_index = 0;
                            self.state.detail_ticket_index = 0;
                            self.state.detail_prs = Vec::new();
                            self.state.detail_pr_index = 0;
                            self.state.pr_last_fetched_at = None;
                            if let Some(ref tx) = self.bg_tx {
                                crate::background::spawn_pr_fetch_once(
                                    tx.clone(),
                                    remote_url,
                                    repo_id.clone(),
                                );
                            }
                            self.rebuild_detail_gates();
                            self.state.rebuild_filtered_tickets();
                            self.state.repo_detail_focus = RepoDetailFocus::Worktrees;
                            self.state.view = View::RepoDetail;
                        }
                    }
                    Some(DashboardRow::Feature {
                        repo_idx,
                        feature_idx,
                        ..
                    }) => {
                        // Enter on a feature header toggles collapse
                        if let Some(feature) = self.state.feature_at(*repo_idx, *feature_idx) {
                            let fid = feature.id.clone();
                            if !self.state.collapsed_features.remove(&fid) {
                                self.state.collapsed_features.insert(fid);
                            }
                            self.state.invalidate_dashboard_rows();
                        }
                    }
                    Some(&DashboardRow::Worktree { idx: wt_idx, .. }) => {
                        if let Some(wt) = self.state.data.worktrees.get(wt_idx).cloned() {
                            self.state.selected_worktree_id = Some(wt.id.clone());
                            self.state.selected_repo_id = None;
                            self.state.view = View::WorktreeDetail;
                            *self.state.agent_list_state.borrow_mut() = ListState::default();
                            self.reload_agent_events();
                        }
                    }
                    None => {}
                }
            }
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Info => {
                    // Delegate to info open handler
                    self.handle_repo_detail_info_open();
                }
                RepoDetailFocus::Worktrees => {
                    if let Some(wt) = self.state.detail_worktrees.get(self.state.detail_wt_index) {
                        let wt_id = wt.id.clone();
                        self.state.selected_worktree_id = Some(wt_id);
                        self.state.view = View::WorktreeDetail;
                        *self.state.agent_list_state.borrow_mut() = ListState::default();
                        self.reload_agent_events();
                    }
                }
                RepoDetailFocus::Tickets => {
                    if let Some(ticket) = self
                        .state
                        .filtered_detail_tickets
                        .get(self.state.detail_ticket_index)
                    {
                        self.state.modal = Modal::TicketInfo {
                            ticket: Box::new(ticket.clone()),
                        };
                    }
                }
                RepoDetailFocus::Prs => {
                    // No-op: PR selection deferred to a future ticket.
                }
            },
            View::WorkflowRunDetail => {
                // Build modal title+body from cached data, then assign modal after borrows end
                let modal = if let Some(step) = self
                    .state
                    .data
                    .workflow_steps
                    .get(self.state.workflow_step_index)
                {
                    if step.child_run_id.is_some() {
                        // Step has an agent run — show agent activity from cached data
                        let events = &self.state.data.step_agent_events;
                        let run = &self.state.data.step_agent_run;

                        let title = format!(
                            "Step: {} — Agent Activity ({})",
                            step.step_name, step.status
                        );
                        let mut parts: Vec<String> = Vec::new();

                        if let Some(ref r) = run {
                            parts.push(format!(
                                "Agent: {}  Model: {}  Status: {}",
                                r.id,
                                r.model.as_deref().unwrap_or("default"),
                                r.status
                            ));
                            if let Some(cost) = r.cost_usd {
                                parts.push(format!(
                                    "Cost: ${cost:.4}  Turns: {}",
                                    r.num_turns.unwrap_or(0)
                                ));
                            }
                            parts.push(String::new());
                        }

                        if let Some(ref rt) = step.result_text {
                            parts.push("── Result ──".to_string());
                            parts.push(rt.clone());
                            parts.push(String::new());
                        }

                        if events.is_empty() {
                            parts.push("No agent events recorded yet.".to_string());
                        } else {
                            parts.push(format!("── Events ({}) ──", events.len()));
                            for ev in events {
                                let ts = ev.started_at.get(11..19).unwrap_or(&ev.started_at);
                                let dur = ev
                                    .duration_ms()
                                    .map(|ms| format!(" ({:.1}s)", ms as f64 / 1000.0))
                                    .unwrap_or_default();
                                parts.push(format!("{ts}  [{:<10}]{dur}  {}", ev.kind, ev.summary));
                            }
                        }

                        let body = parts.join("\n");
                        let line_count = body.lines().count();
                        Some(Modal::EventDetail {
                            title,
                            body,
                            line_count,
                            scroll_offset: 0,
                            horizontal_offset: 0,
                        })
                    } else {
                        // No agent run — show step metadata modal
                        let title = format!("Step: {} ({})", step.step_name, step.status);
                        let body = super::helpers::format_metadata_entries(&step.metadata_fields());
                        let line_count = body.lines().count();
                        Some(Modal::EventDetail {
                            title,
                            body,
                            line_count,
                            scroll_offset: 0,
                            horizontal_offset: 0,
                        })
                    }
                } else {
                    None
                };
                if let Some(m) = modal {
                    self.state.modal = m;
                }
            }
            View::WorktreeDetail => {}
            View::WorkflowDefDetail => {}
        }
    }
}

// Suppress unused import warnings — FormField is used in the `_` catch-all
// patterns that Rust doesn't track as "used" via match.
#[allow(unused_imports)]
const _: Option<FormField> = None;
