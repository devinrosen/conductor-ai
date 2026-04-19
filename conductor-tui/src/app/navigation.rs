use ratatui::widgets::ListState;

use crate::state::{
    info_row, repo_info_row, workflow_run_info_row, DashboardRow, FormField, Modal,
    RepoDetailFocus, View, WorkflowDefFocus, WorkflowPickerItem, WorkflowRunDetailFocus,
    WorkflowsFocus, WorktreeDetailFocus,
};

use super::helpers::{clamp_increment, max_scroll, wrap_decrement, wrap_increment};
use super::App;

/// Return the nearest selectable index in `items` when navigating in a given
/// direction, wrapping at the boundaries.  Iterates at most `items.len()`
/// steps so the loop always terminates even if every item is a Header.
///
/// `forward = true` increments the index; `forward = false` decrements it.
/// Returns `start` unchanged when the slice is empty or contains no selectable
/// item.
fn next_selectable(items: &[WorkflowPickerItem], start: usize, forward: bool) -> usize {
    let len = items.len();
    if len == 0 {
        return start;
    }
    let mut idx = if forward {
        if start + 1 >= len {
            0
        } else {
            start + 1
        }
    } else {
        start.checked_sub(1).unwrap_or(len - 1)
    };
    for _ in 0..len {
        if items[idx].is_selectable() {
            return idx;
        }
        idx = if forward {
            if idx + 1 >= len {
                0
            } else {
                idx + 1
            }
        } else {
            idx.checked_sub(1).unwrap_or(len - 1)
        };
    }
    // No selectable item found — leave selection unchanged.
    start
}

/// Half the visible content height used for scroll-centering the workflow picker.
///
/// The popup renders up to ~22 content lines (height cap from `modal.rs` minus borders
/// and chrome). Half of that is ~10 lines, which keeps the selected item roughly
/// centred without requiring a runtime terminal-height query during key handling.
/// If the popup height formula in `modal.rs` changes significantly, update this constant.
const WORKFLOW_PICKER_HALF_VISIBLE_LINES: u16 = 10;

/// Count the rendered visual line index of the item at `selected` in the
/// workflow picker, matching the exact layout emitted by `render_workflow_picker`
/// in `conductor-tui/src/ui/modal.rs`:
/// - 3 top-chrome lines (blank + subtitle + blank)
/// - each `Header` item emits 2 lines (blank + label)
/// - every other item emits 1 line
///
/// NOTE: This function intentionally mirrors the rendering geometry from `modal.rs`
/// so that navigation can compute the correct scroll offset without a runtime
/// layout query. Keep it in sync with the `render_workflow_picker` layout there.
fn workflow_picker_visual_line(items: &[WorkflowPickerItem], selected: usize) -> u16 {
    let mut line: u16 = 3; // top chrome
    for (i, item) in items.iter().enumerate() {
        if i == selected {
            return line;
        }
        match item {
            WorkflowPickerItem::Header(_) => line = line.saturating_add(2),
            _ => line = line.saturating_add(1),
        }
    }
    line
}

