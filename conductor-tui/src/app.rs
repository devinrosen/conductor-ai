use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;
use rusqlite::Connection;

use conductor_core::agent::{AgentManager, AgentRun, FeedbackRequest};
use conductor_core::config::{save_config, AutoStartAgent, Config, WorkTarget};
use conductor_core::github;
use conductor_core::issue_source::IssueSourceManager;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::workflow::MetadataEntry;
use conductor_core::worktree::WorktreeManager;

use crate::action::{Action, GithubDiscoverPayload};
use crate::background;
use crate::event::{BackgroundSender, EventLoop};
use crate::input;
use crate::state::{
    AppState, ConfirmAction, DashboardFocus, FormAction, FormField, InputAction, Modal,
    RepoDetailFocus, View, WorkflowRunDetailFocus, WorkflowsFocus,
};
use crate::ui;

/// Maximum scroll offset for a text body (total lines minus one visible line).
fn max_scroll(line_count: usize) -> u16 {
    line_count.saturating_sub(1) as u16
}

/// Increment `index` by one, clamping to `len - 1` (no-op when `len` is zero).
fn clamp_increment(index: &mut usize, len: usize) {
    let max = len.saturating_sub(1);
    if *index < max {
        *index += 1;
    }
}

/// Increment `index` by one, wrapping back to 0 when reaching `len`.
fn wrap_increment(index: &mut usize, len: usize) {
    if *index + 1 < len {
        *index += 1;
    } else {
        *index = 0;
    }
}

/// Decrement `index` by one, wrapping to `len - 1` when at 0.
fn wrap_decrement(index: &mut usize, len: usize) {
    if *index > 0 {
        *index -= 1;
    } else {
        *index = len.saturating_sub(1);
    }
}