impl App {
    /// Update `status_message` to show unresolved blockers for the currently selected ticket,
    /// or clear it if the ticket has no unresolved blockers.
    pub(super) fn update_selected_ticket_blocked_message(&mut self) {
        if let Some(ticket) = self
            .state
            .filtered_detail_tickets
            .get(self.state.detail_ticket_index)
        {
            let blockers: Vec<String> = self
                .state
                .data
                .ticket_dependencies
                .get(&ticket.id)
                .map(|d| {
                    d.active_blockers()
                        .map(|b| format!("#{}", b.source_id))
                        .collect()
                })
                .unwrap_or_default();
            if blockers.is_empty() {
                self.state.status_message = None;
                self.state.status_message_at = None;
            } else {
                self.state.status_message = Some(format!("blocked by: {}", blockers.join(", ")));
                self.state.status_message_at = Some(std::time::Instant::now());
            }
        }
    }

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
            View::Settings => {
                self.state.view = self.state.previous_view.take().unwrap_or(View::Dashboard);
            }
            View::RepoDetail => {
                self.state.view = View::Dashboard;
                self.state.selected_repo_id = None;
                self.sync_selection_arcs();
            }
            View::WorktreeDetail => {
                self.state.view = self.state.previous_view.take().unwrap_or(View::RepoDetail);
                self.state.selected_worktree_id = None;
                if self.state.view == View::Dashboard {
                    self.state.selected_repo_id = None;
                }
                self.sync_selection_arcs();
            }
            View::WorkflowRunDetail => {
                // Pop back to parent child workflow if we drilled in; otherwise leave the view.
                if let Some(parent_id) = self.state.workflow_run_nav_stack.pop() {
                    self.switch_to_workflow_run(parent_id);
                } else {
                    self.state.view = self.state.previous_view.take().unwrap_or(View::Dashboard);
                    if let Some(prev_wt_id) = self.state.previous_selected_worktree_id.take() {
                        self.state.selected_worktree_id = prev_wt_id;
                        self.sync_selection_arcs();
                    }
                    self.state.selected_workflow_run_id = None;
                    self.state.column_focus = crate::state::ColumnFocus::Workflow;
                    self.state.workflows_focus = WorkflowsFocus::Runs;
                    // Re-poll immediately so the workflow column reflects the restored view's
                    // context (repo- or worktree-scoped) instead of showing stale global data
                    // that was loaded while in WorkflowRunDetail.
                    self.poll_workflow_data_async();
                }
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
                View::Settings => self.settings_toggle_focus(),
                View::RepoDetail => {
                    self.state.repo_detail_focus = self.state.repo_detail_focus.next();
                }
                View::WorkflowRunDetail => {
                    self.next_workflow_run_detail_focus();
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
                View::Settings => self.settings_toggle_focus(),
                View::RepoDetail => {
                    self.state.repo_detail_focus = self.state.repo_detail_focus.prev();
                }
                View::WorkflowRunDetail => {
                    self.prev_workflow_run_detail_focus();
                }
                View::WorktreeDetail => {
                    self.state.worktree_detail_focus = self.state.worktree_detail_focus.toggle();
                }
                View::WorkflowDefDetail => {} // single panel — Tab is a no-op
            },
        }
    }

    /// Cycle focus forward: Info → Error (if visible) → Steps → AgentActivity → Info.
    /// Error is skipped when the run has no error; AgentActivity is skipped when
    /// the selected step has no agent.
    pub(super) fn next_workflow_run_detail_focus(&mut self) {
        let has_agent = self.state.selected_step_has_agent();
        let has_error = self.state.selected_run_has_error();
        self.state.workflow_run_detail_focus = self
            .state
            .workflow_run_detail_focus
            .next(has_agent, has_error);
        if self.state.workflow_run_detail_focus == WorkflowRunDetailFocus::Error {
            self.state.error_pane_scroll = 0;
        }
    }

    /// Cycle focus backward: Info ← Error (if visible) ← Steps ← AgentActivity ← Info.
    /// Error is skipped when the run has no error; AgentActivity is skipped when
    /// the selected step has no agent.
    pub(super) fn prev_workflow_run_detail_focus(&mut self) {
        let has_agent = self.state.selected_step_has_agent();
        let has_error = self.state.selected_run_has_error();
        self.state.workflow_run_detail_focus = self
            .state
            .workflow_run_detail_focus
            .prev(has_agent, has_error);
        if self.state.workflow_run_detail_focus == WorkflowRunDetailFocus::Error {
            self.state.error_pane_scroll = 0;
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
            WorkflowsFocus::Runs | WorkflowsFocus::Filter => {
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
            WorkflowsFocus::Runs | WorkflowsFocus::Filter => {
                let visible_len = self.state.visible_workflow_run_rows_len();
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
                        self.enter_workflow_run_detail(run_id, worktree_id);
                    } else {
                        self.state.status_message =
                            Some("Workflow run not found — try refreshing".to_string());
                    }
                }
            }
            WorkflowsFocus::Runs | WorkflowsFocus::Filter => {
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
                        self.enter_workflow_run_detail(run_id, worktree_id);
                    }
                }
            }
        }
    }

    fn enter_workflow_run_detail(&mut self, run_id: String, worktree_id: Option<String>) {
        self.state.previous_selected_worktree_id = Some(self.state.selected_worktree_id.clone());
        if self.state.selected_worktree_id.is_none() {
            self.state.selected_worktree_id = worktree_id;
            self.sync_selection_arcs();
        }
        self.state.selected_workflow_run_id = Some(run_id);
        self.state.workflow_run_nav_stack.clear();
        self.state.previous_view = Some(self.state.view);
        self.state.view = View::WorkflowRunDetail;
        self.state.workflow_step_index = 0;
        self.state.workflow_run_detail_focus = WorkflowRunDetailFocus::Steps;
        self.state.step_agent_event_index = 0;
        self.state.error_pane_scroll = 0;
        self.state.column_focus = crate::state::ColumnFocus::Content;
        self.reload_workflow_steps();
    }

    fn switch_to_workflow_run(&mut self, run_id: String) {
        self.state.selected_workflow_run_id = Some(run_id);
        self.state.workflow_step_index = 0;
        self.state.step_agent_event_index = 0;
        self.state.error_pane_scroll = 0;
        self.reload_workflow_steps();
    }

    fn drill_into_child_workflow(&mut self, child_run_id: String) {
        if let Some(current_id) = self.state.selected_workflow_run_id.take() {
            self.state.workflow_run_nav_stack.push(current_id);
        }
        self.switch_to_workflow_run(child_run_id);
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
            }
            | Modal::BaseBranchPicker {
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
            Modal::WorkflowPicker {
                ref items,
                ref mut selected,
                ref mut scroll_offset,
                ..
            } => {
                if !items.is_empty() {
                    *selected = next_selectable(items, *selected, false);
                    let visual = workflow_picker_visual_line(items, *selected);
                    *scroll_offset = visual.saturating_sub(WORKFLOW_PICKER_HALF_VISIBLE_LINES);
                }
                return;
            }
            Modal::TemplatePicker {
                ref items,
                ref mut selected,
                ..
            } => {
                wrap_decrement(selected, items.len());
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
            Modal::GateAction {
                ref options,
                ref mut focused_option,
                ..
            } if !options.is_empty() => {
                wrap_decrement(focused_option, options.len());
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
                    self.update_selected_ticket_blocked_message();
                }
                RepoDetailFocus::Prs => {
                    self.state.detail_pr_index = self.state.detail_pr_index.saturating_sub(1);
                }
                RepoDetailFocus::RepoAgent => {
                    self.state
                        .repo_agent_list_state
                        .borrow_mut()
                        .select_previous();
                }
            },
            View::WorkflowRunDetail => match self.state.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Info => {
                    self.state.workflow_run_info_row =
                        self.state.workflow_run_info_row.saturating_sub(1);
                }
                WorkflowRunDetailFocus::Error => {
                    self.state.error_pane_scroll = self.state.error_pane_scroll.saturating_sub(1);
                }
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
            View::Settings => {
                self.settings_move_up();
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
            }
            | Modal::BaseBranchPicker {
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
            Modal::WorkflowPicker {
                ref items,
                ref mut selected,
                ref mut scroll_offset,
                ..
            } => {
                if !items.is_empty() {
                    *selected = next_selectable(items, *selected, true);
                    let visual = workflow_picker_visual_line(items, *selected);
                    *scroll_offset = visual.saturating_sub(WORKFLOW_PICKER_HALF_VISIBLE_LINES);
                }
                return;
            }
            Modal::TemplatePicker {
                ref items,
                ref mut selected,
                ..
            } => {
                wrap_increment(selected, items.len());
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
            Modal::GateAction {
                ref options,
                ref mut focused_option,
                ..
            } if !options.is_empty() => {
                wrap_increment(focused_option, options.len());
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
                    self.update_selected_ticket_blocked_message();
                }
                RepoDetailFocus::Prs => {
                    clamp_increment(&mut self.state.detail_pr_index, self.state.detail_prs.len());
                }
                RepoDetailFocus::RepoAgent => {
                    let len = self.state.data.repo_agent_activity_len();
                    let cur = self
                        .state
                        .repo_agent_list_state
                        .borrow()
                        .selected()
                        .unwrap_or(0);
                    if len > 0 && cur + 1 < len {
                        self.state.repo_agent_list_state.borrow_mut().select_next();
                    }
                }
            },
            View::WorkflowRunDetail => match self.state.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Info => {
                    clamp_increment(
                        &mut self.state.workflow_run_info_row,
                        workflow_run_info_row::COUNT,
                    );
                }
                WorkflowRunDetailFocus::Error => {
                    self.state.error_pane_scroll = self.state.error_pane_scroll.saturating_add(1);
                }
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
                let wt_branch = self
                    .state
                    .selected_worktree()
                    .map(|wt| wt.branch.clone())
                    .unwrap_or_default();
                let has_pr = self.state.find_pr_for_worktree(&wt_branch).is_some();
                let count = if has_pr {
                    info_row::COUNT
                } else {
                    info_row::COUNT - 1
                };
                clamp_increment(&mut self.state.worktree_detail_selected_row, count);
            }
            View::WorkflowDefDetail => {
                self.state.workflow_def_detail_scroll =
                    self.state.workflow_def_detail_scroll.saturating_add(1);
            }
            View::Settings => {
                self.settings_move_down();
            }
            _ => {}
        }
    }

    fn navigate_to_repo_detail(&mut self, repo_idx: usize) {
        if let Some(repo) = self.state.data.repos.get(repo_idx).cloned() {
            let repo_id = repo.id.clone();
            let remote_url = repo.remote_url.clone();
            self.state.selected_repo_id = Some(repo_id.clone());
            self.sync_selection_arcs();
            self.state.rebuild_detail_worktree_tree(&repo_id);
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
                    remote_url.clone(),
                    repo_id.clone(),
                );
            }
            // Auto-sync tickets (staleness check happens in the background thread).
            if !self.state.ticket_sync_in_progress {
                if let Some(ref tx) = self.bg_tx {
                    self.state.ticket_sync_in_progress = true;
                    crate::background::spawn_ticket_sync_for_repo(
                        tx.clone(),
                        repo_id.clone(),
                        repo.slug.clone(),
                        remote_url,
                        false,
                    );
                }
            }
            self.rebuild_detail_gates();
            self.state.rebuild_filtered_tickets();
            self.state.repo_detail_focus = RepoDetailFocus::Worktrees;
            self.state.view = View::RepoDetail;
            self.reload_repo_agent_events();
            self.refresh_pending_repo_feedback();
            *self.state.repo_agent_list_state.borrow_mut() = ratatui::widgets::ListState::default();
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
                        self.navigate_to_repo_detail(repo_idx);
                    }
                    Some(&DashboardRow::Worktree { idx: wt_idx, .. }) => {
                        if let Some(wt) = self.state.data.worktrees.get(wt_idx).cloned() {
                            self.state.selected_worktree_id = Some(wt.id.clone());
                            self.state.selected_repo_id = Some(wt.repo_id.clone());
                            self.sync_selection_arcs();
                            self.state.previous_view = Some(View::Dashboard);
                            self.state.detail_prs = Vec::new();
                            self.state.pr_last_fetched_at = None;
                            self.state.view = View::WorktreeDetail;
                            *self.state.agent_list_state.borrow_mut() = ListState::default();
                            self.reload_agent_events();
                            if let Some(repo) =
                                self.state.data.repos.iter().find(|r| r.id == wt.repo_id)
                            {
                                let remote_url = repo.remote_url.clone();
                                let repo_id = wt.repo_id.clone();
                                if let Some(ref tx) = self.bg_tx {
                                    crate::background::spawn_pr_fetch_once(
                                        tx.clone(),
                                        remote_url,
                                        repo_id,
                                    );
                                }
                            }
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
                        self.sync_selection_arcs();
                        self.state.previous_view = Some(View::RepoDetail);
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
                RepoDetailFocus::RepoAgent => {
                    // Enter on repo agent event opens detail modal
                    self.handle_expand_repo_agent_event();
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
                    // [workflow] steps: drill into the child workflow run instead of a modal.
                    if step.role == conductor_core::workflow::STEP_ROLE_WORKFLOW {
                        if let Some(ref child_id) = step.child_run_id.clone() {
                            self.drill_into_child_workflow(child_id.clone());
                        }
                        return;
                    }
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
            View::Settings => {}
        }
    }
}