/// Format structured [`MetadataEntry`] values into a fixed-width text block
/// suitable for the TUI modal.
fn format_metadata_entries(entries: &[MetadataEntry]) -> String {
    let pad = entries
        .iter()
        .filter_map(|e| match e {
            MetadataEntry::Field { label, .. } => Some(label.len()),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let mut parts: Vec<String> = Vec::new();
    for entry in entries {
        match entry {
            MetadataEntry::Field { label, value } => {
                parts.push(format!(
                    "{:<pad$}  {}",
                    format!("{label}:"),
                    value,
                    pad = pad + 1
                ));
            }
            MetadataEntry::Section { heading, body } => {
                parts.push(String::new());
                parts.push(format!("── {heading} ──"));
                parts.push(body.clone());
            }
        }
    }
    parts.join("\n")
}

/// Derive a worktree slug from a ticket's source_id and title.
/// Format: `{source_id}-{slugified-title}`, e.g. `15-tui-create-worktree`.
/// Title portion is truncated to keep the total slug under ~40 chars.
fn derive_worktree_slug(source_id: &str, title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive dashes
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }
    let title_slug = collapsed.trim_matches('-');

    // Budget: 40 chars total, minus source_id and separator
    let budget = 40_usize.saturating_sub(source_id.len() + 1);
    let truncated = if title_slug.len() <= budget {
        title_slug
    } else {
        match title_slug[..budget].rfind('-') {
            Some(pos) => &title_slug[..pos],
            None => &title_slug[..budget],
        }
    };

    format!("{}-{}", source_id, truncated)
}

pub struct App {
    state: AppState,
    conn: Connection,
    config: Config,
    bg_tx: Option<BackgroundSender>,
    /// Guard to prevent multiple concurrent workflow poll threads.
    workflow_poll_in_flight: Arc<AtomicBool>,
}

impl App {
    pub fn new(conn: Connection, config: Config) -> Self {
        Self {
            state: AppState::new(),
            conn,
            config,
            bg_tx: None,
            workflow_poll_in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Main run loop.
    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Initial data load
        self.refresh_data();

        let events = EventLoop::new(Duration::from_millis(200));

        // Spawn background workers
        let bg_tx = events.bg_sender();
        self.bg_tx = Some(bg_tx.clone());
        background::spawn_db_poller(bg_tx.clone(), Duration::from_secs(5));
        let sync_mins = self.config.general.sync_interval_minutes as u64;
        background::spawn_ticket_sync(bg_tx, Duration::from_secs(sync_mins * 60));

        let mut dirty = true; // tracks whether state changed since last draw

        loop {
            // Only redraw when state has actually changed.
            if dirty {
                terminal.draw(|frame| ui::render(frame, &self.state))?;
                dirty = false;
            }

            // Block until at least one event is available
            events.wait();

            // PRIORITY 1: Drain all key events first — input is never starved
            for key in events.drain_input() {
                let action = input::map_key(key, &self.state);
                dirty |= self.update(action);
            }

            // PRIORITY 2: Drain all background events
            let bg_actions = events.drain_background();
            for action in bg_actions {
                dirty |= self.update(action);
            }

            if self.state.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Handle an action by mutating state. Returns true if the UI needs a redraw.
    ///
    /// This thin wrapper delegates to `handle_action` and updates
    /// `status_message_at` whenever the status message presence changes.
    fn update(&mut self, action: Action) -> bool {
        let had_message = self.state.status_message.is_some();
        let dirty = self.handle_action(action);
        self.state.track_status_message_change(had_message);
        dirty
    }

    fn handle_action(&mut self, action: Action) -> bool {
        // Manage pending_g chord state
        match &action {
            Action::PendingG => {
                self.state.pending_g = true;
                return true;
            }
            Action::None | Action::Tick => {
                // Don't affect pending_g on no-ops / ticks
            }
            _ => {
                self.state.pending_g = false;
            }
        }

        match action {
            Action::None => return false,
            Action::Tick => {
                // Poll workflow data asynchronously on every tick so the global
                // status bar (and workflow views) stay current regardless of which
                // view is active.
                self.poll_workflow_data_async();
                // Auto-clear status messages after 4 seconds so the context hint
                // bar is restored without requiring user navigation.
                self.state.tick_status_message(Duration::from_secs(4));
                // Always redraw on tick so elapsed times, spinners, and other
                // time-sensitive indicators update smoothly (ratatui diffs cells,
                // so this is cheap).
                return true;
            }
            Action::Quit => self.state.should_quit = true,

            // Navigation
            Action::Back => self.go_back(),
            Action::NextPanel => self.next_panel(),
            Action::PrevPanel => self.prev_panel(),
            Action::MoveUp => self.move_up(),
            Action::MoveDown => self.move_down(),
            Action::ScrollLeft => {
                if let Modal::EventDetail {
                    ref mut horizontal_offset,
                    ..
                } = self.state.modal
                {
                    *horizontal_offset = horizontal_offset.saturating_sub(4);
                }
            }
            Action::ScrollRight => {
                if let Modal::EventDetail {
                    ref mut horizontal_offset,
                    ..
                } = self.state.modal
                {
                    *horizontal_offset += 4;
                }
            }
            Action::Select => self.select(),

            // View navigation
            Action::GoToDashboard => {
                self.state.view = View::Dashboard;
            }
            Action::GoToTickets => {
                self.state.view = View::Tickets;
                self.state.ticket_index = 0;
            }
            Action::GoToWorkflows => {
                // Navigate to the global workflows view.
                // If a worktree is already selected (e.g. from WorktreeDetail), keep it
                // for scoped defs/runs; otherwise show all runs across all worktrees.
                self.state.view = View::Workflows;
                self.state.workflows_focus = crate::state::WorkflowsFocus::Runs;
                self.state.workflow_def_index = 0;
                self.state.workflow_run_index = 0;
                self.reload_workflow_data();
            }

            // Filter
            Action::EnterFilter => self.state.active_filter_mut().enter(),
            Action::FilterChar(c) => {
                self.state.active_filter_mut().push(c);
                self.state.rebuild_filtered_tickets();
            }
            Action::FilterBackspace => {
                self.state.active_filter_mut().backspace();
                self.state.rebuild_filtered_tickets();
            }
            Action::ExitFilter => self.state.active_filter_mut().exit(),

            // Modal
            Action::ShowHelp => {
                self.state.modal = Modal::Help;
            }
            Action::DismissModal => {
                self.state.modal = Modal::None;
            }
            Action::OpenTicketUrl => self.handle_open_ticket_url(),
            Action::ConfirmYes => self.handle_confirm_yes(),
            Action::ConfirmNo => {
                self.state.modal = Modal::None;
            }
            Action::InputChar(c) => match self.state.modal {
                Modal::Input { ref mut value, .. } | Modal::ConfirmByName { ref mut value, .. } => {
                    value.push(c);
                }
                Modal::ModelPicker {
                    ref mut custom_input,
                    custom_active: true,
                    ..
                } => {
                    custom_input.push(c);
                }
                _ => {}
            },
            Action::InputBackspace => match self.state.modal {
                Modal::Input { ref mut value, .. } | Modal::ConfirmByName { ref mut value, .. } => {
                    value.pop();
                }
                Modal::ModelPicker {
                    ref mut custom_input,
                    custom_active: true,
                    ..
                } => {
                    custom_input.pop();
                }
                Modal::ModelPicker {
                    custom_active: false,
                    ref on_submit,
                    ..
                } => {
                    // Backspace in non-custom mode: clear the model (submit empty value)
                    let on_submit = on_submit.clone();
                    self.state.modal = Modal::None;
                    self.handle_input_submit_with_value(String::new(), on_submit);
                    return true;
                }
                _ => {}
            },
            Action::InputSubmit => self.handle_input_submit(),
            Action::TextAreaInput(key) => {
                if let Modal::AgentPrompt {
                    ref mut textarea, ..
                } = self.state.modal
                {
                    textarea.input(key);
                }
            }
            Action::TextAreaClear => {
                if let Modal::AgentPrompt {
                    ref mut textarea, ..
                } = self.state.modal
                {
                    **textarea = tui_textarea::TextArea::new(vec![String::new()]);
                    textarea.set_cursor_line_style(ratatui::style::Style::default());
                    textarea.set_placeholder_text("Type your prompt here...");
                }
            }
            Action::FormChar(c) => self.handle_form_char(c),
            Action::FormBackspace => self.handle_form_backspace(),
            Action::FormNextField => self.handle_form_next_field(),
            Action::FormPrevField => self.handle_form_prev_field(),
            Action::FormSubmit => self.handle_form_submit(),

            // CRUD
            Action::AddRepo => self.handle_add_repo(),
            Action::Create => self.handle_create(),
            Action::Delete => self.handle_delete(),
            Action::Push => self.handle_push(),
            Action::CreatePr => self.handle_create_pr(),
            Action::SyncTickets => self.handle_sync_tickets(),
            Action::LinkTicket => self.handle_link_ticket(),
            Action::StartWork => self.handle_start_work(),
            Action::SelectWorkTarget(index) => self.handle_select_work_target(index),
            Action::ManageWorkTargets => self.handle_manage_work_targets(),
            Action::ManageIssueSources => self.handle_manage_issue_sources(),
            Action::IssueSourceAdd => self.handle_issue_source_add(),
            Action::IssueSourceDelete => self.handle_issue_source_delete(),
            Action::DiscoverGithubOrgs => self.handle_discover_github_orgs(),
            Action::GithubOrgsLoaded { orgs } => self.handle_github_orgs_loaded(orgs),
            Action::GithubOrgsFailed { error } => self.handle_github_orgs_failed(error),
            Action::GithubDrillIntoOwner { owner } => self.handle_github_drill_into_owner(owner),
            Action::GithubBackToOrgs => self.handle_github_back_to_orgs(),
            Action::GithubDiscoverLoaded(payload) => self.handle_github_discover_loaded(*payload),
            Action::GithubDiscoverFailed { error } => self.handle_github_discover_failed(error),
            Action::GithubDiscoverToggle => self.handle_github_discover_toggle(),
            Action::GithubDiscoverSelectAll => self.handle_github_discover_select_all(),
            Action::GithubDiscoverImport => self.handle_github_discover_import(),
            Action::WorkTargetMoveUp => self.handle_work_target_move_up(),
            Action::WorkTargetMoveDown => self.handle_work_target_move_down(),
            Action::WorkTargetAdd => self.handle_work_target_add(),
            Action::WorkTargetDelete => self.handle_work_target_delete(),

            // Model configuration
            Action::SetModel => self.handle_set_model(),

            // Agent issue creation toggle
            Action::ToggleAgentIssues => self.handle_toggle_agent_issues(),

            // Ticket closed visibility toggle
            Action::ToggleClosedTickets => {
                self.state.show_closed_tickets = !self.state.show_closed_tickets;
                self.state.rebuild_filtered_tickets();
                // Reset selection indices so they don't point past the filtered list
                self.state.ticket_index = 0;
                self.state.detail_ticket_index = 0;
            }

            // Global status bar toggle (expand/collapse detail line for 4+ active items)
            Action::ToggleStatusBar => {
                self.state.status_bar_expanded = !self.state.status_bar_expanded;
            }

            // Agent (tmux-based)
            Action::LaunchAgent => self.handle_launch_agent(),
            Action::OrchestrateAgent => self.handle_orchestrate_agent(),
            Action::StopAgent => self.handle_stop_agent(),
            Action::AttachAgent => self.handle_attach_agent(),
            Action::SubmitFeedback => self.handle_submit_feedback(),
            Action::DismissFeedback => self.handle_dismiss_feedback(),
            Action::ViewAgentLog => self.handle_view_agent_log(),
            Action::CopyLastCodeBlock => self.handle_copy_last_code_block(),
            Action::ExpandAgentEvent => self.handle_expand_agent_event(),
            Action::AgentActivityDown => {
                let len = self.state.data.agent_activity_len();
                let cur = self.state.agent_list_state.borrow().selected().unwrap_or(0);
                if len > 0 && cur + 1 < len {
                    self.state.agent_list_state.borrow_mut().select_next();
                }
            }
            Action::AgentActivityUp => {
                self.state.agent_list_state.borrow_mut().select_previous();
            }
            // Scroll navigation (all views + discover modals)
            Action::GoToTop => match self.state.modal {
                Modal::EventDetail {
                    ref mut scroll_offset,
                    ref mut horizontal_offset,
                    ..
                } => {
                    *scroll_offset = 0;
                    *horizontal_offset = 0;
                }
                Modal::GithubDiscoverOrgs { ref mut cursor, .. }
                | Modal::GithubDiscover { ref mut cursor, .. } => {
                    *cursor = 0;
                }
                _ => {
                    self.state.set_focused_index(0);
                }
            },
            Action::GoToBottom => match self.state.modal {
                Modal::EventDetail {
                    ref mut scroll_offset,
                    line_count,
                    ..
                } => {
                    *scroll_offset = max_scroll(line_count);
                }
                Modal::GithubDiscoverOrgs {
                    ref orgs,
                    ref mut cursor,
                    ..
                } => {
                    *cursor = orgs.len().saturating_sub(1);
                }
                Modal::GithubDiscover {
                    ref repos,
                    ref mut cursor,
                    ..
                } => {
                    *cursor = repos.len().saturating_sub(1);
                }
                _ => {
                    let (_, len) = self.state.focused_index_and_len();
                    self.state.set_focused_index(len.saturating_sub(1));
                }
            },
            Action::HalfPageDown => {
                let half = self.half_page_size();
                match self.state.modal {
                    Modal::GithubDiscoverOrgs {
                        ref orgs,
                        ref mut cursor,
                        ..
                    } => {
                        *cursor = (*cursor + half).min(orgs.len().saturating_sub(1));
                    }
                    Modal::GithubDiscover {
                        ref repos,
                        ref mut cursor,
                        ..
                    } => {
                        *cursor = (*cursor + half).min(repos.len().saturating_sub(1));
                    }
                    _ => {
                        let (idx, len) = self.state.focused_index_and_len();
                        self.state
                            .set_focused_index((idx + half).min(len.saturating_sub(1)));
                    }
                }
            }
            Action::HalfPageUp => {
                let half = self.half_page_size();
                match self.state.modal {
                    Modal::GithubDiscoverOrgs { ref mut cursor, .. }
                    | Modal::GithubDiscover { ref mut cursor, .. } => {
                        *cursor = cursor.saturating_sub(half);
                    }
                    _ => {
                        let (idx, _) = self.state.focused_index_and_len();
                        self.state.set_focused_index(idx.saturating_sub(half));
                    }
                }
            }

            // Workflow actions
            Action::RunWorkflow => self.handle_run_workflow(),
            Action::CancelWorkflow => self.handle_cancel_workflow(),
            Action::ApproveGate => self.handle_approve_gate(),
            Action::RejectGate => self.handle_reject_gate(),
            Action::ViewWorkflowDef => self.handle_view_workflow_def(),
            Action::EditWorkflowDef => self.handle_edit_workflow_def(),
            Action::GateInputChar(c) => {
                if let Modal::GateAction {
                    ref mut feedback, ..
                } = self.state.modal
                {
                    feedback.push(c);
                }
            }
            Action::GateInputBackspace => {
                if let Modal::GateAction {
                    ref mut feedback, ..
                } = self.state.modal
                {
                    feedback.pop();
                }
            }
            Action::WorkflowDataRefreshed(payload) => {
                self.state.data.workflow_defs = payload.workflow_defs;
                self.state.data.workflow_runs = payload.workflow_runs;
                self.state.data.workflow_steps = payload.workflow_steps;
                self.state.data.step_agent_events = payload.step_agent_events;
                self.state.data.step_agent_run = payload.step_agent_run;
                self.clamp_workflow_indices();
                return matches!(self.state.view, View::Workflows | View::WorkflowRunDetail);
            }

            // PendingG is handled above before the match
            Action::PendingG => unreachable!(),

            // Background results
            Action::DataRefreshed(payload) => {
                self.state.data.repos = payload.repos;
                self.state.data.worktrees = payload.worktrees;
                self.state.data.tickets = payload.tickets;
                self.state.data.latest_agent_runs = payload.latest_agent_runs;
                self.state.data.ticket_agent_totals = payload.ticket_agent_totals;
                self.state.data.latest_workflow_runs_by_worktree =
                    payload.latest_workflow_runs_by_worktree;
                self.refresh_pending_feedback();
                self.state.data.rebuild_maps();
                self.reload_agent_events();
                self.state.rebuild_filtered_tickets();
                self.clamp_indices();
                // Redraw when viewing worktree detail / workflows, or on the
                // dashboard (which now has a live workflow panel).
                return matches!(
                    self.state.view,
                    View::Dashboard
                        | View::WorktreeDetail
                        | View::Workflows
                        | View::WorkflowRunDetail
                );
            }
            Action::TicketSyncComplete { repo_slug, count } => {
                self.state.status_message = Some(format!("Synced {count} tickets for {repo_slug}"));
            }
            Action::TicketSyncFailed { repo_slug, error } => {
                self.state.status_message = Some(format!("Sync failed for {repo_slug}: {error}"));
            }
            Action::TicketSyncDone => {
                self.state.ticket_sync_in_progress = false;
                self.refresh_data();
            }
            Action::BackgroundError { message } => {
                self.state.modal = Modal::Error { message };
            }
            Action::BackgroundSuccess { message } => {
                self.state.status_message = Some(message);
                self.refresh_data();
            }
            Action::DeleteWorktreeReady {
                repo_slug,
                wt_slug,
                issue_closed,
                pr_merged,
                has_ticket,
            } => {
                self.state.status_message = None;
                self.show_delete_worktree_modal(
                    &repo_slug,
                    &wt_slug,
                    issue_closed,
                    pr_merged,
                    has_ticket,
                );
            }
        }
        true
    }

    /// Approximate half-page size for the agent activity pane.
    fn half_page_size(&self) -> usize {
        let (_, height) = crossterm::terminal::size().unwrap_or((80, 24));
        // Agent activity pane is roughly the bottom half of the terminal.
        // Use terminal height / 3 as a reasonable half-page for that pane.
        (height as usize / 3).max(1)
    }

    fn refresh_data(&mut self) {
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
            self.state.detail_worktrees = self
                .state
                .data
                .worktrees
                .iter()
                .filter(|wt| &wt.repo_id == repo_id)
                .cloned()
                .collect();
            self.state.detail_tickets = self
                .state
                .data
                .tickets
                .iter()
                .filter(|t| &t.repo_id == repo_id)
                .cloned()
                .collect();
        }

        self.state.rebuild_filtered_tickets();
        self.clamp_indices();
    }

    fn reload_agent_events(&mut self) {
        use conductor_core::agent::{
            count_turns_in_log, parse_agent_log, AgentManager, AgentRunEvent,
        };

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
        }

        // For running agents, count live turns from the log file
        if let Some(run) = runs.last() {
            if run.status == conductor_core::agent::AgentRunStatus::Running {
                if let Some(ref path) = run.log_file {
                    totals.live_turns = count_turns_in_log(path);
                }
            }
        }

        self.state.data.agent_totals = totals;

        // Load events: prefer DB records, fall back to log file parsing for older runs
        let db_events = mgr.list_events_for_worktree(wt_id).unwrap_or_default();
        let all_events = if !db_events.is_empty() {
            db_events
        } else {
            // Backward compat: parse log files and wrap as AgentRunEvent without timing
            let mut fallback = Vec::new();
            for run in &runs {
                if let Some(ref path) = run.log_file {
                    let events = parse_agent_log(path);
                    for ev in events {
                        fallback.push(AgentRunEvent {
                            id: ulid::Ulid::new().to_string(),
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

    fn clamp_indices(&mut self) {
        let repos_len = self.state.data.repos.len();
        if repos_len > 0 && self.state.repo_index >= repos_len {
            self.state.repo_index = repos_len - 1;
        }

        let wt_len = self.state.data.worktrees.len();
        if wt_len > 0 && self.state.worktree_index >= wt_len {
            self.state.worktree_index = wt_len - 1;
        }

        let t_len = self.state.filtered_tickets.len();
        if t_len > 0 && self.state.ticket_index >= t_len {
            self.state.ticket_index = t_len - 1;
        }

        let dt_len = self.state.filtered_detail_tickets.len();
        if dt_len > 0 && self.state.detail_ticket_index >= dt_len {
            self.state.detail_ticket_index = dt_len - 1;
        }
    }

    fn go_back(&mut self) {
        match self.state.view {
            View::Dashboard => self.state.should_quit = true,
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
            View::Tickets => {
                self.state.view = View::Dashboard;
            }
            View::Workflows => {
                // Go back to worktree detail if we came from there
                if self.state.selected_worktree_id.is_some() {
                    self.state.view = View::WorktreeDetail;
                    *self.state.agent_list_state.borrow_mut() = ListState::default();
                    self.reload_agent_events();
                } else {
                    self.state.view = View::Dashboard;
                }
            }
            View::WorkflowRunDetail => {
                self.state.view = View::Workflows;
                self.state.selected_workflow_run_id = None;
            }
        }
    }

    fn next_panel(&mut self) {
        match self.state.view {
            View::Dashboard => {
                self.state.dashboard_focus = self.state.dashboard_focus.next();
            }
            View::RepoDetail => {
                self.state.repo_detail_focus = self.state.repo_detail_focus.toggle();
            }
            View::Workflows => {
                self.state.workflows_focus = self.state.workflows_focus.toggle();
            }
            View::WorkflowRunDetail => {
                // Only toggle if the selected step has agent activity
                let has_agent = self
                    .state
                    .data
                    .workflow_steps
                    .get(self.state.workflow_step_index)
                    .map(|s| s.child_run_id.is_some())
                    .unwrap_or(false);
                if has_agent {
                    self.state.workflow_run_detail_focus =
                        self.state.workflow_run_detail_focus.toggle();
                }
            }
            _ => {}
        }
    }

    fn prev_panel(&mut self) {
        match self.state.view {
            View::Dashboard => {
                self.state.dashboard_focus = self.state.dashboard_focus.prev();
            }
            View::RepoDetail => {
                self.state.repo_detail_focus = self.state.repo_detail_focus.toggle();
            }
            View::Workflows => {
                self.state.workflows_focus = self.state.workflows_focus.toggle();
            }
            View::WorkflowRunDetail => {
                let has_agent = self
                    .state
                    .data
                    .workflow_steps
                    .get(self.state.workflow_step_index)
                    .map(|s| s.child_run_id.is_some())
                    .unwrap_or(false);
                if has_agent {
                    self.state.workflow_run_detail_focus =
                        self.state.workflow_run_detail_focus.toggle();
                }
            }
            _ => {}
        }
    }

    fn move_up(&mut self) {
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
                ..
            } => {
                *custom_active = false;
                let total = conductor_core::models::KNOWN_MODELS.len() + 1; // +1 for custom
                wrap_decrement(selected, total);
                return;
            }
            Modal::WorkTargetPicker {
                ref targets,
                ref mut selected,
            } => {
                wrap_decrement(selected, targets.len());
                return;
            }
            Modal::WorkTargetManager {
                ref targets,
                ref mut selected,
            } => {
                wrap_decrement(selected, targets.len());
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
        match self.state.view {
            View::Dashboard => match self.state.dashboard_focus {
                DashboardFocus::Repos => {
                    self.state.repo_index = self.state.repo_index.saturating_sub(1);
                }
                DashboardFocus::Worktrees => {
                    self.state.worktree_index = self.state.worktree_index.saturating_sub(1);
                }
                DashboardFocus::Tickets => {
                    self.state.ticket_index = self.state.ticket_index.saturating_sub(1);
                }
            },
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Worktrees => {
                    self.state.detail_wt_index = self.state.detail_wt_index.saturating_sub(1);
                }
                RepoDetailFocus::Tickets => {
                    self.state.detail_ticket_index =
                        self.state.detail_ticket_index.saturating_sub(1);
                }
            },
            View::Tickets => {
                self.state.ticket_index = self.state.ticket_index.saturating_sub(1);
            }
            View::Workflows => match self.state.workflows_focus {
                WorkflowsFocus::Defs => {
                    self.state.workflow_def_index = self.state.workflow_def_index.saturating_sub(1);
                }
                WorkflowsFocus::Runs => {
                    self.state.workflow_run_index = self.state.workflow_run_index.saturating_sub(1);
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
            _ => {}
        }
    }

    fn move_down(&mut self) {
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
                ..
            } => {
                *custom_active = false;
                let total = conductor_core::models::KNOWN_MODELS.len() + 1; // +1 for custom
                wrap_increment(selected, total);
                return;
            }
            Modal::WorkTargetPicker {
                ref targets,
                ref mut selected,
            } => {
                wrap_increment(selected, targets.len());
                return;
            }
            Modal::WorkTargetManager {
                ref targets,
                ref mut selected,
            } => {
                wrap_increment(selected, targets.len());
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
        match self.state.view {
            View::Dashboard => match self.state.dashboard_focus {
                DashboardFocus::Repos => {
                    clamp_increment(&mut self.state.repo_index, self.state.data.repos.len());
                }
                DashboardFocus::Worktrees => {
                    clamp_increment(
                        &mut self.state.worktree_index,
                        self.state.data.worktrees.len(),
                    );
                }
                DashboardFocus::Tickets => {
                    clamp_increment(
                        &mut self.state.ticket_index,
                        self.state.filtered_tickets.len(),
                    );
                }
            },
            View::RepoDetail => match self.state.repo_detail_focus {
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
            },
            View::Tickets => {
                clamp_increment(
                    &mut self.state.ticket_index,
                    self.state.filtered_tickets.len(),
                );
            }
            View::Workflows => match self.state.workflows_focus {
                WorkflowsFocus::Defs => {
                    clamp_increment(
                        &mut self.state.workflow_def_index,
                        self.state.data.workflow_defs.len(),
                    );
                }
                WorkflowsFocus::Runs => {
                    clamp_increment(
                        &mut self.state.workflow_run_index,
                        self.state.data.workflow_runs.len(),
                    );
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
            _ => {}
        }
    }

    fn select(&mut self) {
        match self.state.view {
            View::Dashboard => match self.state.dashboard_focus {
                DashboardFocus::Repos => {
                    if let Some(repo) = self.state.selected_repo() {
                        let repo_id = repo.id.clone();
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
                        self.state.rebuild_filtered_tickets();
                        self.state.repo_detail_focus = RepoDetailFocus::Worktrees;
                        self.state.view = View::RepoDetail;
                    }
                }
                DashboardFocus::Worktrees => {
                    if let Some(wt) = self.state.selected_worktree() {
                        let wt_id = wt.id.clone();
                        self.state.selected_worktree_id = Some(wt_id);
                        self.state.selected_repo_id = None;
                        self.state.view = View::WorktreeDetail;
                        *self.state.agent_list_state.borrow_mut() = ListState::default();
                        self.reload_agent_events();
                    }
                }
                DashboardFocus::Tickets => {
                    if let Some(ticket) = self.state.filtered_tickets.get(self.state.ticket_index) {
                        self.state.modal = Modal::TicketInfo {
                            ticket: Box::new(ticket.clone()),
                        };
                    }
                }
            },
            View::RepoDetail => match self.state.repo_detail_focus {
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
            },
            View::Tickets => {
                if let Some(ticket) = self.state.filtered_tickets.get(self.state.ticket_index) {
                    self.state.modal = Modal::TicketInfo {
                        ticket: Box::new(ticket.clone()),
                    };
                }
            }
            View::Workflows => {
                match self.state.workflows_focus {
                    WorkflowsFocus::Defs => {
                        // Run selected workflow definition
                        self.handle_run_workflow();
                    }
                    WorkflowsFocus::Runs => {
                        // Enter workflow run detail
                        if let Some(run) = self
                            .state
                            .data
                            .workflow_runs
                            .get(self.state.workflow_run_index)
                        {
                            let run_id = run.id.clone();
                            // In global mode, set the worktree context from the run
                            if self.state.selected_worktree_id.is_none() {
                                self.state.selected_worktree_id = Some(run.worktree_id.clone());
                            }
                            self.state.selected_workflow_run_id = Some(run_id);
                            self.state.view = View::WorkflowRunDetail;
                            self.state.workflow_step_index = 0;
                            self.state.workflow_run_detail_focus = WorkflowRunDetailFocus::Steps;
                            self.state.step_agent_event_index = 0;
                            self.reload_workflow_steps();
                        }
                    }
                }
            }
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
                        let body = format_metadata_entries(&step.metadata_fields());
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
        }
    }

    fn handle_open_ticket_url(&mut self) {
        // Resolve the ticket URL from either the TicketInfo modal or the WorktreeDetail view
        let url = if let Modal::TicketInfo { ref ticket } = self.state.modal {
            Some(ticket.url.clone())
        } else if self.state.view == View::WorktreeDetail {
            self.state
                .selected_worktree_id
                .as_ref()
                .and_then(|wt_id| self.state.data.worktrees.iter().find(|w| &w.id == wt_id))
                .and_then(|wt| wt.ticket_id.as_ref())
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .map(|t| t.url.clone())
        } else {
            None
        };

        let Some(url) = url else {
            if self.state.view == View::WorktreeDetail {
                self.state.status_message = Some("No ticket linked to this worktree".to_string());
            }
            return;
        };

        if url.is_empty() {
            self.state.status_message = Some("No URL available".to_string());
            return;
        }

        let result = Command::new("open").arg(&url).output();
        match result {
            Ok(o) if o.status.success() => {
                self.state.status_message = Some(format!("Opened {url}"));
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                self.state.status_message = Some(format!("Failed to open URL: {stderr}"));
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to open URL: {e}"));
            }
        }
    }

    fn handle_confirm_yes(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::Confirm { on_confirm, .. } = modal {
            self.execute_confirm_action(on_confirm);
        }
    }

    fn execute_confirm_action(&mut self, on_confirm: ConfirmAction) {
        match on_confirm {
            ConfirmAction::DeleteWorktree { repo_slug, wt_slug } => {
                let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
                match wt_mgr.delete(&repo_slug, &wt_slug) {
                    Ok(wt) => {
                        self.state.status_message =
                            Some(format!("Worktree {} marked as {}", wt_slug, wt.status));
                        self.state.view = View::Dashboard;
                        self.state.selected_worktree_id = None;
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Delete failed: {e}"),
                        };
                    }
                }
            }
            ConfirmAction::RemoveRepo { repo_slug } => {
                let mgr = RepoManager::new(&self.conn, &self.config);
                match mgr.remove(&repo_slug) {
                    Ok(()) => {
                        self.state.status_message = Some(format!("Removed repo: {repo_slug}"));
                        self.state.view = View::Dashboard;
                        self.state.selected_repo_id = None;
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Remove failed: {e}"),
                        };
                    }
                }
            }
            ConfirmAction::StartAgentForWorktree {
                worktree_id,
                worktree_path,
                worktree_slug,
                ticket_id,
            } => {
                self.show_agent_prompt_for_ticket(
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                );
            }
            ConfirmAction::DeleteWorkTarget { index } => {
                if index < self.config.general.work_targets.len() {
                    let removed = self.config.general.work_targets.remove(index);
                    match save_config(&self.config) {
                        Ok(()) => {
                            let new_selected =
                                index.min(self.config.general.work_targets.len().saturating_sub(1));
                            self.state.modal = Modal::WorkTargetManager {
                                targets: self.config.general.work_targets.clone(),
                                selected: new_selected,
                            };
                            self.state.status_message =
                                Some(format!("Deleted work target: {}", removed.name));
                        }
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("Failed to save config: {e}"),
                            };
                        }
                    }
                }
            }
            ConfirmAction::CancelWorkflow { workflow_run_id } => {
                use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};
                let wf_mgr = WorkflowManager::new(&self.conn);
                match wf_mgr.update_workflow_status(
                    &workflow_run_id,
                    WorkflowRunStatus::Cancelled,
                    Some("Cancelled by user"),
                ) {
                    Ok(()) => {
                        self.state.status_message = Some("Workflow run cancelled".to_string());
                        self.reload_workflow_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Cancel failed: {e}"),
                        };
                    }
                }
            }
            ConfirmAction::DeleteIssueSource {
                source_id,
                repo_id,
                repo_slug,
                remote_url,
            } => {
                let mgr = IssueSourceManager::new(&self.conn);
                match mgr.remove(&source_id) {
                    Ok(()) => {
                        let sources = mgr.list(&repo_id).unwrap_or_default();
                        self.state.modal = Modal::IssueSourceManager {
                            repo_id,
                            repo_slug: repo_slug.clone(),
                            remote_url,
                            sources,
                            selected: 0,
                        };
                        self.state.status_message =
                            Some(format!("Removed issue source from {repo_slug}"));
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Failed to remove source: {e}"),
                        };
                    }
                }
            }
        }
    }

    fn handle_input_submit(&mut self) {
        // ConfirmByName: only proceed if typed value matches expected slug
        if let Modal::ConfirmByName {
            ref expected,
            ref value,
            ..
        } = self.state.modal
        {
            if value != expected {
                return; // ignore Enter when text doesn't match
            }
            // Value matches — take the modal and execute
            let modal = std::mem::replace(&mut self.state.modal, Modal::None);
            if let Modal::ConfirmByName { on_confirm, .. } = modal {
                self.execute_confirm_action(on_confirm);
            }
            return;
        }

        let modal = std::mem::replace(&mut self.state.modal, Modal::None);

        // Extract (value, on_submit) from Input, AgentPrompt, or ModelPicker modals
        let (value, on_submit) = match modal {
            Modal::Input {
                value, on_submit, ..
            } => (value, on_submit),
            Modal::AgentPrompt {
                textarea,
                on_submit,
                ..
            } => {
                let value = textarea.lines().join("\n");
                (value, on_submit)
            }
            Modal::ModelPicker {
                context_label,
                effective_default,
                effective_source,
                selected,
                custom_input,
                custom_active,
                suggested,
                on_submit,
            } => {
                let models = conductor_core::models::KNOWN_MODELS;
                let value = if custom_active {
                    custom_input
                } else if selected < models.len() {
                    // Selected a known model — use its alias
                    models[selected].alias.to_string()
                } else {
                    // "custom…" was highlighted but Enter pressed without typing:
                    // re-open the picker with custom mode active
                    self.state.modal = Modal::ModelPicker {
                        context_label,
                        effective_default,
                        effective_source,
                        selected,
                        custom_input,
                        custom_active: true,
                        suggested,
                        on_submit,
                    };
                    return;
                };
                (value, on_submit)
            }
            _ => return,
        };

        match on_submit {
            InputAction::CreateWorktree {
                repo_slug,
                ticket_id,
            } => {
                if value.is_empty() {
                    return;
                }
                let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
                match wt_mgr.create(&repo_slug, &value, None, ticket_id.as_deref()) {
                    Ok((wt, warnings)) => {
                        let mut msg = if let Some(ref tid) = ticket_id {
                            let source_id = self
                                .state
                                .data
                                .ticket_map
                                .get(tid)
                                .map(|t| t.source_id.as_str())
                                .unwrap_or("?");
                            format!("Created worktree: {} (linked to #{})", wt.slug, source_id)
                        } else {
                            format!("Created worktree: {}", wt.slug)
                        };
                        if !warnings.is_empty() {
                            msg.push_str(&format!(" [{}]", warnings.join("; ")));
                        }
                        self.state.status_message = Some(msg);
                        self.refresh_data();

                        if let Some(tid) = ticket_id {
                            self.maybe_start_agent_for_worktree(wt.id, wt.path, wt.slug, tid);
                        }
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Create failed: {e}"),
                        };
                    }
                }
            }
            InputAction::LinkTicket { worktree_id } => {
                if value.is_empty() {
                    return;
                }
                let syncer = TicketSyncer::new(&self.conn);
                // Find ticket by source_id, scoped to the worktree's repo
                let wt_repo_id = self
                    .state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| w.id == worktree_id)
                    .map(|w| w.repo_id.as_str());
                let ticket = self
                    .state
                    .data
                    .tickets
                    .iter()
                    .find(|t| t.source_id == value && Some(t.repo_id.as_str()) == wt_repo_id);
                if let Some(t) = ticket {
                    match syncer.link_to_worktree(&t.id, &worktree_id) {
                        Ok(()) => {
                            self.state.status_message =
                                Some(format!("Linked ticket #{}", t.source_id));
                            self.refresh_data();
                        }
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("Link failed: {e}"),
                            };
                        }
                    }
                } else {
                    self.state.modal = Modal::Error {
                        message: format!("Ticket #{value} not found"),
                    };
                }
            }
            InputAction::AgentPrompt {
                worktree_id,
                worktree_path,
                worktree_slug,
                resume_session_id,
            } => {
                if value.is_empty() {
                    return;
                }
                // Resolve the default model: per-worktree → per-repo → global config
                let wt_model = self
                    .state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| w.id == worktree_id)
                    .and_then(|w| w.model.clone());
                let repo_model = self
                    .state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| w.id == worktree_id)
                    .and_then(|w| self.state.data.repos.iter().find(|r| r.id == w.repo_id))
                    .and_then(|r| r.model.clone());
                let resolved_default = wt_model
                    .or(repo_model)
                    .or_else(|| self.config.general.model.clone());

                // Suggest a model based on the prompt text
                let suggested = conductor_core::models::suggest_model(&value);

                // Pre-select the suggested model in the picker
                let initial_selected = conductor_core::models::KNOWN_MODELS
                    .iter()
                    .position(|m| m.alias == suggested)
                    .unwrap_or(1); // default to sonnet

                let (effective_default, effective_source) = match &resolved_default {
                    Some(m) => {
                        // Determine source
                        let source = if self
                            .state
                            .data
                            .worktrees
                            .iter()
                            .find(|w| w.id == worktree_id)
                            .and_then(|w| w.model.as_ref())
                            .is_some()
                        {
                            "worktree"
                        } else if self
                            .state
                            .data
                            .worktrees
                            .iter()
                            .find(|w| w.id == worktree_id)
                            .and_then(|w| self.state.data.repos.iter().find(|r| r.id == w.repo_id))
                            .and_then(|r| r.model.as_ref())
                            .is_some()
                        {
                            "repo"
                        } else {
                            "global config"
                        };
                        (Some(m.clone()), source.to_string())
                    }
                    None => (None, "not set".to_string()),
                };

                self.state.modal = Modal::ModelPicker {
                    context_label: "agent run".to_string(),
                    effective_default,
                    effective_source,
                    selected: initial_selected,
                    custom_input: String::new(),
                    custom_active: false,
                    suggested: Some(suggested.to_string()),
                    on_submit: InputAction::AgentModelOverride {
                        prompt: value,
                        worktree_id,
                        worktree_path,
                        worktree_slug,
                        resume_session_id,
                        resolved_default,
                    },
                };
            }
            InputAction::AgentModelOverride {
                prompt,
                worktree_id,
                worktree_path,
                worktree_slug,
                resume_session_id,
                resolved_default,
            } => {
                // Empty value means "use the resolved default"
                let model = if value.trim().is_empty() {
                    resolved_default
                } else {
                    Some(value)
                };
                self.start_agent_tmux(
                    prompt,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    resume_session_id,
                    model,
                );
            }
            InputAction::OrchestratePrompt {
                worktree_id,
                worktree_path,
                worktree_slug,
            } => {
                if value.is_empty() {
                    return;
                }
                self.start_orchestrate_tmux(value, worktree_id, worktree_path, worktree_slug);
            }
            InputAction::SetWorktreeModel {
                worktree_id,
                repo_slug,
                slug,
            } => {
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                let mgr = WorktreeManager::new(&self.conn, &self.config);
                match mgr.set_model(&repo_slug, &slug, model.as_deref()) {
                    Ok(()) => {
                        let msg = match &model {
                            Some(m) => format!("Model for {slug} set to: {m}"),
                            None => format!("Model for {slug} cleared"),
                        };
                        self.state.status_message = Some(msg);
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Failed to set model: {e}"),
                        };
                    }
                }
                let _ = worktree_id;
            }
            InputAction::SetRepoModel { repo_id, slug } => {
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                let mgr = RepoManager::new(&self.conn, &self.config);
                match mgr.set_model(&slug, model.as_deref()) {
                    Ok(()) => {
                        let msg = match &model {
                            Some(m) => format!("Model for {slug} set to: {m}"),
                            None => format!("Model for {slug} cleared"),
                        };
                        self.state.status_message = Some(msg);
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Failed to set model: {e}"),
                        };
                    }
                }
                let _ = repo_id;
            }
            InputAction::FeedbackResponse { feedback_id } => {
                if value.is_empty() {
                    return;
                }
                let mgr = AgentManager::new(&self.conn);
                match mgr.submit_feedback(&feedback_id, &value) {
                    Ok(_) => {
                        self.state.status_message =
                            Some("Feedback submitted — agent resumed".to_string());
                        self.state.data.pending_feedback = None;
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Failed to submit feedback: {e}"),
                        };
                    }
                }
            }
        }
    }

    /// Helper: submit a value + action directly (used when clearing model via Backspace).
    fn handle_input_submit_with_value(&mut self, value: String, on_submit: InputAction) {
        // Reuse the same match arm logic from handle_input_submit
        match on_submit {
            InputAction::SetWorktreeModel {
                worktree_id,
                repo_slug,
                slug,
            } => {
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                let mgr = WorktreeManager::new(&self.conn, &self.config);
                match mgr.set_model(&repo_slug, &slug, model.as_deref()) {
                    Ok(()) => {
                        let msg = match &model {
                            Some(m) => format!("Model for {slug} set to: {m}"),
                            None => format!("Model for {slug} cleared"),
                        };
                        self.state.status_message = Some(msg);
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Failed to set model: {e}"),
                        };
                    }
                }
                let _ = worktree_id;
            }
            InputAction::SetRepoModel { repo_id, slug } => {
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                let mgr = RepoManager::new(&self.conn, &self.config);
                match mgr.set_model(&slug, model.as_deref()) {
                    Ok(()) => {
                        let msg = match &model {
                            Some(m) => format!("Model for {slug} set to: {m}"),
                            None => format!("Model for {slug} cleared"),
                        };
                        self.state.status_message = Some(msg);
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Failed to set model: {e}"),
                        };
                    }
                }
                let _ = repo_id;
            }
            InputAction::AgentModelOverride {
                prompt,
                worktree_id,
                worktree_path,
                worktree_slug,
                resume_session_id,
                resolved_default,
            } => {
                let model = if value.trim().is_empty() {
                    resolved_default
                } else {
                    Some(value)
                };
                self.start_agent_tmux(
                    prompt,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    resume_session_id,
                    model,
                );
            }
            _ => {}
        }
    }

    fn handle_create(&mut self) {
        // Try to detect ticket context based on current view and focus
        let ticket_context = match self.state.view {
            View::Dashboard if self.state.dashboard_focus == DashboardFocus::Tickets => self
                .state
                .filtered_tickets
                .get(self.state.ticket_index)
                .cloned(),
            View::RepoDetail if self.state.repo_detail_focus == RepoDetailFocus::Tickets => self
                .state
                .filtered_detail_tickets
                .get(self.state.detail_ticket_index)
                .cloned(),
            View::Tickets => self
                .state
                .filtered_tickets
                .get(self.state.ticket_index)
                .cloned(),
            _ => None,
        };

        if let Some(ticket) = ticket_context {
            // Ticket-aware path: derive repo and name from the ticket
            let repo_slug = self.state.data.repo_slug_map.get(&ticket.repo_id).cloned();
            if let Some(slug) = repo_slug {
                let suggested = derive_worktree_slug(&ticket.source_id, &ticket.title);
                self.state.modal = Modal::Input {
                    title: "Create Worktree".to_string(),
                    prompt: format!("Worktree for #{} ({}):", ticket.source_id, slug),
                    value: suggested,
                    on_submit: InputAction::CreateWorktree {
                        repo_slug: slug,
                        ticket_id: Some(ticket.id.clone()),
                    },
                };
            } else {
                self.state.status_message = Some("Repo not found for ticket".to_string());
            }
            return;
        }

        // Fallback: repo-only path (no ticket context)
        match self.state.view {
            View::Dashboard | View::RepoDetail => {
                let repo_slug = self
                    .state
                    .selected_repo_id
                    .as_ref()
                    .and_then(|id| self.state.data.repo_slug_map.get(id))
                    .cloned()
                    .or_else(|| self.state.selected_repo().map(|r| r.slug.clone()));

                if let Some(slug) = repo_slug {
                    self.state.modal = Modal::Input {
                        title: "Create Worktree".to_string(),
                        prompt: format!("Worktree name for {slug} (e.g., smart-playlists):"),
                        value: String::new(),
                        on_submit: InputAction::CreateWorktree {
                            repo_slug: slug,
                            ticket_id: None,
                        },
                    };
                } else if self.state.view == View::Dashboard
                    && self.state.dashboard_focus == DashboardFocus::Repos
                {
                    // No repo selected on repos panel — open add repo form instead
                    self.handle_add_repo();
                } else {
                    self.state.status_message = Some("Select a repo first".to_string());
                }
            }
            _ => {}
        }
    }

    fn handle_add_repo(&mut self) {
        if self.state.view != View::Dashboard {
            return;
        }
        self.state.modal = Modal::Form {
            title: "Add Repository".to_string(),
            fields: vec![
                FormField {
                    label: "Remote URL".to_string(),
                    value: String::new(),
                    placeholder: "https://github.com/org/repo.git".to_string(),
                    manually_edited: true,
                    required: true,
                },
                FormField {
                    label: "Slug".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from URL".to_string(),
                    manually_edited: false,
                    required: true,
                },
                FormField {
                    label: "Local Path".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from slug".to_string(),
                    manually_edited: false,
                    required: false,
                },
            ],
            active_field: 0,
            on_submit: FormAction::AddRepo,
        };
    }

    fn handle_form_char(&mut self, c: char) {
        let config = &self.config;
        if let Modal::Form {
            ref mut fields,
            active_field,
            ref on_submit,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                field.value.push(c);
                field.manually_edited = true;
            }
            // Auto-derive dependent fields
            match on_submit {
                FormAction::AddRepo => {
                    Self::auto_derive_add_repo_fields(fields, active_field, config)
                }
                FormAction::AddIssueSource { .. } if active_field == 0 => {
                    Self::sync_issue_source_form_fields(fields);
                }
                FormAction::AddWorkTarget | FormAction::AddIssueSource { .. } => {}
            }
        }
    }

    fn handle_form_backspace(&mut self) {
        let config = &self.config;
        if let Modal::Form {
            ref mut fields,
            active_field,
            ref on_submit,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                field.value.pop();
                // If field emptied and it's a derived field, reset to auto-derive
                if field.value.is_empty() && active_field > 0 {
                    field.manually_edited = false;
                }
            }
            match on_submit {
                FormAction::AddRepo => {
                    Self::auto_derive_add_repo_fields(fields, active_field, config)
                }
                FormAction::AddIssueSource { .. } if active_field == 0 => {
                    Self::sync_issue_source_form_fields(fields);
                }
                FormAction::AddWorkTarget | FormAction::AddIssueSource { .. } => {}
            }
        }
    }

    fn handle_form_next_field(&mut self) {
        if let Modal::Form {
            ref fields,
            ref mut active_field,
            ..
        } = self.state.modal
        {
            *active_field = (*active_field + 1) % fields.len();
        }
    }

    fn handle_form_prev_field(&mut self) {
        if let Modal::Form {
            ref fields,
            ref mut active_field,
            ..
        } = self.state.modal
        {
            if *active_field == 0 {
                *active_field = fields.len() - 1;
            } else {
                *active_field -= 1;
            }
        }
    }

    fn auto_derive_add_repo_fields(
        fields: &mut [FormField],
        changed_field: usize,
        config: &Config,
    ) {
        // When URL (field 0) changes, auto-update Slug (field 1) if not manually edited
        if changed_field == 0 && fields.len() > 1 && !fields[1].manually_edited {
            let url = &fields[0].value;
            fields[1].value = if url.is_empty() {
                String::new()
            } else {
                derive_slug_from_url(url)
            };
        }
        // When Slug (field 1) changes (or was just auto-derived), auto-update Local Path (field 2)
        if changed_field <= 1 && fields.len() > 2 && !fields[2].manually_edited {
            let slug = &fields[1].value;
            fields[2].value = if slug.is_empty() {
                String::new()
            } else {
                derive_local_path(config, slug)
            };
        }
    }

    /// Dynamically add or remove Jira-specific fields based on the current
    /// value of the Type field (field 0).  Called whenever the type field changes.
    fn sync_issue_source_form_fields(fields: &mut Vec<FormField>) {
        let type_val = fields
            .first()
            .map(|f| f.value.trim().to_lowercase())
            .unwrap_or_default();

        let is_jira = type_val == "jira" || type_val == "j";

        if is_jira && fields.len() == 1 {
            // Add JQL and URL fields
            fields.push(FormField {
                label: "JQL".to_string(),
                value: String::new(),
                placeholder: "e.g. project = PROJ AND status != Done".to_string(),
                manually_edited: false,
                required: true,
            });
            fields.push(FormField {
                label: "Jira URL".to_string(),
                value: String::new(),
                placeholder: "e.g. https://mycompany.atlassian.net".to_string(),
                manually_edited: false,
                required: true,
            });
        } else if !is_jira && fields.len() > 1 {
            // Remove extra fields when switching away from Jira
            fields.truncate(1);
        }
    }

    fn handle_form_submit(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::Form {
            fields, on_submit, ..
        } = modal
        {
            match on_submit {
                FormAction::AddRepo => self.submit_add_repo(fields),
                FormAction::AddWorkTarget => self.submit_add_work_target(fields),
                FormAction::AddIssueSource {
                    repo_id,
                    repo_slug,
                    remote_url,
                } => self.submit_add_issue_source(fields, &repo_id, &repo_slug, &remote_url),
            }
        }
    }

    fn submit_add_repo(&mut self, fields: Vec<FormField>) {
        let url = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let slug = fields
            .get(1)
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let local_path = fields
            .get(2)
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();

        if url.is_empty() || slug.is_empty() {
            self.state.modal = Modal::Error {
                message: "Remote URL and Slug are required".to_string(),
            };
            return;
        }

        let local = if local_path.is_empty() {
            derive_local_path(&self.config, &slug)
        } else {
            local_path
        };

        let mgr = RepoManager::new(&self.conn, &self.config);
        match mgr.add(&slug, &local, &url, None) {
            Ok(repo) => {
                self.state.status_message = Some(format!("Added repo: {}", repo.slug));
                self.refresh_data();
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Add repo failed: {e}"),
                };
            }
        }
    }

    fn submit_add_work_target(&mut self, fields: Vec<FormField>) {
        let name = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let command = fields
            .get(1)
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let target_type = fields
            .get(2)
            .map(|f| f.value.trim().to_string())
            .unwrap_or_else(|| "editor".to_string());

        if name.is_empty() || command.is_empty() {
            self.state.modal = Modal::Error {
                message: "Name and Command are required".to_string(),
            };
            return;
        }

        let target = WorkTarget {
            name: name.clone(),
            command,
            target_type,
        };
        self.config.general.work_targets.push(target);

        match save_config(&self.config) {
            Ok(()) => {
                let new_index = self.config.general.work_targets.len().saturating_sub(1);
                self.state.modal = Modal::WorkTargetManager {
                    targets: self.config.general.work_targets.clone(),
                    selected: new_index,
                };
                self.state.status_message = Some(format!("Added work target: {name}"));
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to save config: {e}"),
                };
            }
        }
    }

    fn submit_add_issue_source(
        &mut self,
        fields: Vec<FormField>,
        repo_id: &str,
        repo_slug: &str,
        remote_url: &str,
    ) {
        let source_type = fields
            .first()
            .map(|f| f.value.trim().to_lowercase())
            .unwrap_or_default();

        let (config_json, type_str) = match source_type.as_str() {
            "github" | "g" | "gh" => {
                // Auto-infer from remote URL
                match github::parse_github_remote(remote_url) {
                    Some((owner, repo)) => {
                        let json = serde_json::json!({"owner": owner, "repo": repo}).to_string();
                        (json, "github")
                    }
                    None => {
                        self.state.modal = Modal::Error {
                            message: "Cannot infer GitHub owner/repo from remote URL".to_string(),
                        };
                        return;
                    }
                }
            }
            "jira" | "j" => {
                let jql = fields
                    .get(1)
                    .map(|f| f.value.trim().to_string())
                    .unwrap_or_default();
                let url = fields
                    .get(2)
                    .map(|f| f.value.trim().to_string())
                    .unwrap_or_default();
                if jql.is_empty() || url.is_empty() {
                    self.state.modal = Modal::Error {
                        message: "JQL and URL are required for Jira sources".to_string(),
                    };
                    return;
                }
                let json = serde_json::json!({"jql": jql, "url": url}).to_string();
                (json, "jira")
            }
            other => {
                let msg = if other.is_empty() {
                    "Type is required — enter 'github' or 'jira'".to_string()
                } else {
                    format!("Unknown source type '{other}' — use 'github' or 'jira'")
                };
                self.state.modal = Modal::Error { message: msg };
                return;
            }
        };

        let mgr = IssueSourceManager::new(&self.conn);
        match mgr.add(repo_id, type_str, &config_json, repo_slug) {
            Ok(_) => {
                let sources = mgr.list(repo_id).unwrap_or_default();
                self.state.modal = Modal::IssueSourceManager {
                    repo_id: repo_id.to_string(),
                    repo_slug: repo_slug.to_string(),
                    remote_url: remote_url.to_string(),
                    sources,
                    selected: 0,
                };
                self.state.status_message =
                    Some(format!("Added {type_str} source for {repo_slug}"));
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to add source: {e}"),
                };
            }
        }
    }

    fn handle_manage_issue_sources(&mut self) {
        // Only available from RepoDetail view
        if self.state.view != View::RepoDetail {
            return;
        }
        let Some(ref repo_id) = self.state.selected_repo_id.clone() else {
            return;
        };
        let Some(repo) = self.state.data.repos.iter().find(|r| r.id == *repo_id) else {
            return;
        };

        let mgr = IssueSourceManager::new(&self.conn);
        let sources = mgr.list(repo_id).unwrap_or_default();

        self.state.modal = Modal::IssueSourceManager {
            repo_id: repo.id.clone(),
            repo_slug: repo.slug.clone(),
            remote_url: repo.remote_url.clone(),
            sources,
            selected: 0,
        };
    }

    fn handle_issue_source_add(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::IssueSourceManager {
            repo_id,
            repo_slug,
            remote_url,
            sources,
            ..
        } = modal
        {
            let has_github = sources.iter().any(|s| s.source_type == "github");
            let has_jira = sources.iter().any(|s| s.source_type == "jira");

            if has_github && has_jira {
                self.state.modal = Modal::IssueSourceManager {
                    repo_id,
                    repo_slug,
                    remote_url,
                    sources,
                    selected: 0,
                };
                self.state.status_message =
                    Some("Both source types already configured".to_string());
                return;
            }

            let default_type = if has_github {
                "jira".to_string()
            } else if has_jira {
                "github".to_string()
            } else {
                String::new()
            };

            let mut fields = vec![FormField {
                label: "Type".to_string(),
                value: default_type,
                placeholder: "github or jira (Tab to next field)".to_string(),
                manually_edited: false,
                required: true,
            }];

            // If type is pre-filled to jira, include the Jira fields up front
            Self::sync_issue_source_form_fields(&mut fields);

            self.state.modal = Modal::Form {
                title: "Add Issue Source".to_string(),
                fields,
                active_field: 0,
                on_submit: FormAction::AddIssueSource {
                    repo_id,
                    repo_slug,
                    remote_url,
                },
            };
        }
    }

    fn handle_issue_source_delete(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::IssueSourceManager {
            repo_id,
            repo_slug,
            remote_url,
            sources,
            selected,
        } = modal
        {
            if sources.is_empty() {
                self.state.modal = Modal::IssueSourceManager {
                    repo_id,
                    repo_slug,
                    remote_url,
                    sources,
                    selected,
                };
                return;
            }

            let source = &sources[selected];
            self.state.modal = Modal::Confirm {
                title: "Remove Issue Source".to_string(),
                message: format!("Remove {} source for {}?", source.source_type, repo_slug),
                on_confirm: ConfirmAction::DeleteIssueSource {
                    source_id: source.id.clone(),
                    repo_id,
                    repo_slug,
                    remote_url,
                },
            };
        }
    }

    fn show_delete_worktree_modal(
        &mut self,
        repo_slug: &str,
        wt_slug: &str,
        issue_closed: bool,
        pr_merged: bool,
        has_ticket: bool,
    ) {
        let on_confirm = ConfirmAction::DeleteWorktree {
            repo_slug: repo_slug.to_string(),
            wt_slug: wt_slug.to_string(),
        };

        if issue_closed && pr_merged {
            // Work is done — simple confirm
            self.state.modal = Modal::Confirm {
                title: "Delete Worktree".to_string(),
                message: format!(
                    "Delete worktree {}/{}? Issue is closed and PR is merged.",
                    repo_slug, wt_slug
                ),
                on_confirm,
            };
        } else {
            // Work may be in progress — require typing the slug
            let reason = if !has_ticket {
                "This worktree has no linked issue."
            } else if !issue_closed && !pr_merged {
                "This worktree has an open issue and unmerged code."
            } else if !issue_closed {
                "This worktree has an open issue."
            } else {
                "This worktree has unmerged code."
            };
            self.state.modal = Modal::ConfirmByName {
                title: "Delete Worktree".to_string(),
                message: format!("{reason} This removes the git worktree and branch."),
                expected: wt_slug.to_string(),
                value: String::new(),
                on_confirm,
            };
        }
    }

    fn handle_delete(&mut self) {
        match self.state.view {
            View::WorktreeDetail => {
                if let Some(ref wt_id) = self.state.selected_worktree_id {
                    if let Some(wt) = self.state.data.worktrees.iter().find(|w| &w.id == wt_id) {
                        if !wt.is_active() {
                            self.state.status_message =
                                Some("Cannot modify archived worktree".to_string());
                            return;
                        }
                        let repo_slug = self
                            .state
                            .data
                            .repo_slug_map
                            .get(&wt.repo_id)
                            .cloned()
                            .unwrap_or_else(|| "?".to_string());

                        // Check if work is completed (issue closed + PR merged)
                        let issue_closed = wt
                            .ticket_id
                            .as_ref()
                            .and_then(|tid| self.state.data.ticket_map.get(tid))
                            .is_some_and(|t| t.state == "closed");
                        let has_ticket = wt.ticket_id.is_some();

                        if issue_closed {
                            // Issue is closed — check PR status in background
                            let remote_url = self
                                .state
                                .data
                                .repos
                                .iter()
                                .find(|r| r.id == wt.repo_id)
                                .map(|r| r.remote_url.clone())
                                .unwrap_or_default();
                            let branch = wt.branch.clone();
                            let slug = wt.slug.clone();
                            let rs = repo_slug.clone();
                            if let Some(ref tx) = self.bg_tx {
                                let tx = tx.clone();
                                std::thread::spawn(move || {
                                    let pr_merged =
                                        conductor_core::github::has_merged_pr(&remote_url, &branch);
                                    let _ = tx.send(Action::DeleteWorktreeReady {
                                        repo_slug: rs,
                                        wt_slug: slug,
                                        issue_closed: true,
                                        pr_merged,
                                        has_ticket: true,
                                    });
                                });
                                self.state.status_message = Some("Checking PR status…".to_string());
                            }
                        } else {
                            // Issue is open or no ticket — no network call needed
                            self.show_delete_worktree_modal(
                                &repo_slug,
                                &wt.slug.clone(),
                                issue_closed,
                                false,
                                has_ticket,
                            );
                        }
                    }
                }
            }
            View::RepoDetail => {
                if let Some(ref repo_id) = self.state.selected_repo_id.clone() {
                    if let Some(repo) = self.state.data.repos.iter().find(|r| &r.id == repo_id) {
                        let wt_count = self
                            .state
                            .data
                            .repo_worktree_count
                            .get(repo_id)
                            .copied()
                            .unwrap_or(0);
                        let warning = if wt_count > 0 {
                            format!(
                                " This repo has {wt_count} worktree{}.",
                                if wt_count == 1 { "" } else { "s" }
                            )
                        } else {
                            String::new()
                        };
                        self.state.modal = Modal::ConfirmByName {
                            title: "Remove Repository".to_string(),
                            message: format!(
                                "This will permanently delete the repo and all associated worktrees, agent runs, and tickets.{}",
                                warning
                            ),
                            expected: repo.slug.clone(),
                            value: String::new(),
                            on_confirm: ConfirmAction::RemoveRepo {
                                repo_slug: repo.slug.clone(),
                            },
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_push(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        if let Some(wt) = wt {
            let repo_slug = match self.state.data.repo_slug_map.get(&wt.repo_id) {
                Some(s) => s.clone(),
                None => {
                    self.state.status_message = Some("Cannot find repo for worktree".to_string());
                    return;
                }
            };
            self.state.status_message = Some(format!("Pushing {}...", wt.slug));
            let mgr = WorktreeManager::new(&self.conn, &self.config);
            match mgr.push(&repo_slug, &wt.slug) {
                Ok(msg) => {
                    self.state.status_message = Some(msg);
                }
                Err(e) => {
                    self.state.modal = Modal::Error {
                        message: format!("Push failed: {e}"),
                    };
                }
            }
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    fn handle_create_pr(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        if let Some(wt) = wt {
            let repo_slug = match self.state.data.repo_slug_map.get(&wt.repo_id) {
                Some(s) => s.clone(),
                None => {
                    self.state.status_message = Some("Cannot find repo for worktree".to_string());
                    return;
                }
            };
            self.state.status_message = Some(format!("Creating PR for {}...", wt.slug));
            let mgr = WorktreeManager::new(&self.conn, &self.config);
            match mgr.create_pr(&repo_slug, &wt.slug, false) {
                Ok(url) => {
                    self.state.status_message = Some(format!("PR created: {url}"));
                }
                Err(e) => {
                    self.state.modal = Modal::Error {
                        message: format!("PR creation failed: {e}"),
                    };
                }
            }
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    fn handle_sync_tickets(&mut self) {
        if self.state.ticket_sync_in_progress {
            self.state.status_message = Some("Sync already in progress...".to_string());
            return;
        }
        let Some(ref tx) = self.bg_tx else {
            self.state.status_message = Some("Background sender not ready".to_string());
            return;
        };
        self.state.ticket_sync_in_progress = true;
        self.state.status_message = Some("Syncing tickets...".to_string());
        background::spawn_ticket_sync_once(tx.clone());
    }

    fn handle_link_ticket(&mut self) {
        if let Some(ref wt_id) = self.state.selected_worktree_id.clone() {
            if let Some(wt) = self.state.data.worktrees.iter().find(|w| &w.id == wt_id) {
                if !wt.is_active() {
                    self.state.status_message = Some("Cannot modify archived worktree".to_string());
                    return;
                }
                if wt.ticket_id.is_some() {
                    self.state.status_message =
                        Some("Worktree already has a linked ticket".to_string());
                    return;
                }
            }
            self.state.modal = Modal::Input {
                title: "Link Ticket".to_string(),
                prompt: "Enter ticket number (e.g., 42):".to_string(),
                value: String::new(),
                on_submit: InputAction::LinkTicket {
                    worktree_id: wt_id.clone(),
                },
            };
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    fn handle_start_work(&mut self) {
        // Resolve the selected worktree from the current view context
        let wt = self.resolve_selected_worktree();

        let Some(wt) = wt else {
            self.state.status_message = Some("Select a worktree first".to_string());
            return;
        };

        if !wt.is_active() {
            self.state.status_message = Some("Cannot modify archived worktree".to_string());
            return;
        }

        let targets = self.config.general.work_targets.clone();
        if targets.is_empty() {
            self.state.status_message =
                Some("No work targets configured. Press W to manage.".to_string());
            return;
        }

        if targets.len() == 1 {
            self.open_work_target(&targets[0], &wt);
        } else {
            self.state.modal = Modal::WorkTargetPicker {
                targets,
                selected: 0,
            };
        }
    }

    fn handle_select_work_target(&mut self, index: usize) {
        let (targets, selected) = if let Modal::WorkTargetPicker {
            ref targets,
            selected,
        } = self.state.modal
        {
            (targets.clone(), selected)
        } else {
            return;
        };

        // usize::MAX is sentinel for "use current selected"
        let actual_index = if index == usize::MAX { selected } else { index };
        if actual_index >= targets.len() {
            return;
        }

        self.state.modal = Modal::None;

        let wt = self.resolve_selected_worktree();
        let Some(wt) = wt else {
            self.state.status_message = Some("Select a worktree first".to_string());
            return;
        };

        self.open_work_target(&targets[actual_index], &wt);
    }

    fn resolve_selected_worktree(&self) -> Option<conductor_core::worktree::Worktree> {
        self.state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned()
            .or_else(|| match self.state.view {
                View::Dashboard if self.state.dashboard_focus == DashboardFocus::Worktrees => self
                    .state
                    .data
                    .worktrees
                    .get(self.state.worktree_index)
                    .cloned(),
                View::RepoDetail => self
                    .state
                    .detail_worktrees
                    .get(self.state.detail_wt_index)
                    .cloned(),
                _ => None,
            })
    }

    fn open_work_target(&mut self, target: &WorkTarget, wt: &conductor_core::worktree::Worktree) {
        let result = if target.command == "terminal" {
            // Legacy special case: open Terminal.app and run claude
            Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        "tell application \"Terminal\" to do script \"cd '{}' && claude\"",
                        wt.path
                    ),
                ])
                .spawn()
        } else {
            // Run through shell so multi-word commands like "open -a iTerm" work
            let shell_cmd = format!("{} '{}'", target.command, wt.path);
            Command::new("sh").args(["-c", &shell_cmd]).spawn()
        };

        match result {
            Ok(_) => {
                self.state.status_message = Some(format!("Opened {} at {}", target.name, wt.slug));
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to open {}: {e}", target.name),
                };
            }
        }
    }

    fn handle_manage_work_targets(&mut self) {
        self.state.modal = Modal::WorkTargetManager {
            targets: self.config.general.work_targets.clone(),
            selected: 0,
        };
    }

    fn handle_work_target_move_up(&mut self) {
        if let Modal::WorkTargetManager {
            ref mut targets,
            ref mut selected,
        } = self.state.modal
        {
            if *selected > 0 {
                targets.swap(*selected, *selected - 1);
                *selected -= 1;
                self.config.general.work_targets = targets.clone();
                if let Err(e) = save_config(&self.config) {
                    self.state.status_message = Some(format!("Failed to save config: {e}"));
                }
            }
        }
    }

    fn handle_work_target_move_down(&mut self) {
        if let Modal::WorkTargetManager {
            ref mut targets,
            ref mut selected,
        } = self.state.modal
        {
            if *selected + 1 < targets.len() {
                targets.swap(*selected, *selected + 1);
                *selected += 1;
                self.config.general.work_targets = targets.clone();
                if let Err(e) = save_config(&self.config) {
                    self.state.status_message = Some(format!("Failed to save config: {e}"));
                }
            }
        }
    }

    fn handle_work_target_add(&mut self) {
        if matches!(self.state.modal, Modal::WorkTargetManager { .. }) {
            self.state.modal = Modal::Form {
                title: "Add Work Target".to_string(),
                fields: vec![
                    FormField {
                        label: "Name".to_string(),
                        value: String::new(),
                        placeholder: "e.g., VS Code, Cursor, Terminal".to_string(),
                        manually_edited: true,
                        required: true,
                    },
                    FormField {
                        label: "Command".to_string(),
                        value: String::new(),
                        placeholder: "e.g., code, cursor, terminal".to_string(),
                        manually_edited: true,
                        required: true,
                    },
                    FormField {
                        label: "Type".to_string(),
                        value: "editor".to_string(),
                        placeholder: "editor or terminal".to_string(),
                        manually_edited: true,
                        required: true,
                    },
                ],
                active_field: 0,
                on_submit: FormAction::AddWorkTarget,
            };
        }
    }

    fn handle_work_target_delete(&mut self) {
        if let Modal::WorkTargetManager {
            ref targets,
            selected,
        } = self.state.modal
        {
            if targets.is_empty() {
                return;
            }
            if targets.len() == 1 {
                self.state.status_message = Some("Cannot delete the last work target".to_string());
                return;
            }
            let target_name = targets[selected].name.clone();
            self.state.modal = Modal::Confirm {
                title: "Delete Work Target".to_string(),
                message: format!("Delete work target '{}'?", target_name),
                on_confirm: ConfirmAction::DeleteWorkTarget { index: selected },
            };
        }
    }

    // ── Model configuration ────────────────────────────────────────────

    fn handle_set_model(&mut self) {
        // Helper to compute effective default and source for a worktree context
        let resolve_wt_effective =
            |wt: &conductor_core::worktree::Worktree,
             config: &conductor_core::config::Config,
             repos: &[conductor_core::repo::Repo]| {
                let repo_model = repos
                    .iter()
                    .find(|r| r.id == wt.repo_id)
                    .and_then(|r| r.model.clone());
                if let Some(ref m) = wt.model {
                    (Some(m.clone()), "worktree".to_string())
                } else if let Some(ref m) = repo_model {
                    (Some(m.clone()), "repo".to_string())
                } else if let Some(ref m) = config.general.model {
                    (Some(m.clone()), "global config".to_string())
                } else {
                    (None, "not set".to_string())
                }
            };

        // Helper to find the initial selected index matching current model
        let initial_selected = |current: &Option<String>| -> usize {
            match current {
                Some(m) => conductor_core::models::KNOWN_MODELS
                    .iter()
                    .position(|km| km.id == m.as_str() || km.alias == m.as_str())
                    .unwrap_or(conductor_core::models::KNOWN_MODELS.len()),
                None => {
                    // Default to sonnet (index 1)
                    1
                }
            }
        };

        match self.state.view {
            View::Dashboard => {
                use crate::state::DashboardFocus;
                match self.state.dashboard_focus {
                    DashboardFocus::Worktrees => {
                        let Some(wt) = self
                            .state
                            .data
                            .worktrees
                            .get(self.state.worktree_index)
                            .cloned()
                        else {
                            return;
                        };
                        let repo_slug = self
                            .state
                            .data
                            .repo_slug_map
                            .get(&wt.repo_id)
                            .cloned()
                            .unwrap_or_default();
                        let (effective, source) =
                            resolve_wt_effective(&wt, &self.config, &self.state.data.repos);
                        let selected = initial_selected(&wt.model);
                        self.state.modal = Modal::ModelPicker {
                            context_label: format!("worktree: {}", wt.slug),
                            effective_default: effective,
                            effective_source: source,
                            selected,
                            custom_input: String::new(),
                            custom_active: false,
                            suggested: None,
                            on_submit: InputAction::SetWorktreeModel {
                                worktree_id: wt.id.clone(),
                                repo_slug,
                                slug: wt.slug.clone(),
                            },
                        };
                    }
                    DashboardFocus::Repos => {
                        let Some(repo) = self.state.data.repos.get(self.state.repo_index).cloned()
                        else {
                            return;
                        };
                        let (effective, source) = if let Some(ref m) = repo.model {
                            (Some(m.clone()), "repo".to_string())
                        } else if let Some(ref m) = self.config.general.model {
                            (Some(m.clone()), "global config".to_string())
                        } else {
                            (None, "not set".to_string())
                        };
                        let selected = initial_selected(&repo.model);
                        self.state.modal = Modal::ModelPicker {
                            context_label: format!("repo: {}", repo.slug),
                            effective_default: effective,
                            effective_source: source,
                            selected,
                            custom_input: String::new(),
                            custom_active: false,
                            suggested: None,
                            on_submit: InputAction::SetRepoModel {
                                repo_id: repo.id.clone(),
                                slug: repo.slug.clone(),
                            },
                        };
                    }
                    DashboardFocus::Tickets => {}
                }
            }
            View::WorktreeDetail => {
                let Some(wt) = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                    .cloned()
                else {
                    return;
                };
                let repo_slug = self
                    .state
                    .data
                    .repo_slug_map
                    .get(&wt.repo_id)
                    .cloned()
                    .unwrap_or_default();
                let (effective, source) =
                    resolve_wt_effective(&wt, &self.config, &self.state.data.repos);
                let selected = initial_selected(&wt.model);
                self.state.modal = Modal::ModelPicker {
                    context_label: format!("worktree: {}", wt.slug),
                    effective_default: effective,
                    effective_source: source,
                    selected,
                    custom_input: String::new(),
                    custom_active: false,
                    suggested: None,
                    on_submit: InputAction::SetWorktreeModel {
                        worktree_id: wt.id.clone(),
                        repo_slug,
                        slug: wt.slug.clone(),
                    },
                };
            }
            View::RepoDetail => {
                let Some(repo) = self
                    .state
                    .selected_repo_id
                    .as_ref()
                    .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
                    .cloned()
                else {
                    return;
                };
                let (effective, source) = if let Some(ref m) = repo.model {
                    (Some(m.clone()), "repo".to_string())
                } else if let Some(ref m) = self.config.general.model {
                    (Some(m.clone()), "global config".to_string())
                } else {
                    (None, "not set".to_string())
                };
                let selected = initial_selected(&repo.model);
                self.state.modal = Modal::ModelPicker {
                    context_label: format!("repo: {}", repo.slug),
                    effective_default: effective,
                    effective_source: source,
                    selected,
                    custom_input: String::new(),
                    custom_active: false,
                    suggested: None,
                    on_submit: InputAction::SetRepoModel {
                        repo_id: repo.id.clone(),
                        slug: repo.slug.clone(),
                    },
                };
            }
            _ => {}
        }
    }

    fn handle_toggle_agent_issues(&mut self) {
        let Some(repo) = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
            .cloned()
        else {
            self.state.status_message = Some("No repo selected".to_string());
            return;
        };
        let new_value = !repo.allow_agent_issue_creation;
        let mgr = conductor_core::repo::RepoManager::new(&self.conn, &self.config);
        match mgr.set_allow_agent_issue_creation(&repo.id, new_value) {
            Ok(()) => {
                let label = if new_value { "enabled" } else { "disabled" };
                self.state.status_message =
                    Some(format!("Agent issue creation {} for {}", label, repo.slug));
                self.refresh_data();
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to toggle agent issues: {e}"));
            }
        }
    }

    // ── Agent handlers (tmux-based) ────────────────────────────────────

    fn selected_worktree_run(&self) -> Option<&AgentRun> {
        self.state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.latest_agent_runs.get(id))
    }

    fn refresh_pending_feedback(&mut self) {
        self.state.data.pending_feedback =
            self.state.selected_worktree_id.as_ref().and_then(|wt_id| {
                AgentManager::new(&self.conn)
                    .pending_feedback_for_worktree(wt_id)
                    .ok()
                    .flatten()
            });
    }

    /// Returns `true` (and sets a status message) if the worktree already has
    /// an active agent, meaning the caller should abort.
    fn agent_busy_guard(&mut self, worktree_id: &str) -> bool {
        use conductor_core::agent::AgentRunStatus;
        let status = self
            .state
            .data
            .latest_agent_runs
            .get(worktree_id)
            .map(|run| &run.status);
        match status {
            Some(AgentRunStatus::Running) => {
                self.state.status_message =
                    Some("Agent already running — press x to stop".to_string());
                true
            }
            Some(AgentRunStatus::WaitingForFeedback) => {
                self.state.status_message =
                    Some("Agent waiting for feedback — press f to respond".to_string());
                true
            }
            _ => false,
        }
    }

    fn handle_launch_agent(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        let Some(wt) = wt else {
            self.state.status_message = Some("Select a worktree first".to_string());
            return;
        };

        if self.agent_busy_guard(&wt.id) {
            return;
        }

        // Check for existing session to resume (from DB)
        let latest_run = self.state.data.latest_agent_runs.get(&wt.id);

        // Determine resume state: either a normal resume (completed run with session_id)
        // or a needs_resume (failed/cancelled run with incomplete plan steps)
        let (resume_session_id, needs_resume) = match latest_run {
            Some(run) if run.needs_resume() => (run.claude_session_id.clone(), true),
            Some(run) => (run.claude_session_id.clone(), false),
            None => (None, false),
        };

        let has_prior_runs = AgentManager::new(&self.conn)
            .has_runs_for_worktree(&wt.id)
            .unwrap_or(false);

        let (title, prefill) = if needs_resume {
            // Auto-build resume prompt from incomplete plan steps
            let incomplete_count = latest_run
                .map(|r| r.incomplete_plan_steps().len())
                .unwrap_or(0);
            let resume_prompt = latest_run
                .map(|r| r.build_resume_prompt())
                .unwrap_or_default();
            (
                format!("Claude Agent (Resume — {incomplete_count} steps remaining)"),
                resume_prompt,
            )
        } else if resume_session_id.is_some() {
            ("Claude Agent (Resume)".to_string(), String::new())
        } else if has_prior_runs {
            // Skip pre-fill when worktree has prior agent activity
            ("Claude Agent".to_string(), String::new())
        } else {
            // Pre-fill prompt with rich ticket context if available
            let prefill = wt
                .ticket_id
                .as_ref()
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .map(build_agent_prompt)
                .unwrap_or_default();
            ("Claude Agent".to_string(), prefill)
        };

        self.open_agent_prompt_modal(
            title,
            prefill,
            wt.id.clone(),
            wt.path.clone(),
            wt.slug.clone(),
            resume_session_id,
        );
    }

    fn handle_stop_agent(&mut self) {
        let run = self.selected_worktree_run();

        let Some(run) = run else {
            return;
        };

        if !run.is_active() {
            return;
        }

        let run_id = run.id.clone();
        let tmux_window = run.tmux_window.clone();

        let mgr = AgentManager::new(&self.conn);

        // Best-effort: capture tmux scrollback before killing
        if let Some(ref window) = tmux_window {
            mgr.capture_agent_log(&run_id, window);
        }

        // Kill the tmux window
        if let Some(ref window) = tmux_window {
            let _ = Command::new("tmux")
                .args(["kill-window", "-t", &format!(":{window}")])
                .output();
        }

        // Update DB record to cancelled
        let _ = mgr.update_run_cancelled(&run_id);

        self.state.status_message = Some("Agent cancelled".to_string());
        self.refresh_data();
    }

    fn handle_attach_agent(&mut self) {
        let run = self.selected_worktree_run();

        let tmux_window = run.and_then(|r| {
            if r.is_active() {
                r.tmux_window.as_deref()
            } else {
                None
            }
        });

        let Some(window) = tmux_window else {
            self.state.status_message = Some("No active agent to attach to".to_string());
            return;
        };

        let mgr = AgentManager::new(&self.conn);
        if let Err(e) = mgr.attach_agent_window(window) {
            self.state.status_message = Some(format!("Failed to attach: {e}"));
        }
    }

    fn require_pending_feedback(&mut self) -> Option<FeedbackRequest> {
        match self.state.data.pending_feedback.clone() {
            Some(fb) => Some(fb),
            None => {
                self.state.status_message = Some("No pending feedback request".to_string());
                None
            }
        }
    }

    fn handle_submit_feedback(&mut self) {
        let Some(fb) = self.require_pending_feedback() else {
            return;
        };

        // Open a text area modal for the user to type their response
        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_placeholder_text("Type your feedback response...");

        self.state.modal = Modal::AgentPrompt {
            title: format!("Agent Feedback: {}", &fb.prompt),
            prompt: fb.prompt.clone(),
            textarea: Box::new(textarea),
            on_submit: InputAction::FeedbackResponse {
                feedback_id: fb.id.clone(),
            },
        };
    }

    fn handle_dismiss_feedback(&mut self) {
        let Some(fb) = self.require_pending_feedback() else {
            return;
        };

        let mgr = AgentManager::new(&self.conn);
        match mgr.dismiss_feedback(&fb.id) {
            Ok(()) => {
                self.state.status_message = Some("Feedback dismissed — agent resumed".to_string());
                self.state.data.pending_feedback = None;
                self.refresh_data();
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to dismiss feedback: {e}"));
            }
        }
    }

    fn handle_orchestrate_agent(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        let Some(wt) = wt else {
            self.state.status_message = Some("Select a worktree first".to_string());
            return;
        };

        if self.agent_busy_guard(&wt.id) {
            return;
        }

        // Pre-fill prompt from linked ticket if available
        let has_prior_runs = AgentManager::new(&self.conn)
            .has_runs_for_worktree(&wt.id)
            .unwrap_or(false);

        let prefill = if has_prior_runs {
            String::new()
        } else {
            wt.ticket_id
                .as_ref()
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .map(build_agent_prompt)
                .unwrap_or_default()
        };

        let lines = if prefill.is_empty() {
            vec![String::new()]
        } else {
            prefill.lines().map(String::from).collect()
        };
        let mut textarea = tui_textarea::TextArea::new(lines);
        textarea.set_cursor_line_style(ratatui::style::Style::default());
        textarea.set_placeholder_text("Type your prompt here...");

        self.state.modal = Modal::AgentPrompt {
            title: "Orchestrate (multi-step)".to_string(),
            prompt: "Enter prompt — plan will be generated, then each step runs as a child agent:"
                .to_string(),
            textarea: Box::new(textarea),
            on_submit: InputAction::OrchestratePrompt {
                worktree_id: wt.id.clone(),
                worktree_path: wt.path.clone(),
                worktree_slug: wt.slug.clone(),
            },
        };
    }

    fn start_orchestrate_tmux(
        &mut self,
        prompt: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
    ) {
        // Resolve model: per-worktree → per-repo → global config
        let wt_model = self
            .state
            .data
            .worktrees
            .iter()
            .find(|w| w.id == worktree_id)
            .and_then(|w| w.model.clone());
        let repo_model = self
            .state
            .data
            .worktrees
            .iter()
            .find(|w| w.id == worktree_id)
            .and_then(|w| self.state.data.repos.iter().find(|r| r.id == w.repo_id))
            .and_then(|r| r.model.clone());
        let model = wt_model
            .or(repo_model)
            .or_else(|| self.config.general.model.clone());

        // Create DB record with tmux window name
        let mgr = AgentManager::new(&self.conn);
        let run = match mgr.create_run(
            &worktree_id,
            &prompt,
            Some(&worktree_slug),
            model.as_deref(),
        ) {
            Ok(run) => run,
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to create agent run: {e}"),
                };
                return;
            }
        };

        // Build the conductor agent orchestrate command
        let mut args = vec![
            "agent".to_string(),
            "orchestrate".to_string(),
            "--run-id".to_string(),
            run.id.clone(),
            "--worktree-path".to_string(),
            worktree_path,
        ];

        if let Some(ref m) = model {
            args.push("--model".to_string());
            args.push(m.clone());
        }

        match conductor_core::agent_runtime::spawn_tmux_window(&args, &worktree_slug) {
            Ok(()) => {
                self.state.status_message = Some(format!(
                    "Orchestrator launched in tmux window: {worktree_slug}"
                ));
                self.refresh_data();
            }
            Err(e) => {
                let _ = mgr.update_run_failed(&run.id, &e);
                self.state.modal = Modal::Error { message: e };
            }
        }
    }

    fn maybe_start_agent_for_worktree(
        &mut self,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
    ) {
        match self.config.general.auto_start_agent {
            AutoStartAgent::Never => {}
            AutoStartAgent::Ask => {
                self.state.modal = Modal::Confirm {
                    title: "Start Agent".to_string(),
                    message: "Start an AI agent for this ticket?".to_string(),
                    on_confirm: ConfirmAction::StartAgentForWorktree {
                        worktree_id,
                        worktree_path,
                        worktree_slug,
                        ticket_id,
                    },
                };
            }
            AutoStartAgent::Always => {
                self.show_agent_prompt_for_ticket(
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                );
            }
        }
    }

    fn show_agent_prompt_for_ticket(
        &mut self,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
    ) {
        let has_prior_runs = AgentManager::new(&self.conn)
            .has_runs_for_worktree(&worktree_id)
            .unwrap_or(false);

        let prefill = if has_prior_runs {
            String::new()
        } else {
            self.state
                .data
                .ticket_map
                .get(&ticket_id)
                .map(build_agent_prompt)
                .unwrap_or_default()
        };

        self.open_agent_prompt_modal(
            "Agent Prompt".to_string(),
            prefill,
            worktree_id,
            worktree_path,
            worktree_slug,
            None,
        );
    }

    /// Shared helper to open the multiline agent prompt modal.
    fn open_agent_prompt_modal(
        &mut self,
        title: String,
        prefill: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
    ) {
        let lines = if prefill.is_empty() {
            vec![String::new()]
        } else {
            prefill.lines().map(String::from).collect()
        };
        let mut textarea = tui_textarea::TextArea::new(lines);
        textarea.set_cursor_line_style(ratatui::style::Style::default());
        textarea.set_placeholder_text("Type your prompt here...");

        self.state.modal = Modal::AgentPrompt {
            title,
            prompt: "Enter prompt for Claude:".to_string(),
            textarea: Box::new(textarea),
            on_submit: InputAction::AgentPrompt {
                worktree_id,
                worktree_path,
                worktree_slug,
                resume_session_id,
            },
        };
    }

    fn start_agent_tmux(
        &mut self,
        prompt: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
        model: Option<String>,
    ) {
        // Create DB record with tmux window name
        let mgr = AgentManager::new(&self.conn);
        let run = match mgr.create_run(
            &worktree_id,
            &prompt,
            Some(&worktree_slug),
            model.as_deref(),
        ) {
            Ok(run) => run,
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to create agent run: {e}"),
                };
                return;
            }
        };

        // Build the conductor agent run command
        let mut args = vec![
            "agent".to_string(),
            "run".to_string(),
            "--run-id".to_string(),
            run.id.clone(),
            "--worktree-path".to_string(),
            worktree_path,
            "--prompt".to_string(),
            prompt,
        ];

        if let Some(ref session_id) = resume_session_id {
            args.push("--resume".to_string());
            args.push(session_id.clone());
        }

        if let Some(ref m) = model {
            args.push("--model".to_string());
            args.push(m.clone());
        }

        match conductor_core::agent_runtime::spawn_tmux_window(&args, &worktree_slug) {
            Ok(()) => {
                self.state.status_message =
                    Some(format!("Agent launched in tmux window: {worktree_slug}"));
                self.refresh_data();
            }
            Err(e) => {
                let _ = mgr.update_run_failed(&run.id, &e);
                self.state.modal = Modal::Error { message: e };
            }
        }
    }

    fn handle_view_agent_log(&mut self) {
        let run = self.selected_worktree_run();

        let log_path = run.and_then(|r| r.log_file.as_deref());
        let Some(log_path) = log_path else {
            self.state.status_message = Some("No agent log available".to_string());
            return;
        };

        let viewer = std::env::var("EDITOR").unwrap_or_else(|_| "less".to_string());

        let result = Command::new("tmux")
            .args(["new-window", "--", &viewer, log_path])
            .output();

        match result {
            Ok(o) if o.status.success() => {
                self.state.status_message = Some("Opened agent log".to_string());
            }
            Ok(_) => {
                self.state.status_message = Some("Failed to open log viewer".to_string());
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to open log: {e}"));
            }
        }
    }

    fn handle_copy_last_code_block(&mut self) {
        let run = self.selected_worktree_run();

        let log_path = run.and_then(|r| r.log_file.as_deref());
        let Some(log_path) = log_path else {
            self.state.status_message = Some("No agent log available".to_string());
            return;
        };

        let file = match std::fs::File::open(log_path) {
            Ok(f) => f,
            Err(e) => {
                self.state.status_message = Some(format!("Failed to read log: {e}"));
                return;
            }
        };
        let reader = std::io::BufReader::new(file);

        let Some(code_block) = extract_last_code_block(reader) else {
            self.state.status_message = Some("No code block found in log".to_string());
            return;
        };

        // Try pbcopy (macOS), then xclip, then xsel
        let copy_result = Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .or_else(|_| {
                Command::new("xclip")
                    .args(["-selection", "clipboard"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
            })
            .or_else(|_| {
                Command::new("xsel")
                    .arg("--clipboard")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
            });

        match copy_result {
            Ok(mut child) => {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(code_block.as_bytes());
                    drop(stdin); // Close stdin so clipboard tool sees EOF
                }
                match child.wait() {
                    Ok(status) if status.success() => {
                        self.state.status_message = Some("Copied to clipboard".to_string());
                    }
                    _ => {
                        self.state.status_message = Some("Clipboard command failed".to_string());
                    }
                }
            }
            Err(_) => {
                self.state.status_message =
                    Some("No clipboard tool found (pbcopy/xclip/xsel)".to_string());
            }
        }
    }

    fn handle_expand_agent_event(&mut self) {
        let selected = self.state.agent_list_state.borrow().selected().unwrap_or(0);

        let Some(ev) = self.state.data.event_at_visual_index(selected) else {
            return;
        };

        let summary_prefix = truncate_to_char_boundary(&ev.summary, 60);
        let title = format!("[{}] {}", ev.kind, summary_prefix);
        let body = ev.summary.clone();
        let line_count = body.lines().count();

        self.state.modal = Modal::EventDetail {
            title,
            body,
            line_count,
            scroll_offset: 0,
            horizontal_offset: 0,
        };
    }

    // ── GitHub repo discovery ────────────────────────────────────────────────

    fn handle_discover_github_orgs(&mut self) {
        self.state.modal = Modal::GithubDiscoverOrgs {
            orgs: Vec::new(),
            cursor: 0,
            loading: true,
            error: None,
        };

        if let Some(ref tx) = self.bg_tx {
            let tx = tx.clone();
            background::spawn_blocking(tx, move || match github::list_github_orgs() {
                Ok(orgs) => Action::GithubOrgsLoaded { orgs },
                Err(e) => Action::GithubOrgsFailed {
                    error: e.to_string(),
                },
            });
        }
    }

    fn handle_github_orgs_loaded(&mut self, orgs: Vec<String>) {
        if !matches!(
            self.state.modal,
            Modal::GithubDiscoverOrgs { loading: true, .. }
        ) {
            return;
        }
        // Prepend empty string sentinel for "Personal" (displayed as "Personal")
        let mut display_orgs = vec![String::new()];
        display_orgs.extend(orgs);
        self.state.github_orgs_cache = display_orgs.clone();
        self.state.modal = Modal::GithubDiscoverOrgs {
            orgs: display_orgs,
            cursor: 0,
            loading: false,
            error: None,
        };
    }

    fn handle_github_orgs_failed(&mut self, error: String) {
        if matches!(self.state.modal, Modal::GithubDiscoverOrgs { .. }) {
            self.state.modal = Modal::GithubDiscoverOrgs {
                orgs: Vec::new(),
                cursor: 0,
                loading: false,
                error: Some(error),
            };
        }
    }

    fn handle_github_drill_into_owner(&mut self, owner: String) {
        let registered_urls: Vec<String> = self
            .state
            .data
            .repos
            .iter()
            .map(|r| r.remote_url.clone())
            .collect();

        let owner_opt = if owner.is_empty() {
            None
        } else {
            Some(owner.clone())
        };

        self.state.modal = Modal::GithubDiscover {
            owner: owner.clone(),
            repos: Vec::new(),
            registered_urls: Vec::new(),
            selected: Vec::new(),
            cursor: 0,
            loading: true,
            error: None,
        };

        if let Some(ref tx) = self.bg_tx {
            let tx = tx.clone();
            background::spawn_blocking(tx, move || {
                match github::discover_github_repos(owner_opt.as_deref()) {
                    Ok(repos) => Action::GithubDiscoverLoaded(Box::new(GithubDiscoverPayload {
                        owner: owner_opt.unwrap_or_default(),
                        repos,
                        registered_urls,
                    })),
                    Err(e) => Action::GithubDiscoverFailed {
                        error: e.to_string(),
                    },
                }
            });
        }
    }

    fn handle_github_back_to_orgs(&mut self) {
        let orgs = self.state.github_orgs_cache.clone();
        self.state.modal = Modal::GithubDiscoverOrgs {
            orgs,
            cursor: 0,
            loading: false,
            error: None,
        };
    }

    fn handle_github_discover_loaded(&mut self, payload: GithubDiscoverPayload) {
        // Only update if the modal is still open in loading state
        if let Modal::GithubDiscover { loading, .. } = self.state.modal {
            if !loading {
                return;
            }
        } else {
            return;
        }

        let count = payload.repos.len();
        self.state.modal = Modal::GithubDiscover {
            owner: payload.owner,
            selected: vec![false; count],
            repos: payload.repos,
            registered_urls: payload.registered_urls,
            cursor: 0,
            loading: false,
            error: None,
        };
    }

    fn handle_github_discover_failed(&mut self, error: String) {
        let owner = if let Modal::GithubDiscover { ref owner, .. } = self.state.modal {
            owner.clone()
        } else {
            return;
        };
        self.state.modal = Modal::GithubDiscover {
            owner,
            repos: Vec::new(),
            registered_urls: Vec::new(),
            selected: Vec::new(),
            cursor: 0,
            loading: false,
            error: Some(error),
        };
    }

    fn handle_github_discover_toggle(&mut self) {
        if let Modal::GithubDiscover {
            ref repos,
            ref registered_urls,
            ref mut selected,
            cursor,
            ..
        } = self.state.modal
        {
            if let Some(sel) = selected.get_mut(cursor) {
                let repo = &repos[cursor];
                let is_registered = registered_urls.contains(&repo.clone_url)
                    || registered_urls.contains(&repo.ssh_url);
                if !is_registered {
                    *sel = !*sel;
                }
            }
        }
    }

    fn handle_github_discover_select_all(&mut self) {
        if let Modal::GithubDiscover {
            ref repos,
            ref registered_urls,
            ref mut selected,
            ..
        } = self.state.modal
        {
            let any_unselected = repos.iter().zip(selected.iter()).any(|(r, &s)| {
                !s && !registered_urls.contains(&r.clone_url)
                    && !registered_urls.contains(&r.ssh_url)
            });
            for (repo, sel) in repos.iter().zip(selected.iter_mut()) {
                let is_registered = registered_urls.contains(&repo.clone_url)
                    || registered_urls.contains(&repo.ssh_url);
                if !is_registered {
                    *sel = any_unselected;
                }
            }
        }
    }

    fn handle_github_discover_import(&mut self) {
        let to_import: Vec<String> = if let Modal::GithubDiscover {
            ref repos,
            ref registered_urls,
            ref selected,
            ..
        } = self.state.modal
        {
            repos
                .iter()
                .zip(selected.iter())
                .filter_map(|(repo, &sel)| {
                    if sel
                        && !registered_urls.contains(&repo.clone_url)
                        && !registered_urls.contains(&repo.ssh_url)
                    {
                        Some(repo.clone_url.clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            return;
        };

        if to_import.is_empty() {
            return;
        }

        let mgr = RepoManager::new(&self.conn, &self.config);
        let mut imported = 0usize;
        let mut errors = Vec::new();

        for url in &to_import {
            let slug = derive_slug_from_url(url);
            let local_path = derive_local_path(&self.config, &slug);
            match mgr.add(&slug, &local_path, url, None) {
                Ok(_) => imported += 1,
                Err(e) => errors.push(format!("{slug}: {e}")),
            }
        }

        self.refresh_data();
        self.handle_github_back_to_orgs();

        if errors.is_empty() {
            self.state.status_message = Some(format!("Imported {imported} repo(s) from GitHub"));
        } else {
            self.state.status_message = Some(format!(
                "Imported {imported} repo(s), {} error(s)",
                errors.len()
            ));
        }
    }

    // ── Workflow handlers ──────────────────────────────────────────────

    /// Dispatch workflow data loading to a background thread. The result
    /// arrives as a `WorkflowDataRefreshed` action, avoiding synchronous
    /// FS + DB I/O on the main loop tick.
    /// When no worktree is selected (global mode), loads all runs across worktrees.
    fn poll_workflow_data_async(&self) {
        let Some(ref tx) = self.bg_tx else { return };

        // Skip if a poll is already in flight to avoid thread pile-up.
        if self
            .workflow_poll_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let wt_id = self.state.selected_worktree_id.clone();
        let (worktree_path, repo_path) = if let Some(ref id) = wt_id {
            match self.resolve_worktree_paths(id) {
                Some((wt_path, rp)) => (Some(wt_path), Some(rp)),
                None => {
                    self.workflow_poll_in_flight.store(false, Ordering::SeqCst);
                    return;
                }
            }
        } else {
            (None, None)
        };

        let selected_run_id = self.state.selected_workflow_run_id.clone();
        let selected_step_child_run_id = self.selected_step_child_run_id();

        let in_flight = Arc::clone(&self.workflow_poll_in_flight);
        crate::background::spawn_workflow_poll_once_guarded(
            tx.clone(),
            wt_id,
            worktree_path,
            repo_path,
            selected_run_id,
            selected_step_child_run_id,
            in_flight,
        );
    }

    fn reload_workflow_data(&mut self) {
        use conductor_core::workflow::WorkflowManager;

        let wf_mgr = WorkflowManager::new(&self.conn);

        if let Some(ref wt_id) = self.state.selected_worktree_id.clone() {
            // Worktree-scoped: load defs from FS
            if let Some((wt_path, rp)) = self.resolve_worktree_paths(wt_id) {
                self.state.data.workflow_defs =
                    WorkflowManager::list_defs(&wt_path, &rp).unwrap_or_default();
            }
        } else {
            // Global mode: defs are cross-worktree, cleared here and populated by background poller
            self.state.data.workflow_defs.clear();
        }
        self.state.data.workflow_runs = wf_mgr
            .list_workflow_runs_for_scope(self.state.selected_worktree_id.as_deref(), 50)
            .unwrap_or_default();

        // Load steps for the currently selected run
        self.reload_workflow_steps();
        self.clamp_workflow_indices();
    }

    fn reload_workflow_steps(&mut self) {
        use conductor_core::workflow::WorkflowManager;

        if let Some(ref run_id) = self.state.selected_workflow_run_id {
            let wf_mgr = WorkflowManager::new(&self.conn);
            self.state.data.workflow_steps = wf_mgr.get_workflow_steps(run_id).unwrap_or_default();
        } else {
            self.state.data.workflow_steps.clear();
        }
        // Clear stale agent event cache; the background poller will refresh it.
        self.state.data.step_agent_events.clear();
        self.state.data.step_agent_run = None;
    }

    /// Get the child_run_id of the currently selected workflow step.
    fn selected_step_child_run_id(&self) -> Option<String> {
        self.state
            .data
            .workflow_steps
            .get(self.state.workflow_step_index)
            .and_then(|s| s.child_run_id.clone())
    }

    fn clamp_workflow_indices(&mut self) {
        let def_len = self.state.data.workflow_defs.len();
        if def_len > 0 && self.state.workflow_def_index >= def_len {
            self.state.workflow_def_index = def_len - 1;
        }
        let run_len = self.state.data.workflow_runs.len();
        if run_len > 0 && self.state.workflow_run_index >= run_len {
            self.state.workflow_run_index = run_len - 1;
        }
        let step_len = self.state.data.workflow_steps.len();
        if step_len > 0 && self.state.workflow_step_index >= step_len {
            self.state.workflow_step_index = step_len - 1;
        }
        let event_len = self.state.data.step_agent_events.len();
        if event_len > 0 && self.state.step_agent_event_index >= event_len {
            self.state.step_agent_event_index = event_len - 1;
        }
        // Auto-reset focus to Steps if current step has no agent activity
        let has_agent = self
            .state
            .data
            .workflow_steps
            .get(self.state.workflow_step_index)
            .map(|s| s.child_run_id.is_some())
            .unwrap_or(false);
        if !has_agent {
            self.state.workflow_run_detail_focus = WorkflowRunDetailFocus::Steps;
        }
    }

    fn handle_run_workflow(&mut self) {
        let def = match self
            .state
            .data
            .workflow_defs
            .get(self.state.workflow_def_index)
        {
            Some(d) => d.clone(),
            None => {
                self.state.status_message = Some("No workflow definition selected".to_string());
                return;
            }
        };

        let wt = match self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
        {
            Some(w) => w.clone(),
            None => {
                self.state.status_message = Some("No worktree selected".to_string());
                return;
            }
        };

        let repo_path = self
            .state
            .data
            .repos
            .iter()
            .find(|r| r.id == wt.repo_id)
            .map(|r| r.local_path.clone())
            .unwrap_or_default();

        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();

        // Spawn workflow execution in a background thread
        std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let params = WorkflowExecStandalone {
                config,
                workflow: def.clone(),
                worktree_id: wt.id.clone(),
                worktree_path: wt.path.clone(),
                repo_path,
                model: None,
                exec_config: WorkflowExecConfig::default(),
                inputs: std::collections::HashMap::new(),
            };

            let result = execute_workflow_standalone(&params);

            if let Some(ref tx) = bg_tx {
                let msg = match result {
                    Ok(res) => {
                        if res.all_succeeded {
                            format!("Workflow '{}' completed successfully", def.name)
                        } else {
                            format!("Workflow '{}' completed with failures", def.name)
                        }
                    }
                    Err(e) => format!("Workflow '{}' failed: {e}", def.name),
                };
                let _ = tx.send(Action::BackgroundSuccess { message: msg });
            }
        });

        self.state.status_message = Some(format!("Starting workflow '{workflow_name}'…"));
    }

    fn handle_cancel_workflow(&mut self) {
        let run = match self
            .state
            .selected_workflow_run_id
            .as_ref()
            .and_then(|id| self.state.data.workflow_runs.iter().find(|r| &r.id == id))
        {
            Some(r) => r.clone(),
            None => {
                self.state.status_message = Some("No workflow run selected".to_string());
                return;
            }
        };

        self.state.modal = Modal::Confirm {
            title: "Cancel Workflow".to_string(),
            message: format!("Cancel workflow run '{}'?", run.workflow_name),
            on_confirm: ConfirmAction::CancelWorkflow {
                workflow_run_id: run.id.clone(),
            },
        };
    }

    fn handle_approve_gate(&mut self) {
        use conductor_core::workflow::WorkflowManager;

        // If we're in GateAction modal, use its data
        if let Modal::GateAction {
            ref step_id,
            ref feedback,
            ..
        } = self.state.modal
        {
            let wf_mgr = WorkflowManager::new(&self.conn);
            let fb = if feedback.is_empty() {
                None
            } else {
                Some(feedback.as_str())
            };
            match wf_mgr.approve_gate(step_id, "tui-user", fb) {
                Ok(()) => {
                    self.state.status_message = Some("Gate approved".to_string());
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Gate approval failed: {e}"));
                }
            }
            self.state.modal = Modal::None;
            self.reload_workflow_steps();
            return;
        }

        // Otherwise, find the waiting gate and show the GateAction modal
        if let Some(ref run_id) = self.state.selected_workflow_run_id {
            let wf_mgr = WorkflowManager::new(&self.conn);
            if let Ok(Some(step)) = wf_mgr.find_waiting_gate(run_id) {
                self.state.modal = Modal::GateAction {
                    run_id: run_id.clone(),
                    step_id: step.id.clone(),
                    gate_prompt: step.gate_prompt.unwrap_or_default(),
                    feedback: String::new(),
                };
            } else {
                self.state.status_message = Some("No waiting gate found".to_string());
            }
        }
    }

    fn handle_reject_gate(&mut self) {
        use conductor_core::workflow::WorkflowManager;

        if let Modal::GateAction { ref step_id, .. } = self.state.modal {
            let wf_mgr = WorkflowManager::new(&self.conn);
            match wf_mgr.reject_gate(step_id, "tui-user") {
                Ok(()) => {
                    self.state.status_message = Some("Gate rejected".to_string());
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Gate rejection failed: {e}"));
                }
            }
            self.state.modal = Modal::None;
            self.reload_workflow_steps();
        }
    }

    /// Show the selected workflow definition's source file in a scrollable modal.
    fn handle_view_workflow_def(&mut self) {
        let Some(def) = self.selected_workflow_def() else {
            self.state.status_message = Some("No workflow definition selected".to_string());
            return;
        };

        let body = match std::fs::read_to_string(&def.source_path) {
            Ok(s) => s,
            Err(e) => format!("Could not read {}: {e}", def.source_path),
        };
        let line_count = body.lines().count();
        self.state.modal = Modal::EventDetail {
            title: format!(" {} ", def.source_path),
            body,
            line_count,
            scroll_offset: 0,
            horizontal_offset: 0,
        };
    }

    /// Return `(worktree_path, repo_local_path)` for the given worktree ID,
    /// or `None` if the worktree (or its repo) is not found in the data cache.
    fn resolve_worktree_paths(&self, wt_id: &str) -> Option<(String, String)> {
        let wt = self.state.data.worktrees.iter().find(|w| w.id == wt_id)?;
        let repo_path = self
            .state
            .data
            .repos
            .iter()
            .find(|r| r.id == wt.repo_id)
            .map(|r| r.local_path.clone())
            .unwrap_or_default();
        Some((wt.path.clone(), repo_path))
    }

    /// Return the currently selected workflow definition, if any.
    fn selected_workflow_def(&self) -> Option<conductor_core::workflow::WorkflowDef> {
        self.state
            .data
            .workflow_defs
            .get(self.state.workflow_def_index)
            .cloned()
    }

    /// Open the selected workflow definition's source file in $EDITOR.
    fn handle_edit_workflow_def(&mut self) {
        let Some(def) = self.selected_workflow_def() else {
            self.state.status_message = Some("No workflow definition selected".to_string());
            return;
        };

        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| "vi".to_string());

        // Suspend the TUI, open the editor, then restore
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);

        let status = Command::new(&editor).arg(&def.source_path).status();

        let _ = crossterm::terminal::enable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);

        match status {
            Ok(s) if s.success() => {
                self.state.status_message = Some(format!("Saved {}", def.source_path));
                // Reload defs so changes are reflected immediately
                self.reload_workflow_data();
            }
            Ok(_) => {
                self.state.status_message = Some("Editor exited with non-zero status".to_string());
            }
            Err(e) => {
                self.state.status_message = Some(format!("Could not launch {editor}: {e}"));
            }
        }
    }
}

/// Truncate a string to at most `max_chars` characters at a char boundary.
fn truncate_to_char_boundary(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Extract the last fenced code block (```...```) from a reader (line-by-line streaming).
fn extract_last_code_block(reader: impl std::io::BufRead) -> Option<String> {
    let mut last_block: Option<String> = None;
    let mut in_block = false;
    let mut current_block = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim_start().starts_with("```") {
            if in_block {
                // Closing fence — save the block (take avoids clone)
                last_block = Some(std::mem::take(&mut current_block));
                in_block = false;
            } else {
                // Opening fence
                in_block = true;
                current_block.clear();
            }
        } else if in_block {
            if !current_block.is_empty() {
                current_block.push('\n');
            }
            current_block.push_str(&line);
        }
    }

    last_block
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_extract_last_code_block_single() {
        let content = "some text\n```bash\necho hello\n```\nmore text";
        assert_eq!(
            extract_last_code_block(Cursor::new(content)),
            Some("echo hello".to_string())
        );
    }

    #[test]
    fn test_extract_last_code_block_multiple() {
        let content = "```\nfirst\n```\nstuff\n```python\nsecond\nthird\n```\n";
        assert_eq!(
            extract_last_code_block(Cursor::new(content)),
            Some("second\nthird".to_string())
        );
    }

    #[test]
    fn test_extract_last_code_block_none() {
        assert_eq!(extract_last_code_block(Cursor::new("no code here")), None);
    }

    #[test]
    fn test_extract_last_code_block_unclosed() {
        let content = "```\nclosed\n```\n```\nunclosed";
        assert_eq!(
            extract_last_code_block(Cursor::new(content)),
            Some("closed".to_string())
        );
    }

    #[test]
    fn test_clamp_increment_advances() {
        let mut idx = 0;
        clamp_increment(&mut idx, 3);
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_clamp_increment_stops_at_max() {
        let mut idx = 2;
        clamp_increment(&mut idx, 3);
        assert_eq!(idx, 2);
    }

    #[test]
    fn test_clamp_increment_empty_list() {
        let mut idx = 0;
        clamp_increment(&mut idx, 0);
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_wrap_increment_advances() {
        let mut idx = 0;
        wrap_increment(&mut idx, 3);
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_wrap_increment_wraps_to_zero() {
        let mut idx = 2;
        wrap_increment(&mut idx, 3);
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_wrap_decrement_decreases() {
        let mut idx = 2;
        wrap_decrement(&mut idx, 3);
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_wrap_decrement_wraps_to_end() {
        let mut idx = 0;
        wrap_decrement(&mut idx, 3);
        assert_eq!(idx, 2);
    }

    #[test]
    fn test_wrap_decrement_empty_list() {
        let mut idx = 0;
        wrap_decrement(&mut idx, 0);
        assert_eq!(idx, 0);
    }
}