// Suppress unused import warnings — FormField is used in the `_` catch-all
// patterns that Rust doesn't track as "used" via match.
#[allow(unused_imports)]
const _: Option<FormField> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ColumnFocus, View, WorkflowDefFocus, WorkflowPickerTarget, WorkflowsFocus};

    fn make_test_app() -> App {
        let conn = conductor_core::test_helpers::create_test_conn();
        App::new(
            conn,
            conductor_core::config::Config::default(),
            crate::theme::Theme::default(),
        )
    }

    fn make_test_repo(id: &str, slug: &str) -> conductor_core::repo::Repo {
        conductor_core::repo::Repo {
            id: id.into(),
            slug: slug.into(),
            local_path: format!("/tmp/{slug}"),
            remote_url: format!("https://github.com/test/{slug}.git"),
            default_branch: "main".into(),
            workspace_dir: "/tmp".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            model: None,
            allow_agent_issue_creation: false,
        }
    }

    fn make_gate(
        id: &str,
        run_id: &str,
        step_name: &str,
    ) -> conductor_core::workflow::PendingGateRow {
        let base = crate::state::tests::make_wf_step(id, run_id, step_name, 0);
        conductor_core::workflow::PendingGateRow {
            step: conductor_core::workflow::WorkflowRunStep {
                role: "worker".into(),
                status: conductor_core::workflow::WorkflowStepStatus::Waiting,
                ..base
            },
            workflow_name: "test-wf".into(),
            target_label: None,
            branch: None,
            ticket_ref: None,
            workflow_title: None,
        }
    }

    fn make_test_worktree(
        id: &str,
        repo_id: &str,
        slug: &str,
    ) -> conductor_core::worktree::Worktree {
        conductor_core::worktree::Worktree {
            id: id.into(),
            repo_id: repo_id.into(),
            slug: slug.into(),
            branch: format!("feat/{slug}"),
            path: format!("/tmp/ws/{slug}"),
            ticket_id: None,
            status: conductor_core::worktree::WorktreeStatus::Active,
            created_at: "2024-01-01T00:00:00Z".into(),
            completed_at: None,
            model: None,
            base_branch: None,
        }
    }

    // ── clamp_indices ─────────────────────────────────────────────────

    #[test]
    fn clamp_indices_preserves_valid_index() {
        let mut app = make_test_app();
        app.state.data.repos = vec![make_test_repo("r1", "repo-a")];
        app.state.data.worktrees = vec![make_test_worktree("w1", "r1", "feat-a")];
        app.state.dashboard_index = 1; // worktree row (valid)
        app.clamp_indices();
        assert_eq!(app.state.dashboard_index, 1);
    }

    #[test]
    fn clamp_indices_clamps_oversized_dashboard_index() {
        let mut app = make_test_app();
        app.state.data.repos = vec![make_test_repo("r1", "repo-a")];
        // dashboard_rows has 1 entry (just the repo header)
        app.state.dashboard_index = 10;
        app.clamp_indices();
        assert_eq!(app.state.dashboard_index, 0);
    }

    #[test]
    fn clamp_indices_empty_lists_no_clamp() {
        let mut app = make_test_app();
        app.state.dashboard_index = 5;
        app.state.ticket_index = 3;
        app.clamp_indices();
        // Empty lists → clamp block doesn't fire, indices stay as-is
        assert_eq!(app.state.dashboard_index, 5);
        assert_eq!(app.state.ticket_index, 3);
    }

    #[test]
    fn clamp_indices_clamps_ticket_index() {
        let mut app = make_test_app();
        app.state.filtered_tickets = vec![conductor_core::tickets::Ticket {
            id: "t1".into(),
            repo_id: "r1".into(),
            source_type: "github".into(),
            source_id: "1".into(),
            title: "Ticket".into(),
            body: "".into(),
            state: "open".into(),
            labels: "".into(),
            assignee: None,
            priority: None,
            url: "".into(),
            synced_at: "2024-01-01T00:00:00Z".into(),
            raw_json: "{}".into(),
            workflow: None,
            agent_map: None,
        }];
        app.state.ticket_index = 5;
        app.clamp_indices();
        assert_eq!(app.state.ticket_index, 0);
    }

    // ── go_back ───────────────────────────────────────────────────────

    #[test]
    fn go_back_dashboard_shows_confirm_quit() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.go_back();
        assert!(matches!(app.state.modal, Modal::Confirm { .. }));
    }

    #[test]
    fn go_back_repo_detail_to_dashboard() {
        let mut app = make_test_app();
        app.state.view = View::RepoDetail;
        app.state.selected_repo_id = Some("r1".into());
        app.go_back();
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_repo_id.is_none());
    }

    #[test]
    fn go_back_worktree_detail_with_repo_goes_to_repo_detail() {
        let mut app = make_test_app();
        app.state.view = View::WorktreeDetail;
        app.state.selected_repo_id = Some("r1".into());
        app.state.selected_worktree_id = Some("w1".into());
        app.go_back();
        assert_eq!(app.state.view, View::RepoDetail);
        assert!(app.state.selected_worktree_id.is_none());
    }

    #[test]
    fn go_back_worktree_detail_without_repo_goes_to_dashboard() {
        let mut app = make_test_app();
        app.state.view = View::WorktreeDetail;
        app.state.previous_view = Some(View::Dashboard);
        app.state.selected_worktree_id = Some("w1".into());
        app.go_back();
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_worktree_id.is_none());
    }

    #[test]
    fn go_back_worktree_detail_from_dashboard_clears_repo_id() {
        let mut app = make_test_app();
        app.state.view = View::WorktreeDetail;
        app.state.previous_view = Some(View::Dashboard);
        app.state.selected_repo_id = Some("r1".into());
        app.state.selected_worktree_id = Some("w1".into());
        app.go_back();
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_worktree_id.is_none());
        assert!(app.state.selected_repo_id.is_none());
    }

    #[test]
    fn go_back_workflow_run_detail_restores_previous_view() {
        let mut app = make_test_app();
        app.state.view = View::WorkflowRunDetail;
        app.state.previous_view = Some(View::RepoDetail);
        app.state.selected_workflow_run_id = Some("run1".into());
        app.go_back();
        assert_eq!(app.state.view, View::RepoDetail);
        assert_eq!(app.state.column_focus, ColumnFocus::Workflow);
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Runs);
        assert!(app.state.selected_workflow_run_id.is_none());
    }

    #[test]
    fn go_back_workflow_def_detail_restores_defs_focus() {
        let mut app = make_test_app();
        app.state.view = View::WorkflowDefDetail;
        app.state.previous_view = Some(View::Dashboard);
        app.state.selected_workflow_def = Some(conductor_core::workflow::WorkflowDef {
            name: "test".into(),
            title: None,
            description: String::new(),
            trigger: conductor_core::workflow::WorkflowTrigger::Manual,
            targets: vec![],
            group: None,
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: String::new(),
        });
        app.go_back();
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_workflow_def.is_none());
        assert_eq!(app.state.column_focus, ColumnFocus::Workflow);
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Defs);
    }

    #[test]
    fn go_back_from_step_tree_exits_pane_not_view() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Defs;
        app.state.workflow_def_focus = WorkflowDefFocus::Steps;
        app.state.view = View::Dashboard;
        app.go_back();
        assert_eq!(app.state.workflow_def_focus, WorkflowDefFocus::List);
        assert_eq!(app.state.view, View::Dashboard);
    }

    // ── next_panel / prev_panel ───────────────────────────────────────

    #[test]
    fn next_panel_dashboard_is_noop() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.next_panel();
        // Should remain Dashboard — no panel cycling on dashboard
        assert_eq!(app.state.view, View::Dashboard);
    }

    #[test]
    fn next_panel_repo_detail_cycles_focus() {
        let mut app = make_test_app();
        app.state.view = View::RepoDetail;
        app.state.column_focus = ColumnFocus::Content;
        app.state.repo_detail_focus = RepoDetailFocus::Info;
        app.next_panel();
        assert_eq!(app.state.repo_detail_focus, RepoDetailFocus::Worktrees);
    }

    #[test]
    fn prev_panel_repo_detail_cycles_backward() {
        let mut app = make_test_app();
        app.state.view = View::RepoDetail;
        app.state.column_focus = ColumnFocus::Content;
        app.state.repo_detail_focus = RepoDetailFocus::Worktrees;
        app.prev_panel();
        assert_eq!(app.state.repo_detail_focus, RepoDetailFocus::Info);
    }

    #[test]
    fn next_panel_worktree_detail_toggles() {
        let mut app = make_test_app();
        app.state.view = View::WorktreeDetail;
        app.state.column_focus = ColumnFocus::Content;
        app.state.worktree_detail_focus = WorktreeDetailFocus::InfoPanel;
        app.next_panel();
        assert_eq!(
            app.state.worktree_detail_focus,
            WorktreeDetailFocus::LogPanel
        );
        app.next_panel();
        assert_eq!(
            app.state.worktree_detail_focus,
            WorktreeDetailFocus::InfoPanel
        );
    }

    #[test]
    fn next_panel_workflow_column_cycles_defs_to_runs_no_gates() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Defs;
        // No gates → should skip Gates and go to Runs
        app.next_panel();
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Runs);
    }

    #[test]
    fn next_panel_workflow_column_cycles_with_gates() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Defs;
        // Add a gate so Gates pane is visible
        app.state.detail_gates = vec![make_gate("s1", "run1", "gate1")];
        app.next_panel();
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Gates);
        app.next_panel();
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Runs);
    }

    #[test]
    fn prev_panel_workflow_column_cycles_backward() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Defs;
        // No gates
        app.prev_panel();
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Runs);
    }

    #[test]
    fn next_panel_step_tree_exits_to_list() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Defs;
        app.state.workflow_def_focus = WorkflowDefFocus::Steps;
        app.next_panel();
        assert_eq!(app.state.workflow_def_focus, WorkflowDefFocus::List);
    }

    // ── move_up / move_down ───────────────────────────────────────────

    #[test]
    fn move_up_dashboard_saturates_at_zero() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.state.dashboard_index = 0;
        app.move_up();
        assert_eq!(app.state.dashboard_index, 0);
    }

    #[test]
    fn move_down_dashboard_increments() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.state.data.repos = vec![make_test_repo("r1", "repo-a")];
        app.state.data.worktrees = vec![make_test_worktree("w1", "r1", "feat-a")];
        app.state.dashboard_index = 0;
        app.move_down();
        assert_eq!(app.state.dashboard_index, 1);
    }

    #[test]
    fn move_down_dashboard_clamps_at_end() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.state.data.repos = vec![make_test_repo("r1", "repo-a")];
        // Only 1 row (repo header) → can't go past 0
        app.state.dashboard_index = 0;
        app.move_down();
        assert_eq!(app.state.dashboard_index, 0);
    }

    #[test]
    fn move_up_dashboard_decrements() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.state.data.repos = vec![make_test_repo("r1", "repo-a")];
        app.state.data.worktrees = vec![make_test_worktree("w1", "r1", "feat-a")];
        app.state.dashboard_index = 1;
        app.move_up();
        assert_eq!(app.state.dashboard_index, 0);
    }

    #[test]
    fn move_up_event_detail_modal_decrements_scroll() {
        let mut app = make_test_app();
        app.state.modal = Modal::EventDetail {
            title: "Test".into(),
            body: "line1\nline2\nline3".into(),
            line_count: 3,
            scroll_offset: 2,
            horizontal_offset: 0,
        };
        app.move_up();
        if let Modal::EventDetail { scroll_offset, .. } = app.state.modal {
            assert_eq!(scroll_offset, 1);
        } else {
            panic!("expected EventDetail modal");
        }
    }

    #[test]
    fn move_down_event_detail_modal_increments_scroll() {
        let mut app = make_test_app();
        app.state.modal = Modal::EventDetail {
            title: "Test".into(),
            body: "line1\nline2\nline3".into(),
            line_count: 3,
            scroll_offset: 0,
            horizontal_offset: 0,
        };
        app.move_down();
        if let Modal::EventDetail { scroll_offset, .. } = app.state.modal {
            assert_eq!(scroll_offset, 1);
        } else {
            panic!("expected EventDetail modal");
        }
    }

    #[test]
    fn move_up_in_workflow_column_moves_workflow_index() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        app.state.workflow_run_index = 1;
        app.move_up();
        assert_eq!(app.state.workflow_run_index, 0);
    }

    #[test]
    fn move_down_in_workflow_column_increments_gate_index() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Gates;
        app.state.detail_gate_index = 0;
        app.state.detail_gates = vec![
            make_gate("s1", "run1", "gate1"),
            make_gate("s2", "run1", "gate2"),
        ];
        app.move_down();
        assert_eq!(app.state.detail_gate_index, 1);
    }

    // ── select ────────────────────────────────────────────────────────

    #[test]
    fn select_dashboard_repo_navigates_to_repo_detail() {
        let mut app = make_test_app();
        let repo = make_test_repo("r1", "repo-a");
        // Also register in DB so navigate_to_repo_detail doesn't fail
        app.conn
            .execute(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r1', 'repo-a', '/tmp/repo-a', 'https://github.com/test/repo-a.git', '/tmp', '2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        app.state.data.repos = vec![repo];
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.state.dashboard_index = 0; // repo row
        app.select();
        assert_eq!(app.state.view, View::RepoDetail);
        assert_eq!(app.state.selected_repo_id.as_deref(), Some("r1"));
    }

    #[test]
    fn select_dashboard_worktree_navigates_to_worktree_detail() {
        let mut app = make_test_app();
        let repo = make_test_repo("r1", "repo-a");
        let wt = make_test_worktree("w1", "r1", "feat-a");
        app.state.data.repos = vec![repo];
        app.state.data.worktrees = vec![wt];
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.state.dashboard_index = 1; // worktree row
                                       // Pre-populate stale PR data to verify it is cleared on navigation
        app.state.detail_prs = vec![conductor_core::github::GithubPr {
            number: 99,
            title: "stale".into(),
            url: "https://github.com/x/y/pull/99".into(),
            author: "user".into(),
            head_ref_name: "old-branch".into(),
            state: "open".into(),
            is_draft: false,
            review_decision: None,
            ci_status: "pending".into(),
        }];
        app.state.pr_last_fetched_at = Some(std::time::Instant::now());
        app.select();
        assert_eq!(app.state.view, View::WorktreeDetail);
        assert_eq!(app.state.selected_worktree_id.as_deref(), Some("w1"));
        assert_eq!(app.state.selected_repo_id.as_deref(), Some("r1"));
        assert_eq!(app.state.previous_view, Some(View::Dashboard));
        assert!(app.state.detail_prs.is_empty());
        assert!(app.state.pr_last_fetched_at.is_none());
    }

    #[test]
    fn select_on_empty_dashboard_is_noop() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = ColumnFocus::Content;
        app.select();
        assert_eq!(app.state.view, View::Dashboard);
    }

    #[test]
    fn select_workflow_column_delegates() {
        let mut app = make_test_app();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        // No runs → select is a no-op
        app.select();
        assert_eq!(app.state.view, View::Dashboard);
    }

    // ── workflow_picker_visual_line ────────────────────────────────────

    fn make_wf_def(name: &str) -> conductor_core::workflow::WorkflowDef {
        conductor_core::workflow::WorkflowDef {
            name: name.into(),
            title: None,
            description: String::new(),
            trigger: conductor_core::workflow::WorkflowTrigger::Manual,
            targets: vec![],
            group: None,
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: String::new(),
        }
    }

    #[test]
    fn workflow_picker_visual_line_first_workflow_item() {
        // 3 top-chrome lines, no headers before index 0 → visual line = 3
        let items = vec![WorkflowPickerItem::Workflow(make_wf_def("wf-a"))];
        assert_eq!(workflow_picker_visual_line(&items, 0), 3);
    }

    #[test]
    fn workflow_picker_visual_line_after_header() {
        // Header at index 0 emits 2 lines; item at index 1 is at visual line 3+2=5
        let items = vec![
            WorkflowPickerItem::Header("Group".into()),
            WorkflowPickerItem::Workflow(make_wf_def("wf-a")),
        ];
        assert_eq!(workflow_picker_visual_line(&items, 1), 5);
    }

    // ── WorkflowPicker scroll_offset on move_up / move_down ───────────

    fn make_workflow_picker_modal(
        items: Vec<WorkflowPickerItem>,
        selected: usize,
        scroll_offset: u16,
    ) -> Modal {
        Modal::WorkflowPicker {
            target: WorkflowPickerTarget::Repo {
                repo_id: "r1".into(),
                repo_path: "/tmp/r1".into(),
                repo_name: "repo-a".into(),
            },
            items,
            selected,
            scroll_offset,
        }
    }

    #[test]
    fn move_down_workflow_picker_updates_scroll_offset() {
        let mut app = make_test_app();
        // Two selectable items; start at index 0
        let items = vec![
            WorkflowPickerItem::Workflow(make_wf_def("wf-a")),
            WorkflowPickerItem::Workflow(make_wf_def("wf-b")),
        ];
        app.state.modal = make_workflow_picker_modal(items, 0, 0);
        app.move_down();
        if let Modal::WorkflowPicker {
            selected,
            scroll_offset,
            ..
        } = app.state.modal
        {
            assert_eq!(selected, 1, "should advance to index 1");
            // visual line of index 1 = 3 + 1 = 4; saturating_sub(10) = 0
            assert_eq!(scroll_offset, 0);
        } else {
            panic!("expected WorkflowPicker modal");
        }
    }

    #[test]
    fn move_up_workflow_picker_updates_scroll_offset() {
        let mut app = make_test_app();
        // Two selectable items; start at index 1, move up to index 0
        let items = vec![
            WorkflowPickerItem::Workflow(make_wf_def("wf-a")),
            WorkflowPickerItem::Workflow(make_wf_def("wf-b")),
        ];
        app.state.modal = make_workflow_picker_modal(items, 1, 0);
        app.move_up();
        if let Modal::WorkflowPicker {
            selected,
            scroll_offset,
            ..
        } = app.state.modal
        {
            assert_eq!(selected, 0, "should retreat to index 0");
            // visual line of index 0 = 3; saturating_sub(10) = 0
            assert_eq!(scroll_offset, 0);
        } else {
            panic!("expected WorkflowPicker modal");
        }
    }

    #[test]
    fn move_down_workflow_picker_scroll_offset_nonzero_past_half_visible() {
        let mut app = make_test_app();
        // Build enough items that visual line > WORKFLOW_PICKER_HALF_VISIBLE_LINES.
        // 3 top-chrome + 11 prior workflow items → item at index 10 is at visual line 13.
        // 13.saturating_sub(10) = 3
        let items: Vec<WorkflowPickerItem> = (0..12)
            .map(|i| WorkflowPickerItem::Workflow(make_wf_def(&format!("wf-{i}"))))
            .collect();
        // Start at index 9, move down to index 10
        app.state.modal = make_workflow_picker_modal(items, 9, 0);
        app.move_down();
        if let Modal::WorkflowPicker {
            selected,
            scroll_offset,
            ..
        } = app.state.modal
        {
            assert_eq!(selected, 10);
            // visual line = 3 + 10 = 13; 13 - 10 = 3
            assert_eq!(scroll_offset, 3);
        } else {
            panic!("expected WorkflowPicker modal");
        }
    }

    // ── WorkflowRunDetail navigation: sync_selection_arcs invariant ───

    fn make_test_run_with_worktree(
        id: &str,
        worktree_id: &str,
    ) -> conductor_core::workflow::WorkflowRun {
        let mut run = crate::state::tests::make_wf_run_full(
            id,
            conductor_core::workflow::WorkflowRunStatus::Running,
            None,
        );
        run.worktree_id = Some(worktree_id.into());
        run.parent_run_id = String::new();
        run
    }

    fn make_test_run_without_worktree(id: &str) -> conductor_core::workflow::WorkflowRun {
        let mut run = crate::state::tests::make_wf_run_full(
            id,
            conductor_core::workflow::WorkflowRunStatus::Running,
            None,
        );
        run.worktree_id = None;
        run.parent_run_id = String::new();
        run
    }

    #[test]
    fn runs_navigation_syncs_arc_when_worktree_unset() {
        let mut app = make_test_app();
        app.state.selected_worktree_id = None;
        // Set a repo so visible_workflow_run_rows uses non-global mode
        // (no header rows), placing the run directly at index 0.
        app.state.selected_repo_id = Some("r1".into());
        app.state.data.workflow_runs = vec![make_test_run_with_worktree("run1", "wt1")];
        app.state.rebuild_workflow_run_rows();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        app.state.workflow_run_index = 0;
        app.workflow_column_select();
        assert_eq!(
            *app.selected_worktree_id_shared.lock().unwrap(),
            Some("wt1".into()),
            "shared Arc must reflect the worktree assigned during Runs navigation"
        );
        assert_eq!(app.state.view, View::WorkflowRunDetail);
    }

    #[test]
    fn gates_navigation_syncs_arc_when_worktree_unset() {
        let mut app = make_test_app();
        app.state.selected_worktree_id = None;
        let run = make_test_run_with_worktree("run1", "wt1");
        app.state.data.workflow_runs = vec![run];
        app.state.detail_gates = vec![make_gate("s1", "run1", "gate1")];
        app.state.detail_gate_index = 0;
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Gates;
        app.workflow_column_select();
        assert_eq!(
            *app.selected_worktree_id_shared.lock().unwrap(),
            Some("wt1".into()),
            "shared Arc must reflect the worktree assigned during Gates navigation"
        );
        assert_eq!(app.state.view, View::WorkflowRunDetail);
    }

    #[test]
    fn runs_navigation_skips_arc_sync_when_worktree_already_set() {
        let mut app = make_test_app();
        app.state.selected_worktree_id = Some("existing".into());
        *app.selected_worktree_id_shared.lock().unwrap() = Some("existing".into());
        app.state.data.workflow_runs = vec![make_test_run_with_worktree("run1", "wt1")];
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        app.state.workflow_run_index = 0;
        app.workflow_column_select();
        assert_eq!(
            *app.selected_worktree_id_shared.lock().unwrap(),
            Some("existing".into()),
            "Arc must not change when selected_worktree_id was already set"
        );
    }

    #[test]
    fn runs_navigation_syncs_arc_when_run_worktree_id_is_none() {
        let mut app = make_test_app();
        app.state.selected_worktree_id = None;
        app.state.selected_repo_id = Some("r1".into());
        app.state.data.workflow_runs = vec![make_test_run_without_worktree("run1")];
        app.state.rebuild_workflow_run_rows();
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        app.state.workflow_run_index = 0;
        app.workflow_column_select();
        assert_eq!(
            *app.selected_worktree_id_shared.lock().unwrap(),
            None,
            "Arc must be synced to None when run has no worktree_id"
        );
        assert_eq!(app.state.view, View::WorkflowRunDetail);
    }

    // ── drill-into-child / go_back nav stack ──────────────────────────

    #[test]
    fn go_back_workflow_run_detail_pops_nav_stack() {
        let mut app = make_test_app();
        app.state.view = View::WorkflowRunDetail;
        app.state.previous_view = Some(View::Dashboard);
        // Stack has parent run; currently viewing child run.
        app.state.workflow_run_nav_stack = vec!["parent-run".into()];
        app.state.selected_workflow_run_id = Some("child-run".into());
        app.go_back();
        // Should pop stack and restore parent run, not exit the view.
        assert_eq!(app.state.view, View::WorkflowRunDetail);
        assert!(app.state.workflow_run_nav_stack.is_empty());
        assert_eq!(
            app.state.selected_workflow_run_id.as_deref(),
            Some("parent-run")
        );
    }

    #[test]
    fn enter_workflow_run_detail_clears_nav_stack() {
        let mut app = make_test_app();
        // Pre-populate the nav stack as if we had drilled into a child during a prior session.
        app.state.workflow_run_nav_stack = vec!["old-parent".into(), "old-child".into()];
        app.state.view = View::Dashboard;
        app.enter_workflow_run_detail("new-run".into(), None);
        assert!(
            app.state.workflow_run_nav_stack.is_empty(),
            "nav stack must be cleared when entering a new top-level workflow run"
        );
        assert_eq!(
            app.state.selected_workflow_run_id.as_deref(),
            Some("new-run")
        );
        assert_eq!(app.state.view, View::WorkflowRunDetail);
    }

    #[test]
    fn drill_into_child_noop_when_child_not_found() {
        let mut app = make_test_app();
        app.state.view = View::WorkflowRunDetail;
        app.state.selected_workflow_run_id = Some("parent-run".into());
        // workflow_runs is empty — child run not yet loaded
        let step = crate::state::tests::make_wf_step("s1", "parent-run", "call-child", 0);
        app.state.data.workflow_steps = vec![conductor_core::workflow::WorkflowRunStep {
            role: "workflow".into(),
            child_run_id: Some("missing-child".into()),
            ..step
        }];
        app.state.workflow_step_index = 0;
        app.state.data.workflow_runs = vec![];
        app.select();
        // No drill should have happened: stack still empty, run unchanged.
        assert!(app.state.workflow_run_nav_stack.is_empty());
        assert_eq!(
            app.state.selected_workflow_run_id.as_deref(),
            Some("parent-run")
        );
        assert!(app.state.status_message.is_some());
    }

    #[test]
    fn gates_navigation_syncs_arc_when_run_worktree_id_is_none() {
        let mut app = make_test_app();
        app.state.selected_worktree_id = None;
        let run = make_test_run_without_worktree("run1");
        app.state.data.workflow_runs = vec![run];
        app.state.detail_gates = vec![make_gate("s1", "run1", "gate1")];
        app.state.detail_gate_index = 0;
        app.state.column_focus = ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Gates;
        app.workflow_column_select();
        assert_eq!(
            *app.selected_worktree_id_shared.lock().unwrap(),
            None,
            "Arc must be synced to None when run has no worktree_id"
        );
        assert_eq!(app.state.view, View::WorkflowRunDetail);
    }

    #[test]
    fn enter_on_workflow_step_drills_into_child_run() {
        use conductor_core::workflow::STEP_ROLE_WORKFLOW;
        let mut app = make_test_app();
        app.state.view = View::WorkflowRunDetail;
        app.state.selected_workflow_run_id = Some("parent-run".into());
        let mut step = crate::state::tests::make_wf_step("s1", "parent-run", "child-wf", 0);
        step.role = STEP_ROLE_WORKFLOW.into();
        step.child_run_id = Some("child-run".into());
        app.state.data.workflow_steps = vec![step];
        app.state.workflow_step_index = 0;
        app.select();
        assert_eq!(
            app.state.workflow_run_nav_stack,
            vec!["parent-run".to_string()],
            "parent run id must be pushed onto the nav stack"
        );
        assert_eq!(
            app.state.selected_workflow_run_id,
            Some("child-run".into()),
            "selected run must switch to the child run"
        );
    }
}
