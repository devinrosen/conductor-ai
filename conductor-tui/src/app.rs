use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::widgets::ListState;
use ratatui::DefaultTerminal;
use rusqlite::Connection;

use conductor_core::agent::{AgentManager, AgentRun, FeedbackRequest};
use conductor_core::config::{AutoStartAgent, Config};
use conductor_core::github;
use conductor_core::issue_source::IssueSourceManager;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::workflow::{parse_workflow_str, MetadataEntry, WorkflowWarning};
use conductor_core::worktree::WorktreeManager;

use crate::action::{Action, GithubDiscoverPayload};
use crate::background;
use crate::event::{BackgroundSender, EventLoop};
use crate::input;
use crate::state::{
    info_row, repo_info_row, AppState, ConfirmAction, DashboardRow, FormAction, FormField,
    FormFieldType, InputAction, Modal, PostCreateChoice, RepoDetailFocus, View, WorkflowDefFocus,
    WorkflowRunDetailFocus, WorkflowsFocus, WorktreeDetailFocus,
};
use crate::theme::Theme;
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
/// Build a status-bar message for workflow parse warnings, or `None` if there are none.
fn workflow_parse_warning_message(warnings: &[WorkflowWarning]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }
    let count = warnings.len();
    let label = warnings
        .iter()
        .map(|w| w.file.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "⚠ {count} workflow file(s) failed to parse: {label}"
    ))
}

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

/// Send a workflow execution result through the background channel.
///
/// Shared by all three `spawn_*_workflow_in_background` helpers to avoid
/// duplicating the success/failure dispatch logic.
fn send_workflow_result(
    bg_tx: &Option<crate::event::BackgroundSender>,
    workflow_name: &str,
    result: conductor_core::error::Result<conductor_core::workflow::WorkflowResult>,
) {
    if let Some(ref tx) = bg_tx {
        match result {
            Ok(res) => {
                let msg = if res.all_succeeded {
                    format!("Workflow '{workflow_name}' completed successfully")
                } else {
                    format!("Workflow '{workflow_name}' completed with failures")
                };
                tx.send(Action::BackgroundSuccess { message: msg });
            }
            Err(e) => {
                tx.send(Action::BackgroundError {
                    message: format!("Workflow '{workflow_name}' failed: {e}"),
                });
            }
        }
    }
}

/// Build `FormField`s from workflow `InputDecl`s.
fn build_form_fields(inputs: &[conductor_core::workflow::InputDecl]) -> Vec<FormField> {
    use conductor_core::workflow::InputType;
    inputs
        .iter()
        .map(|inp| {
            let (value, field_type) = if inp.input_type == InputType::Boolean {
                (
                    inp.default.clone().unwrap_or_else(|| "false".to_string()),
                    FormFieldType::Boolean,
                )
            } else {
                (inp.default.clone().unwrap_or_default(), FormFieldType::Text)
            };
            FormField {
                label: inp.name.clone(),
                value,
                placeholder: if inp.required {
                    "(required)".to_string()
                } else {
                    String::new()
                },
                manually_edited: false,
                required: inp.required,
                field_type,
            }
        })
        .collect()
}

pub struct App {
    state: AppState,
    conn: Connection,
    config: Config,
    bg_tx: Option<BackgroundSender>,
    /// Guard to prevent multiple concurrent workflow poll threads.
    workflow_poll_in_flight: Arc<AtomicBool>,
    /// Background workflow execution thread handles.
    workflow_threads: Vec<std::thread::JoinHandle<()>>,
    /// Shutdown signal sent to workflow executor threads on TUI exit.
    workflow_shutdown: Arc<AtomicBool>,
}

impl App {
    pub fn new(conn: Connection, config: Config, theme: Theme) -> Self {
        let mut state = AppState::new();
        state.theme = theme;
        Self {
            state,
            conn,
            config,
            bg_tx: None,
            workflow_poll_in_flight: Arc::new(AtomicBool::new(false)),
            workflow_threads: Vec::new(),
            workflow_shutdown: Arc::new(AtomicBool::new(false)),
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

        // Signal all workflow executor threads to stop, then join them with a
        // 10-second bounded timeout. Threads that don't finish in time are
        // abandoned; the startup recovery path will reconcile their steps.
        self.workflow_shutdown.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        for handle in self.workflow_threads.drain(..) {
            loop {
                if handle.is_finished() {
                    let _ = handle.join();
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        Ok(())
    }

    /// Handle an action by mutating state. Returns true if the UI needs a redraw.
    ///
    /// This thin wrapper delegates to `handle_action` and updates
    /// `status_message_at` whenever the status message presence changes.
    pub(crate) fn update(&mut self, action: Action) -> bool {
        let had_message = self.state.status_message.is_some();
        let dirty = self.handle_action(action);
        self.state.track_status_message_change(had_message);
        dirty
    }

    fn handle_action(&mut self, action: Action) -> bool {
        match action {
            Action::None => return false,
            Action::Tick => {
                // Prune completed workflow threads so the Vec doesn't accumulate
                // stale handles across the session. Only in-flight threads remain,
                // making the quit-time join fast in the common case.
                self.workflow_threads.retain(|h| !h.is_finished());
                // Poll workflow data asynchronously on every tick so the global
                // status bar (and workflow views) stay current regardless of which
                // view is active.
                self.poll_workflow_data_async();
                // Periodically refresh the PR list when the RepoDetail view is active.
                if self.state.view == View::RepoDetail {
                    let needs_refresh = self
                        .state
                        .pr_last_fetched_at
                        .map(|t| t.elapsed() >= Duration::from_secs(30))
                        .unwrap_or(true);
                    if needs_refresh {
                        if let Some(ref repo_id) = self.state.selected_repo_id.clone() {
                            if let Some(repo) =
                                self.state.data.repos.iter().find(|r| &r.id == repo_id)
                            {
                                let remote_url = repo.remote_url.clone();
                                let rid = repo_id.clone();
                                if let Some(ref tx) = self.bg_tx {
                                    background::spawn_pr_fetch_once(tx.clone(), remote_url, rid);
                                }
                            }
                        }
                    }
                }
                // Auto-clear status messages after 4 seconds so the context hint
                // bar is restored without requiring user navigation.
                self.state.tick_status_message(Duration::from_secs(4));
                // Always redraw on tick so elapsed times, spinners, and other
                // time-sensitive indicators update smoothly (ratatui diffs cells,
                // so this is cheap).
                return true;
            }
            Action::Quit => {
                if matches!(self.state.modal, Modal::None) {
                    self.show_confirm_quit();
                } else {
                    self.state.should_quit = true;
                }
            }

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

            Action::FocusContentColumn => {
                self.state.column_focus = crate::state::ColumnFocus::Content;
            }
            Action::FocusWorkflowColumn => {
                if self.state.workflow_column_visible {
                    self.state.column_focus = crate::state::ColumnFocus::Workflow;
                }
            }
            Action::ToggleWorkflowColumn => {
                self.state.workflow_column_visible = !self.state.workflow_column_visible;
                if !self.state.workflow_column_visible {
                    self.state.column_focus = crate::state::ColumnFocus::Content;
                }
            }

            // Filter
            Action::EnterFilter => self.state.active_filter_mut().enter(),
            Action::EnterLabelFilter => {
                self.state.label_filter.enter();
            }
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
                if matches!(self.state.modal, Modal::Progress { .. }) {
                    return true;
                }
                // Esc on ThemePicker restores the theme that was active before preview
                if let Modal::ThemePicker {
                    ref original_theme, ..
                } = self.state.modal
                {
                    self.state.theme = *original_theme;
                }
                self.state.modal = Modal::None;
            }
            Action::CopyErrorMessage => {
                if let Modal::Error { ref message } = self.state.modal {
                    self.copy_text_to_clipboard(message.clone());
                }
            }
            Action::OpenTicketUrl => self.handle_open_ticket_url(),
            Action::CopyTicketUrl => self.handle_copy_ticket_url(),
            Action::OpenRepoUrl => self.handle_open_repo_url(),
            Action::CopyRepoUrl => self.handle_copy_repo_url(),
            Action::OpenPrUrl => self.handle_open_pr_url(),
            Action::CopyPrUrl => self.handle_copy_pr_url(),
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
            Action::FormToggle => self.handle_form_toggle(),

            // CRUD
            Action::RegisterRepo => self.handle_register_repo(),
            Action::Create => self.handle_create(),
            Action::Delete => self.handle_delete(),
            Action::Push => self.handle_push(),
            Action::CreatePr => self.handle_create_pr(),
            Action::SyncTickets => self.handle_sync_tickets(),
            Action::LinkTicket => self.handle_link_ticket(),
            Action::SelectPostCreateChoice(index) => self.handle_post_create_pick(index),
            Action::PostCreatePickerReady {
                items,
                worktree_id,
                worktree_path,
                worktree_slug,
                ticket_id,
                repo_path,
            } => {
                self.state.modal = Modal::PostCreatePicker {
                    items,
                    selected: 0,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                    repo_path,
                };
            }
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
            // Model configuration
            Action::SetModel => self.handle_set_model(),

            // Theme picker
            Action::ShowThemePicker => self.handle_show_theme_picker(),
            Action::ThemesLoaded {
                themes,
                loaded_themes,
                warnings,
            } => {
                self.handle_themes_loaded(themes, loaded_themes, warnings);
            }
            Action::ThemePreview(idx) => self.handle_theme_preview(idx),
            Action::ThemeSaveComplete { result } => {
                self.state.modal = match result {
                    Ok(msg) => {
                        self.state.status_message = Some(msg);
                        Modal::None
                    }
                    Err(e) => Modal::Error {
                        message: format!("Failed to save theme: {e}"),
                    },
                };
            }

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

            // Workflow completed/cancelled visibility toggle
            Action::ToggleCompletedRuns => {
                self.state.show_completed_workflow_runs = !self.state.show_completed_workflow_runs;
                self.clamp_workflow_indices();
            }

            // WorktreeDetail panel actions
            Action::WorktreeDetailCopy => self.handle_worktree_detail_copy(),
            Action::WorktreeDetailOpen => self.handle_worktree_detail_open(),
            Action::RepoDetailInfoOpen => self.handle_repo_detail_info_open(),
            Action::RepoDetailInfoCopy => self.handle_repo_detail_info_copy(),

            // Agent (tmux-based)
            Action::LaunchAgent => self.handle_launch_agent(),
            Action::OrchestrateAgent => self.handle_orchestrate_agent(),
            Action::StopAgent => self.handle_stop_agent(),
            Action::SubmitFeedback => self.handle_submit_feedback(),
            Action::DismissFeedback => self.handle_dismiss_feedback(),
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
            Action::PickWorkflow => self.handle_pick_workflow(),
            Action::RunWorkflow => self.handle_run_workflow(),
            Action::RunPrWorkflow => self.handle_run_pr_workflow(),
            Action::ResumeWorkflow => self.handle_resume_workflow(),
            Action::ResumeWorktreeWorkflow => self.handle_resume_worktree_workflow(),
            Action::CancelWorkflow => self.handle_cancel_workflow(),
            Action::ApproveGate => self.handle_approve_gate(),
            Action::RejectGate => self.handle_reject_gate(),
            Action::ViewWorkflowDef => self.handle_view_workflow_def(),
            Action::EditWorkflowDef => self.handle_edit_workflow_def(),
            Action::ToggleDefStepTree => {
                if self.state.workflow_def_focus == WorkflowDefFocus::Steps {
                    self.state.workflow_def_focus = WorkflowDefFocus::List;
                } else {
                    let has_steps = self
                        .state
                        .data
                        .workflow_defs
                        .get(self.state.workflow_def_index)
                        .map(|d| !d.body.is_empty())
                        .unwrap_or(false);
                    if has_steps {
                        self.state.workflow_def_focus = WorkflowDefFocus::Steps;
                        self.state.workflow_def_step_index = 0;
                    }
                }
            }
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
                if let Some(defs) = payload.workflow_defs {
                    self.state.data.workflow_defs = defs;
                }
                if let Some(slugs) = payload.workflow_def_slugs {
                    self.state.data.workflow_def_slugs = slugs;
                }
                self.state.data.workflow_run_declared_inputs = payload
                    .workflow_runs
                    .iter()
                    .filter_map(|run| {
                        let snapshot = run.definition_snapshot.as_deref()?;
                        let inputs = parse_workflow_str(snapshot, "").ok()?.inputs;
                        Some((run.id.clone(), inputs))
                    })
                    .collect();
                self.state.data.workflow_runs = payload.workflow_runs;
                self.state.data.workflow_steps = payload.workflow_steps;
                self.state.data.step_agent_events = payload.step_agent_events;
                self.state.data.step_agent_run = payload.step_agent_run;
                self.state.data.workflow_run_steps = payload.all_run_steps;
                self.state.init_collapse_state();
                if let Some(msg) = workflow_parse_warning_message(&payload.workflow_parse_warnings)
                {
                    self.state.status_message = Some(msg);
                }
                self.clamp_workflow_indices();
                return true; // Always redraw since workflow column is persistent
            }

            Action::ToggleWorkflowRunCollapse => {
                let visible = self.state.visible_workflow_run_rows();
                match visible.get(self.state.workflow_run_index) {
                    Some(crate::state::WorkflowRunRow::RepoHeader { repo_slug, .. }) => {
                        let key = repo_slug.clone();
                        if self.state.collapsed_repo_headers.contains(&key) {
                            self.state.collapsed_repo_headers.remove(&key);
                        } else {
                            self.state.collapsed_repo_headers.insert(key);
                        }
                    }
                    Some(crate::state::WorkflowRunRow::TargetHeader { target_key, .. }) => {
                        let key = target_key.clone();
                        if self.state.collapsed_target_headers.contains(&key) {
                            self.state.collapsed_target_headers.remove(&key);
                        } else {
                            self.state.collapsed_target_headers.insert(key);
                        }
                    }
                    // Leaf runs (no children): toggle step expansion.
                    Some(crate::state::WorkflowRunRow::Parent {
                        run_id,
                        child_count: 0,
                        ..
                    }) => {
                        let id = run_id.clone();
                        if self.state.expanded_step_run_ids.contains(&id) {
                            self.state.expanded_step_run_ids.remove(&id);
                        } else {
                            self.state.expanded_step_run_ids.insert(id);
                        }
                    }
                    Some(crate::state::WorkflowRunRow::Child {
                        run_id,
                        child_count: 0,
                        ..
                    }) => {
                        let id = run_id.clone();
                        if self.state.expanded_step_run_ids.contains(&id) {
                            self.state.expanded_step_run_ids.remove(&id);
                        } else {
                            self.state.expanded_step_run_ids.insert(id);
                        }
                    }
                    // Non-leaf Parent: toggle child run collapse.
                    Some(crate::state::WorkflowRunRow::Parent { run_id, .. }) => {
                        let run_id = run_id.clone();
                        if self.state.collapsed_workflow_run_ids.contains(&run_id) {
                            self.state.collapsed_workflow_run_ids.remove(&run_id);
                        } else {
                            self.state.collapsed_workflow_run_ids.insert(run_id);
                        }
                    }
                    Some(crate::state::WorkflowRunRow::Child {
                        run_id,
                        child_count,
                        ..
                    }) if *child_count > 0 => {
                        let id = run_id.clone();
                        if self.state.collapsed_workflow_run_ids.contains(&id) {
                            self.state.collapsed_workflow_run_ids.remove(&id);
                        } else {
                            self.state.collapsed_workflow_run_ids.insert(id);
                        }
                    }
                    _ => {}
                }
                // Clamp index after visibility change.
                let new_len = self.state.visible_workflow_run_rows().len();
                if new_len > 0 && self.state.workflow_run_index >= new_len {
                    self.state.workflow_run_index = new_len - 1;
                }
            }

            // Background results
            Action::PrsRefreshed { repo_id, mut prs } => {
                if self.state.selected_repo_id.as_deref() == Some(&repo_id) {
                    prs.sort_by_key(|pr| {
                        if pr.is_draft {
                            3u8
                        } else {
                            match pr.review_decision.as_deref() {
                                Some("CHANGES_REQUESTED") => 0,
                                Some("APPROVED") => 1,
                                _ => 2,
                            }
                        }
                    });
                    self.state.detail_prs = prs;
                    self.state.detail_pr_index = 0;
                    self.state.pr_last_fetched_at = Some(std::time::Instant::now());
                }
            }
            Action::DataRefreshed(payload) => {
                self.state.data.repos = payload.repos;
                self.state.data.worktrees = payload.worktrees;
                self.state.data.tickets = payload.tickets;
                self.state.data.ticket_labels = payload.ticket_labels;
                self.state.data.latest_agent_runs = payload.latest_agent_runs;
                self.state.data.ticket_agent_totals = payload.ticket_agent_totals;
                self.state.data.latest_workflow_runs_by_worktree =
                    payload.latest_workflow_runs_by_worktree;
                self.state.data.workflow_step_summaries = payload.workflow_step_summaries;
                self.state.data.active_non_worktree_workflow_runs =
                    payload.active_non_worktree_workflow_runs;
                self.refresh_pending_feedback();
                self.state.data.rebuild_maps();
                self.reload_agent_events();
                self.state.rebuild_filtered_tickets();
                self.clamp_indices();
                // Always redraw since workflow column is persistent across all views.
                return true;
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
            Action::WorktreeCreated {
                wt_id,
                wt_path,
                wt_slug,
                wt_repo_id,
                warnings,
                ticket_id,
            } => {
                self.state.modal = Modal::None;
                let mut msg = if let Some(ref tid) = ticket_id {
                    let source_id = self
                        .state
                        .data
                        .ticket_map
                        .get(tid)
                        .map(|t| t.source_id.as_str())
                        .unwrap_or("?");
                    format!("Created worktree: {} (linked to #{})", wt_slug, source_id)
                } else {
                    format!("Created worktree: {}", wt_slug)
                };
                if !warnings.is_empty() {
                    msg.push_str(&format!(" [{}]", warnings.join("; ")));
                }
                self.state.status_message = Some(msg);
                self.refresh_data();
                if let Some(tid) = ticket_id {
                    self.maybe_start_agent_for_worktree(wt_id, wt_path, wt_slug, tid, wt_repo_id);
                }
            }
            Action::WorktreeCreateFailed { message } => {
                self.state.modal = Modal::Error { message };
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
            Action::PushComplete { result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(msg) => self.state.status_message = Some(msg),
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Push failed: {e}"),
                        }
                    }
                }
            }
            Action::PrCreateComplete { result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(url) => self.state.status_message = Some(format!("PR created: {url}")),
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("PR creation failed: {e}"),
                        }
                    }
                }
            }
            Action::WorktreeDeleteComplete { wt_slug, result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(status) => {
                        self.state.status_message =
                            Some(format!("Worktree {} marked as {}", wt_slug, status));
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
            Action::RepoUnregisterComplete { repo_slug, result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(()) => {
                        self.state.status_message = Some(format!("Unregistered repo: {repo_slug}"));
                        self.state.view = View::Dashboard;
                        self.state.selected_repo_id = None;
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error {
                            message: format!("Unregister failed: {e}"),
                        };
                    }
                }
            }
            Action::GithubImportComplete { imported, errors } => {
                self.state.modal = Modal::None;
                self.handle_github_back_to_orgs();
                if errors.is_empty() {
                    self.state.status_message =
                        Some(format!("Imported {imported} repo(s) from GitHub"));
                } else {
                    self.state.status_message = Some(format!(
                        "Imported {imported} repo(s), {} error(s)",
                        errors.len()
                    ));
                }
                self.refresh_data();
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
            totals.total_input_tokens += run.input_tokens.unwrap_or(0);
            totals.total_output_tokens += run.output_tokens.unwrap_or(0);
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
    }

    fn go_back(&mut self) {
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

    fn next_panel(&mut self) {
        use crate::state::ColumnFocus;
        match self.state.column_focus {
            ColumnFocus::Workflow => {
                // Exit step tree first if active, then Tab toggles Defs↔Runs.
                if self.state.workflow_def_focus == WorkflowDefFocus::Steps {
                    self.state.workflow_def_focus = WorkflowDefFocus::List;
                } else {
                    self.state.workflows_focus = self.state.workflows_focus.toggle();
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

    fn prev_panel(&mut self) {
        use crate::state::ColumnFocus;
        match self.state.column_focus {
            ColumnFocus::Workflow => {
                if self.state.workflows_focus == WorkflowsFocus::Defs
                    && self.state.workflow_def_focus == WorkflowDefFocus::Steps
                {
                    self.state.workflow_def_focus = WorkflowDefFocus::List;
                } else {
                    self.state.workflows_focus = self.state.workflows_focus.toggle();
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
    fn toggle_workflow_run_detail_focus(&mut self) {
        if self.state.selected_step_has_agent() {
            self.state.workflow_run_detail_focus = self.state.workflow_run_detail_focus.toggle();
        }
    }

    fn workflow_column_move_up(&mut self) {
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
            WorkflowsFocus::Runs => {
                self.state.workflow_run_index = self.state.workflow_run_index.saturating_sub(1);
            }
        }
    }

    fn workflow_column_move_down(&mut self) {
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
            WorkflowsFocus::Runs => {
                let visible_len = self.state.visible_workflow_run_rows().len();
                clamp_increment(&mut self.state.workflow_run_index, visible_len);
            }
        }
    }

    fn workflow_column_select(&mut self) {
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

    fn select(&mut self) {
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
                                background::spawn_pr_fetch_once(
                                    tx.clone(),
                                    remote_url,
                                    repo_id.clone(),
                                );
                            }
                            self.state.rebuild_filtered_tickets();
                            self.state.repo_detail_focus = RepoDetailFocus::Worktrees;
                            self.state.view = View::RepoDetail;
                        }
                    }
                    Some(&DashboardRow::Worktree(wt_idx)) => {
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
            View::WorkflowDefDetail => {}
        }
    }

    /// Resolve the URL of the currently focused ticket, across all contexts.
    fn selected_ticket_url(&self) -> Option<String> {
        if let Modal::TicketInfo { ref ticket } = self.state.modal {
            return Some(ticket.url.clone());
        }
        if self.state.view == View::WorktreeDetail {
            return self
                .state
                .selected_worktree_id
                .as_ref()
                .and_then(|wt_id| self.state.data.worktrees.iter().find(|w| &w.id == wt_id))
                .and_then(|wt| wt.ticket_id.as_ref())
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .map(|t| t.url.clone());
        }
        // Ticket list views: RepoDetail Tickets pane
        let ticket = match self.state.view {
            View::RepoDetail if self.state.repo_detail_focus == RepoDetailFocus::Tickets => self
                .state
                .filtered_detail_tickets
                .get(self.state.detail_ticket_index),
            _ => None,
        };
        ticket.map(|t| t.url.clone())
    }

    /// Open a URL in the default browser, checking the exit code.
    fn open_url(&mut self, url: &str, label: &str) {
        match Command::new("open")
            .arg(url)
            .output()
            .or_else(|_| Command::new("xdg-open").arg(url).output())
        {
            Ok(output) if output.status.success() => {
                self.state.status_message = Some(format!("Opened {url}"));
            }
            Ok(output) => {
                let code = output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                self.state.status_message =
                    Some(format!("Failed to open {label} URL (exit code {code})"));
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to open {label} URL: {e}"));
            }
        }
    }

    fn handle_open_ticket_url(&mut self) {
        match self.selected_ticket_url().filter(|u| !u.is_empty()) {
            None => {
                self.state.status_message = Some("No ticket URL available".to_string());
            }
            Some(url) => self.open_url(&url, "ticket"),
        }
    }

    fn handle_copy_ticket_url(&mut self) {
        match self.selected_ticket_url().filter(|u| !u.is_empty()) {
            None => {
                self.state.status_message = Some("No ticket URL available".to_string());
            }
            Some(url) => self.copy_text_to_clipboard(url),
        }
    }

    /// Build a web URL from a repo's remote_url (SSH or HTTPS GitHub format).
    fn repo_web_url(&self) -> Option<String> {
        let remote_url = self.state.selected_repo().map(|r| r.remote_url.clone())?;
        let (owner, repo) = conductor_core::github::parse_github_remote(&remote_url)?;
        Some(format!("https://github.com/{owner}/{repo}"))
    }

    fn handle_open_repo_url(&mut self) {
        match self.repo_web_url() {
            Some(url) => self.open_url(&url, "repo"),
            None => {
                self.state.status_message = Some("No GitHub URL found for this repo".to_string());
            }
        }
    }

    fn handle_copy_repo_url(&mut self) {
        match self.repo_web_url() {
            Some(url) => self.copy_text_to_clipboard(url),
            None => {
                self.state.status_message = Some("No GitHub URL found for this repo".to_string());
            }
        }
    }

    /// Build the web URL for the currently selected PR in RepoDetail.
    fn selected_pr_url(&self) -> Option<String> {
        let pr = self.state.detail_prs.get(self.state.detail_pr_index)?;
        let base = self.repo_web_url()?;
        Some(format!("{base}/pull/{}", pr.number))
    }

    fn handle_open_pr_url(&mut self) {
        match self.selected_pr_url() {
            None => {
                self.state.status_message = Some("No PR URL available".to_string());
            }
            Some(url) => self.open_url(&url, "PR"),
        }
    }

    fn handle_copy_pr_url(&mut self) {
        match self.selected_pr_url() {
            None => {
                self.state.status_message = Some("No PR URL available".to_string());
            }
            Some(url) => self.copy_text_to_clipboard(url),
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
            ConfirmAction::CreateWorktree {
                repo_slug,
                wt_name,
                ticket_id,
                from_pr,
            } => {
                self.spawn_worktree_create(repo_slug, wt_name, ticket_id, from_pr);
            }
            ConfirmAction::DeleteWorktree { repo_slug, wt_slug } => {
                let Some(bg_tx) = self.bg_tx.clone() else {
                    return;
                };
                self.state.modal = Modal::Progress {
                    message: "Deleting worktree…".to_string(),
                };
                let config = self.config.clone();
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<String> {
                        let db = conductor_core::config::db_path();
                        let conn = conductor_core::db::open_database(&db)?;
                        let wt_mgr = WorktreeManager::new(&conn, &config);
                        let wt = wt_mgr.delete(&repo_slug, &wt_slug)?;
                        Ok(wt.status.to_string())
                    })();
                    let _ = bg_tx.send(Action::WorktreeDeleteComplete {
                        wt_slug,
                        result: result.map_err(|e| e.to_string()),
                    });
                });
            }
            ConfirmAction::UnregisterRepo { repo_slug } => {
                let Some(bg_tx) = self.bg_tx.clone() else {
                    return;
                };
                self.state.modal = Modal::Progress {
                    message: "Unregistering repo…".to_string(),
                };
                let config = self.config.clone();
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<()> {
                        let db = conductor_core::config::db_path();
                        let conn = conductor_core::db::open_database(&db)?;
                        let mgr = RepoManager::new(&conn, &config);
                        mgr.unregister(&repo_slug).map_err(anyhow::Error::from)
                    })();
                    let _ = bg_tx.send(Action::RepoUnregisterComplete {
                        repo_slug,
                        result: result.map_err(|e| e.to_string()),
                    });
                });
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
            ConfirmAction::ResumeWorkflow { workflow_run_id } => {
                let config = self.config.clone();
                let bg_tx = self.bg_tx.clone();
                let run_id = workflow_run_id.clone();

                std::thread::spawn(move || {
                    use conductor_core::workflow::{
                        resume_workflow_standalone, WorkflowResumeStandalone,
                    };

                    let params = WorkflowResumeStandalone {
                        config,
                        workflow_run_id: run_id,
                        model: None,
                        from_step: None,
                        restart: false,
                        db_path: None,
                    };

                    let result = resume_workflow_standalone(&params);

                    if let Some(ref tx) = bg_tx {
                        let msg = match result {
                            Ok(res) => {
                                if res.all_succeeded {
                                    "Workflow resumed and completed successfully".to_string()
                                } else {
                                    "Workflow resumed but finished with failures".to_string()
                                }
                            }
                            Err(e) => format!("Resume failed: {e}"),
                        };
                        let _ = tx.send(Action::BackgroundSuccess { message: msg });
                    }
                });

                self.state.status_message = Some("Resuming workflow…".to_string());
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
            ConfirmAction::Quit => {
                self.state.should_quit = true;
            }
        }
    }

    fn show_confirm_quit(&mut self) {
        let running = self
            .state
            .data
            .latest_agent_runs
            .values()
            .filter(|r| r.status == conductor_core::agent::AgentRunStatus::Running)
            .count();
        let message = if running == 0 {
            "Quit conductor?".to_string()
        } else {
            format!(
                "{running} agent{} running. Quit anyway?",
                if running == 1 { " is" } else { "s are" }
            )
        };
        self.state.modal = Modal::Confirm {
            title: "Confirm Quit".to_string(),
            message,
            on_confirm: ConfirmAction::Quit,
        };
    }

    fn handle_input_submit(&mut self) {
        // ThemePicker: persist the selected theme to config
        if let Modal::ThemePicker { selected, .. } = self.state.modal {
            self.handle_theme_picker_confirm(selected);
            return;
        }

        // PrWorkflowPicker: confirm the selected workflow
        if matches!(self.state.modal, Modal::PrWorkflowPicker { .. }) {
            self.handle_pr_workflow_picker_confirm();
            return;
        }

        // WorkflowPicker: confirm the selected workflow
        if matches!(self.state.modal, Modal::WorkflowPicker { .. }) {
            self.handle_workflow_picker_confirm();
            return;
        }

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
                // Show a second prompt asking for an optional PR number.
                self.state.modal = Modal::Input {
                    title: "From PR (optional)".to_string(),
                    prompt: "PR number to check out (leave blank for new branch):".to_string(),
                    value: String::new(),
                    on_submit: InputAction::CreateWorktreePrStep {
                        repo_slug,
                        wt_name: value,
                        ticket_id,
                    },
                };
            }
            InputAction::CreateWorktreePrStep {
                repo_slug,
                wt_name,
                ticket_id,
            } => {
                // Parse optional PR number (blank → None, non-numeric → error).
                let from_pr: Option<u32> = if value.trim().is_empty() {
                    None
                } else {
                    match value.trim().parse::<u32>() {
                        Ok(n) => Some(n),
                        Err(_) => {
                            self.state.modal = Modal::Error {
                                message: format!(
                                    "Invalid PR number '{}': must be a positive integer",
                                    value.trim()
                                ),
                            };
                            return;
                        }
                    }
                };

                // Check if the repo needs to be cloned first.
                let needs_clone = self
                    .state
                    .data
                    .repos
                    .iter()
                    .find(|r| r.slug == repo_slug)
                    .map(|r| !std::path::Path::new(&r.local_path).exists())
                    .unwrap_or(false);

                if needs_clone {
                    self.state.modal = Modal::Confirm {
                        title: "Clone Required".to_string(),
                        message: format!(
                            "Repo '{}' is not cloned locally. Clone it now?",
                            repo_slug
                        ),
                        on_confirm: ConfirmAction::CreateWorktree {
                            repo_slug,
                            wt_name,
                            ticket_id,
                            from_pr,
                        },
                    };
                } else {
                    self.spawn_worktree_create(repo_slug, wt_name, ticket_id, from_pr);
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
            View::RepoDetail if self.state.repo_detail_focus == RepoDetailFocus::Tickets => self
                .state
                .filtered_detail_tickets
                .get(self.state.detail_ticket_index)
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
                } else if self.state.view == View::Dashboard && self.state.data.repos.is_empty() {
                    // No repos registered yet — open register repo form instead
                    self.handle_register_repo();
                } else {
                    self.state.status_message = Some("Select a repo first".to_string());
                }
            }
            _ => {}
        }
    }

    fn handle_register_repo(&mut self) {
        if self.state.view != View::Dashboard {
            return;
        }
        self.state.modal = Modal::Form {
            title: "Register Repository".to_string(),
            fields: vec![
                FormField {
                    label: "Remote URL".to_string(),
                    value: String::new(),
                    placeholder: "https://github.com/org/repo.git".to_string(),
                    manually_edited: true,
                    required: true,
                    field_type: FormFieldType::Text,
                },
                FormField {
                    label: "Slug".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from URL".to_string(),
                    manually_edited: false,
                    required: true,
                    field_type: FormFieldType::Text,
                },
                FormField {
                    label: "Local Path".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from slug".to_string(),
                    manually_edited: false,
                    required: false,
                    field_type: FormFieldType::Text,
                },
            ],
            active_field: 0,
            on_submit: FormAction::RegisterRepo,
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
                FormAction::RegisterRepo => {
                    Self::auto_derive_register_repo_fields(fields, active_field, config)
                }
                FormAction::AddIssueSource { .. } if active_field == 0 => {
                    Self::sync_issue_source_form_fields(fields);
                }
                FormAction::AddIssueSource { .. } => {}
                FormAction::RunWorkflow(_) => {}
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
                FormAction::RegisterRepo => {
                    Self::auto_derive_register_repo_fields(fields, active_field, config)
                }
                FormAction::AddIssueSource { .. } if active_field == 0 => {
                    Self::sync_issue_source_form_fields(fields);
                }
                FormAction::AddIssueSource { .. } => {}
                FormAction::RunWorkflow(_) => {}
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

    fn auto_derive_register_repo_fields(
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
                field_type: FormFieldType::Text,
            });
            fields.push(FormField {
                label: "Jira URL".to_string(),
                value: String::new(),
                placeholder: "e.g. https://mycompany.atlassian.net".to_string(),
                manually_edited: false,
                required: true,
                field_type: FormFieldType::Text,
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
                FormAction::RegisterRepo => self.submit_register_repo(fields),
                FormAction::AddIssueSource {
                    repo_id,
                    repo_slug,
                    remote_url,
                } => self.submit_add_issue_source(fields, &repo_id, &repo_slug, &remote_url),
                FormAction::RunWorkflow(action) => {
                    let inputs = fields.into_iter().map(|f| (f.label, f.value)).collect();
                    self.submit_run_workflow_with_inputs(
                        inputs,
                        action.target,
                        action.workflow_def,
                    );
                }
            }
        }
    }

    fn handle_form_toggle(&mut self) {
        if let Modal::Form {
            ref mut fields,
            active_field,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                if matches!(field.field_type, FormFieldType::Boolean) {
                    field.value = if field.value == "true" {
                        "false".to_string()
                    } else {
                        "true".to_string()
                    };
                    field.manually_edited = true;
                } else {
                    // For text fields, treat space as a regular character input
                    field.value.push(' ');
                    field.manually_edited = true;
                }
            }
        }
    }

    fn submit_register_repo(&mut self, fields: Vec<FormField>) {
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
        match mgr.register(&slug, &local, &url, None) {
            Ok(repo) => {
                self.state.status_message = Some(format!("Registered repo: {}", repo.slug));
                self.refresh_data();
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Register repo failed: {e}"),
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

    /// Show the input form if the workflow declares inputs, otherwise dispatch immediately.
    /// This is the shared entry point from both `handle_workflow_picker_confirm` and
    /// `handle_pr_workflow_picker_confirm`.
    fn show_workflow_inputs_or_run(
        &mut self,
        target: crate::state::WorkflowPickerTarget,
        def: conductor_core::workflow::WorkflowDef,
        prefill: std::collections::HashMap<String, String>,
    ) {
        if !def.inputs.is_empty() {
            let mut fields = build_form_fields(&def.inputs);
            for field in &mut fields {
                if let Some(v) = prefill.get(&field.label) {
                    field.value = v.clone();
                    field.manually_edited = true;
                }
            }
            self.state.modal = Modal::Form {
                title: format!("Inputs for '{}'", def.name),
                fields,
                active_field: 0,
                on_submit: crate::state::FormAction::RunWorkflow(Box::new(
                    crate::state::RunWorkflowAction {
                        target,
                        workflow_def: def,
                    },
                )),
            };
        } else {
            self.submit_run_workflow_with_inputs(prefill, target, def);
        }
    }

    fn submit_run_workflow_with_inputs(
        &mut self,
        inputs: std::collections::HashMap<String, String>,
        target: crate::state::WorkflowPickerTarget,
        def: conductor_core::workflow::WorkflowDef,
    ) {
        use crate::state::WorkflowPickerTarget;

        match target {
            WorkflowPickerTarget::Worktree {
                worktree_id,
                worktree_path,
                repo_path,
            } => {
                // Block if a workflow run is already active on this worktree
                {
                    use conductor_core::workflow::WorkflowManager;
                    let wf_mgr = WorkflowManager::new(&self.conn);
                    match wf_mgr.get_active_run_for_worktree(&worktree_id) {
                        Ok(Some(active)) => {
                            self.state.status_message = Some(format!(
                                "Workflow '{}' is already running — cancel it before starting another",
                                active.workflow_name
                            ));
                            return;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            self.state.status_message =
                                Some(format!("Failed to check active workflow run: {e}"));
                            return;
                        }
                    }
                }

                let (wt_target_label, wt_ticket_id) = self
                    .state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| w.id == worktree_id)
                    .and_then(|w| {
                        self.state
                            .data
                            .repos
                            .iter()
                            .find(|r| r.id == w.repo_id)
                            .map(|r| (format!("{}/{}", r.slug, w.slug), w.ticket_id.clone()))
                    })
                    .unwrap_or_default();
                // Fall back to inputs["ticket_id"] when the worktree's in-memory state
                // hasn't been refreshed yet (e.g. post-create flow).
                let ticket_id = wt_ticket_id.or_else(|| inputs.get("ticket_id").cloned());
                self.spawn_workflow_in_background(
                    def,
                    worktree_id,
                    worktree_path,
                    repo_path,
                    ticket_id,
                    inputs,
                    wt_target_label,
                );
            }
            WorkflowPickerTarget::Pr { pr_number, .. } => {
                let remote_url = match self
                    .state
                    .selected_repo_id
                    .as_ref()
                    .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
                    .map(|r| r.remote_url.clone())
                {
                    Some(url) => url,
                    None => {
                        self.state.modal = Modal::Error {
                            message: "No repo selected".to_string(),
                        };
                        return;
                    }
                };
                let (owner, repo) = match conductor_core::github::parse_github_remote(&remote_url) {
                    Some(pair) => pair,
                    None => {
                        self.state.modal = Modal::Error {
                            message: format!(
                                "Could not parse GitHub owner/repo from remote URL: {remote_url}"
                            ),
                        };
                        return;
                    }
                };
                let pr_ref = conductor_core::workflow_ephemeral::PrRef {
                    owner,
                    repo,
                    number: pr_number as u64,
                };
                self.spawn_pr_workflow_in_background(pr_ref, def, inputs);
            }
            WorkflowPickerTarget::Ticket {
                ticket_id,
                ticket_title,
                repo_id,
                repo_path,
                ..
            } => {
                self.spawn_ticket_workflow_in_background(
                    def,
                    ticket_id,
                    repo_id,
                    repo_path,
                    ticket_title,
                    inputs,
                );
            }
            WorkflowPickerTarget::Repo {
                repo_id,
                repo_path,
                repo_name,
            } => {
                self.spawn_repo_workflow_in_background(def, repo_id, repo_path, repo_name, inputs);
            }
            WorkflowPickerTarget::WorkflowRun {
                workflow_run_id,
                worktree_id,
                worktree_path,
                repo_path,
                ..
            } => {
                let mut run_inputs = inputs;
                run_inputs.insert("workflow_run_id".to_string(), workflow_run_id.clone());
                let working_dir = worktree_path.unwrap_or_else(|| repo_path.clone());
                if let Some(wt_id) = worktree_id {
                    self.spawn_workflow_in_background(
                        def,
                        wt_id,
                        working_dir,
                        repo_path,
                        None,
                        run_inputs,
                        format!("workflow_run:{workflow_run_id}"),
                    );
                } else {
                    self.spawn_workflow_run_target_in_background(
                        def,
                        repo_path,
                        run_inputs,
                        workflow_run_id,
                    );
                }
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
                field_type: FormFieldType::Text,
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
                            title: "Unregister Repository".to_string(),
                            message: format!(
                                "This will permanently delete the repo and all associated worktrees, agent runs, and tickets.{}",
                                warning
                            ),
                            expected: repo.slug.clone(),
                            value: String::new(),
                            on_confirm: ConfirmAction::UnregisterRepo {
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
            let Some(bg_tx) = self.bg_tx.clone() else {
                return;
            };
            self.state.modal = Modal::Progress {
                message: "Pushing branch…".to_string(),
            };
            let config = self.config.clone();
            let wt_slug = wt.slug.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<String> {
                    let db = conductor_core::config::db_path();
                    let conn = conductor_core::db::open_database(&db)?;
                    let mgr = WorktreeManager::new(&conn, &config);
                    mgr.push(&repo_slug, &wt_slug).map_err(anyhow::Error::from)
                })();
                let _ = bg_tx.send(Action::PushComplete {
                    result: result.map_err(|e| e.to_string()),
                });
            });
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
            let Some(bg_tx) = self.bg_tx.clone() else {
                return;
            };
            self.state.modal = Modal::Progress {
                message: "Creating PR…".to_string(),
            };
            let config = self.config.clone();
            let wt_slug = wt.slug.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<String> {
                    let db = conductor_core::config::db_path();
                    let conn = conductor_core::db::open_database(&db)?;
                    let mgr = WorktreeManager::new(&conn, &config);
                    mgr.create_pr(&repo_slug, &wt_slug, false)
                        .map_err(anyhow::Error::from)
                })();
                let _ = bg_tx.send(Action::PrCreateComplete {
                    result: result.map_err(|e| e.to_string()),
                });
            });
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

    fn handle_post_create_pick(&mut self, index: usize) {
        // Resolve the selected index while borrowing the modal immutably
        let actual_index = if let Modal::PostCreatePicker {
            ref items,
            selected,
            ..
        } = self.state.modal
        {
            let idx = if index == usize::MAX { selected } else { index };
            if idx >= items.len() {
                return;
            }
            idx
        } else {
            return;
        };

        // Take ownership of the modal to avoid cloning the entire items Vec
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        let (items, worktree_id, worktree_path, worktree_slug, ticket_id, repo_path) =
            if let Modal::PostCreatePicker {
                items,
                worktree_id,
                worktree_path,
                worktree_slug,
                ticket_id,
                repo_path,
                ..
            } = modal
            {
                (
                    items,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                    repo_path,
                )
            } else {
                unreachable!()
            };

        // Use into_iter to take ownership of the selected item without cloning
        let choice = items.into_iter().nth(actual_index).unwrap();

        match choice {
            PostCreateChoice::StartAgent => {
                self.show_agent_prompt_for_ticket(
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                );
            }
            PostCreateChoice::RunWorkflow { def, .. } => {
                let mut prefill = std::collections::HashMap::new();
                prefill.insert("ticket_id".to_string(), ticket_id.clone());
                let target = crate::state::WorkflowPickerTarget::Worktree {
                    worktree_id,
                    worktree_path,
                    repo_path,
                };
                self.show_workflow_inputs_or_run(target, def, prefill);
            }
            PostCreateChoice::Skip => {
                // No-op — modal already dismissed
            }
        }
    }

    // ── Theme picker ───────────────────────────────────────────────────

    fn handle_show_theme_picker(&mut self) {
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot open theme picker: background sender not ready.".into(),
            };
            return;
        };
        // Show a non-blocking progress modal while scanning ~/.conductor/themes/
        // off the TUI main thread, as required by the threading rule in CLAUDE.md.
        self.state.modal = Modal::Progress {
            message: "Loading themes…".into(),
        };
        std::thread::spawn(move || {
            let (all, mut warnings) = crate::theme::all_themes();
            // Pre-load all Theme objects so keypress preview is an in-memory
            // lookup with no file I/O on the TUI main thread.
            // Themes that fail to re-parse are excluded from both lists so the
            // picker never shows an entry with silently-incorrect preview colors.
            let mut themes: Vec<(String, String)> = Vec::new();
            let mut loaded_themes: Vec<crate::theme::Theme> = Vec::new();
            for (name, label) in all {
                match crate::theme::Theme::from_name(&name) {
                    Ok(t) => {
                        themes.push((name, label));
                        loaded_themes.push(t);
                    }
                    Err(e) => warnings.push(e),
                }
            }
            let _ = bg_tx.send(Action::ThemesLoaded {
                themes,
                loaded_themes,
                warnings,
            });
        });
    }

    fn handle_themes_loaded(
        &mut self,
        themes: Vec<(String, String)>,
        loaded_themes: Vec<crate::theme::Theme>,
        warnings: Vec<String>,
    ) {
        let current_name = self
            .config
            .general
            .theme
            .clone()
            .unwrap_or_else(|| "conductor".to_string());
        let selected = themes
            .iter()
            .position(|(name, _)| name == current_name.as_str())
            .unwrap_or(0);
        self.state.modal = Modal::ThemePicker {
            themes,
            loaded_themes,
            selected,
            original_theme: self.state.theme,
            original_name: current_name,
        };
        // Surface any broken theme files as a status warning (non-fatal).
        if !warnings.is_empty() {
            self.state.status_message = Some(format!(
                "Warning: {} theme file(s) failed to parse — check your ~/.conductor/themes/ directory",
                warnings.len()
            ));
        }
    }

    fn handle_theme_preview(&mut self, idx: usize) {
        // Use the pre-loaded Theme objects stored in the modal — no file I/O on
        // the TUI main thread.
        if let Modal::ThemePicker {
            ref loaded_themes,
            ref mut selected,
            ..
        } = self.state.modal
        {
            if let Some(theme) = loaded_themes.get(idx) {
                self.state.theme = *theme;
            }
            *selected = idx;
        }
    }

    fn handle_theme_picker_confirm(&mut self, selected: usize) {
        let name_opt = if let Modal::ThemePicker { ref themes, .. } = self.state.modal {
            themes.get(selected).map(|(n, _)| n.clone())
        } else {
            None
        };
        let Some(name) = name_opt else {
            self.state.modal = Modal::None;
            return;
        };
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot save theme: background sender not ready.".into(),
            };
            return;
        };
        // Update in-memory config immediately (non-blocking).
        self.config.general.theme = Some(name.clone());
        // Write the updated config to disk off the TUI main thread to avoid
        // blocking the render loop.
        let config = self.config.clone();
        self.state.modal = Modal::Progress {
            message: format!("Saving theme \"{name}\"…"),
        };
        std::thread::spawn(move || {
            let result = conductor_core::config::save_config(&config)
                .map(|()| format!("Theme set to \"{name}\""))
                .map_err(|e| e.to_string());
            let _ = bg_tx.send(Action::ThemeSaveComplete { result });
        });
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
                let rows = self.state.dashboard_rows();
                match rows.get(self.state.dashboard_index) {
                    Some(&DashboardRow::Worktree(wt_idx)) => {
                        let Some(wt) = self.state.data.worktrees.get(wt_idx).cloned() else {
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
                    Some(&DashboardRow::Repo(repo_idx)) => {
                        let Some(repo) = self.state.data.repos.get(repo_idx).cloned() else {
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
                    None => (),
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
            Some(&worktree_id),
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
        let args = conductor_core::agent_runtime::build_orchestrate_args(
            &run.id,
            &worktree_path,
            model.as_deref(),
            false,
            None,
        );

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

    fn spawn_worktree_create(
        &mut self,
        repo_slug: String,
        name: String,
        ticket_id: Option<String>,
        from_pr: Option<u32>,
    ) {
        // Guard before setting the non-dismissable Progress modal: if bg_tx is
        // None (only possible before init() completes), skip rather than
        // permanently locking the UI with no recovery path.
        let Some(bg_tx) = self.bg_tx.clone() else {
            return;
        };
        self.state.modal = Modal::Progress {
            message: if from_pr.is_some() {
                "Fetching PR branch…".to_string()
            } else {
                "Creating worktree…".to_string()
            },
        };
        let config = self.config.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let db = conductor_core::config::db_path();
                let conn = conductor_core::db::open_database(&db)?;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                let (wt, warnings) =
                    wt_mgr.create(&repo_slug, &name, None, ticket_id.as_deref(), from_pr)?;
                Ok((wt, warnings))
            })();
            match result {
                Ok((wt, warnings)) => {
                    if !bg_tx.send(Action::WorktreeCreated {
                        wt_id: wt.id,
                        wt_path: wt.path,
                        wt_slug: wt.slug,
                        wt_repo_id: wt.repo_id,
                        warnings,
                        ticket_id,
                    }) {
                        tracing::warn!(
                            "worktree created but bg_tx.send failed; \
                             Progress modal may remain visible until app exit"
                        );
                    }
                }
                Err(e) => {
                    if !bg_tx.send(Action::WorktreeCreateFailed {
                        message: format!("Create failed: {e}"),
                    }) {
                        tracing::warn!(
                            "worktree creation failed and bg_tx.send also failed; \
                             Progress modal may remain visible until app exit"
                        );
                    }
                }
            }
        });
    }

    fn maybe_start_agent_for_worktree(
        &mut self,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
        repo_id: String,
    ) {
        match self.config.general.auto_start_agent {
            AutoStartAgent::Never => return,
            AutoStartAgent::Always => {
                // Skip the picker and go straight to the agent prompt
                self.show_agent_prompt_for_ticket(
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                );
                return;
            }
            AutoStartAgent::Ask => {}
        }

        // Look up the repo path for workflow discovery
        let repo_path = match self
            .state
            .data
            .repos
            .iter()
            .find(|r| r.id == repo_id)
            .map(|r| r.local_path.clone())
        {
            Some(path) => path,
            None => {
                tracing::warn!(
                    "could not find repo with id {repo_id}; \
                     falling back to empty repo_path for workflow discovery"
                );
                String::new()
            }
        };

        // Discover manual workflows in a background thread to avoid blocking the UI
        let bg_tx = self.bg_tx.clone();
        let wt_path = worktree_path.clone();
        let rp = repo_path.clone();
        std::thread::spawn(move || {
            use conductor_core::workflow::{WorkflowManager, WorkflowTrigger};
            let manual_defs: Vec<_> = match WorkflowManager::list_defs(&wt_path, &rp) {
                Ok((defs, _warnings)) => defs
                    .into_iter()
                    .filter(|d| d.trigger == WorkflowTrigger::Manual)
                    .filter(|d| d.targets.iter().any(|t| t == "worktree"))
                    .collect(),
                Err(e) => {
                    tracing::warn!("failed to list workflow defs: {e}");
                    Vec::new()
                }
            };

            let mut items = vec![PostCreateChoice::StartAgent];
            for def in manual_defs {
                items.push(PostCreateChoice::RunWorkflow {
                    name: def.name.clone(),
                    def,
                });
            }
            items.push(PostCreateChoice::Skip);

            if let Some(ref tx) = bg_tx {
                let _ = tx.send(Action::PostCreatePickerReady {
                    items,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    ticket_id,
                    repo_path,
                });
            }
        });
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
            Some(&worktree_id),
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
        let args = match conductor_core::agent_runtime::build_agent_args(
            &run.id,
            &worktree_path,
            &prompt,
            resume_session_id.as_deref(),
            model.as_deref(),
            None,
        ) {
            Ok(a) => a,
            Err(e) => {
                let _ = mgr.update_run_failed(&run.id, &e);
                self.state.modal = Modal::Error { message: e };
                return;
            }
        };

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

        self.copy_text_to_clipboard(code_block);
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

    // ── WorktreeDetail panel copy/open ───────────────────────────────────────

    fn handle_worktree_detail_copy(&mut self) {
        match self.state.worktree_detail_focus {
            WorktreeDetailFocus::LogPanel => {
                self.handle_copy_last_code_block();
            }
            WorktreeDetailFocus::InfoPanel => {
                let wt = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id));
                let Some(wt) = wt else {
                    return;
                };
                let row = self.state.worktree_detail_selected_row;
                let repo_slug = self
                    .state
                    .data
                    .repo_slug_map
                    .get(&wt.repo_id)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string());
                let value = match row {
                    info_row::SLUG => wt.slug.clone(),
                    info_row::REPO => repo_slug,
                    info_row::BRANCH => wt.branch.clone(),
                    info_row::BASE => wt
                        .base_branch
                        .clone()
                        .unwrap_or_else(|| "(repo default)".to_string()),
                    info_row::PATH => wt.path.clone(),
                    info_row::STATUS => wt.status.to_string(),
                    info_row::MODEL => wt.model.clone().unwrap_or_else(|| "(not set)".to_string()),
                    info_row::CREATED => wt.created_at.clone(),
                    info_row::TICKET => {
                        let url = wt
                            .ticket_id
                            .as_ref()
                            .and_then(|tid| self.state.data.ticket_map.get(tid))
                            .map(|t| t.url.clone())
                            .unwrap_or_default();
                        if url.is_empty() {
                            self.state.status_message =
                                Some("No ticket linked to this worktree".to_string());
                            return;
                        }
                        url
                    }
                    _ => {
                        self.state.status_message = Some("Nothing to copy on this row".to_string());
                        return;
                    }
                };
                self.copy_text_to_clipboard(value);
            }
        }
    }

    fn handle_worktree_detail_open(&mut self) {
        if self.state.worktree_detail_focus != WorktreeDetailFocus::InfoPanel {
            return;
        }
        let row = self.state.worktree_detail_selected_row;
        match row {
            info_row::PATH => {
                let Some(path) = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                    .map(|wt| wt.path.clone())
                else {
                    return;
                };
                self.open_terminal_at_path(&path);
            }
            info_row::TICKET => {
                // Ticket row: open the ticket URL in the default browser
                let url = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                    .and_then(|wt| wt.ticket_id.as_ref())
                    .and_then(|tid| self.state.data.ticket_map.get(tid))
                    .map(|t| t.url.clone());
                match url {
                    Some(ref u) if !u.is_empty() => {
                        let u = u.clone();
                        self.open_url(&u, "ticket");
                    }
                    _ => {
                        self.state.status_message =
                            Some("No ticket linked to this worktree".to_string());
                    }
                }
            }
            _ => {
                self.state.status_message =
                    Some("No action for this row (try Path or Ticket row)".to_string());
            }
        }
    }

    fn handle_repo_detail_info_open(&mut self) {
        let row = self.state.repo_detail_info_row;
        let repo = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id));
        let Some(repo) = repo else { return };
        match row {
            repo_info_row::SLUG | repo_info_row::REMOTE => match self.repo_web_url() {
                Some(url) => self.open_url(&url, "repo"),
                None => {
                    self.state.status_message =
                        Some("No GitHub URL found for this repo".to_string());
                }
            },
            repo_info_row::PATH => {
                let path = repo.local_path.clone();
                self.open_terminal_at_path(&path);
            }
            repo_info_row::WORKTREES_DIR => {
                let path = repo.workspace_dir.clone();
                self.open_terminal_at_path(&path);
            }
            _ => {
                self.state.status_message = Some("No action for this row".to_string());
            }
        }
    }

    fn handle_repo_detail_info_copy(&mut self) {
        let row = self.state.repo_detail_info_row;
        let repo = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id));
        let Some(repo) = repo else { return };
        let text = match row {
            repo_info_row::SLUG => repo.slug.clone(),
            repo_info_row::REMOTE => repo.remote_url.clone(),
            repo_info_row::BRANCH => repo.default_branch.clone(),
            repo_info_row::PATH => repo.local_path.clone(),
            repo_info_row::WORKTREES_DIR => repo.workspace_dir.clone(),
            repo_info_row::MODEL => repo
                .model
                .clone()
                .unwrap_or_else(|| "(not set)".to_string()),
            _ => return,
        };
        self.copy_text_to_clipboard(text);
    }

    /// Open a new terminal window/tab at `path`, using the best available method:
    /// 1. Inside tmux → `tmux new-window -c {path}`
    /// 2. TERM_PROGRAM=Apple_Terminal → AppleScript `do script "cd {path}"`
    /// 3. TERM_PROGRAM=iTerm.app → AppleScript create iTerm2 window at path
    /// 4. Fallback → status message with hint
    fn open_terminal_at_path(&mut self, path: &str) {
        // 1. tmux: preferred when the TUI is already running inside a tmux session
        if std::env::var("TMUX").is_ok() {
            match Command::new("tmux")
                .args(["new-window", "-c", path])
                .output()
            {
                Ok(out) if out.status.success() => {
                    self.state.status_message = Some(format!("Opened tmux window at {path}"));
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    self.state.status_message = Some(format!("tmux error: {}", stderr.trim()));
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Failed to open tmux window: {e}"));
                }
            }
            return;
        }

        // 2 & 3. AppleScript for macOS terminal apps.
        // Embed the path via an AppleScript variable so `quoted form of` handles
        // all shell-special characters without manual escaping.
        let term = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let script: Option<String> = match term.as_str() {
            "Apple_Terminal" => Some(format!(
                "set p to \"{path}\"\n\
                 tell application \"Terminal\"\n\
                 \tdo script \"cd \" & quoted form of p\n\
                 \tactivate\n\
                 end tell",
                path = path.replace('\\', "\\\\").replace('"', "\\\"")
            )),
            "iTerm.app" | "iTerm2" => Some(format!(
                "set p to \"{path}\"\n\
                 tell application \"iTerm\"\n\
                 \tactivate\n\
                 \tcreate window with default profile\n\
                 \ttell current session of current window\n\
                 \t\twrite text \"cd \" & quoted form of p\n\
                 \tend tell\n\
                 end tell",
                path = path.replace('\\', "\\\\").replace('"', "\\\"")
            )),
            _ => None,
        };

        if let Some(script) = script {
            match Command::new("osascript").args(["-e", &script]).output() {
                Ok(out) if out.status.success() => {
                    self.state.status_message = Some(format!("Opened {} at {path}", term));
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    self.state.status_message =
                        Some(format!("Failed to open terminal: {}", stderr.trim()));
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Failed to open terminal: {e}"));
                }
            }
        } else {
            // 4. Unknown environment — guide the user
            let hint = if term.is_empty() {
                "Run inside tmux or set TERM_PROGRAM".to_string()
            } else {
                format!("Terminal '{term}' not supported — run inside tmux")
            };
            self.state.status_message = Some(hint);
        }
    }

    /// Copy arbitrary text to the system clipboard via pbcopy/xclip/xsel.
    fn copy_text_to_clipboard(&mut self, text: String) {
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
                    if stdin.write_all(text.as_bytes()).is_err() {
                        self.state.status_message = Some("Clipboard write failed".to_string());
                        return;
                    }
                    drop(stdin);
                }
                // Fire-and-forget: pbcopy/xclip/xsel completes almost instantly.
                drop(child);
                self.state.status_message = Some("Copied to clipboard".to_string());
            }
            Err(_) => {
                self.state.status_message =
                    Some("No clipboard tool found (pbcopy/xclip/xsel)".to_string());
            }
        }
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

        let Some(bg_tx) = self.bg_tx.clone() else {
            return;
        };
        self.state.modal = Modal::Progress {
            message: "Importing repos…".to_string(),
        };
        let config = self.config.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<(usize, Vec<String>)> {
                let db = conductor_core::config::db_path();
                let conn = conductor_core::db::open_database(&db)?;
                let mgr = RepoManager::new(&conn, &config);
                let mut imported = 0usize;
                let mut errors = Vec::new();
                for url in &to_import {
                    let slug = derive_slug_from_url(url);
                    let local_path = derive_local_path(&config, &slug);
                    match mgr.register(&slug, &local_path, url, None) {
                        Ok(_) => imported += 1,
                        Err(e) => errors.push(format!("{slug}: {e}")),
                    }
                }
                Ok((imported, errors))
            })();
            match result {
                Ok((imported, errors)) => {
                    let _ = bg_tx.send(Action::GithubImportComplete { imported, errors });
                }
                Err(e) => {
                    let _ = bg_tx.send(Action::GithubImportComplete {
                        imported: 0,
                        errors: vec![e.to_string()],
                    });
                }
            }
        });
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
        // In RepoDetail with no worktree selected, scope to the selected repo.
        let repo_id = if wt_id.is_none() && self.state.view == View::RepoDetail {
            self.state.selected_repo_id.clone()
        } else {
            None
        };
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
            repo_id,
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
                let (defs, warnings) =
                    WorkflowManager::list_defs(&wt_path, &rp).unwrap_or_default();
                self.state.data.workflow_defs = defs;
                if let Some(msg) = workflow_parse_warning_message(&warnings) {
                    self.state.status_message = Some(msg);
                }
            }
        } else if self.state.view == View::RepoDetail {
            // Repo-scoped: defs cleared here, background poller will repopulate
            self.state.data.workflow_defs.clear();
            self.state.data.workflow_def_slugs.clear();
        } else {
            // Global mode: defs are cross-worktree, cleared here and populated by background poller
            self.state.data.workflow_defs.clear();
            self.state.data.workflow_def_slugs.clear();
        }
        self.state.data.workflow_runs =
            if let Some(ref wt_id) = self.state.selected_worktree_id.clone() {
                wf_mgr.list_workflow_runs(wt_id).unwrap_or_default()
            } else if self.state.view == View::RepoDetail {
                let repo_id = self.state.selected_repo_id.as_deref().unwrap_or("");
                wf_mgr
                    .list_workflow_runs_for_repo(repo_id, 50)
                    .unwrap_or_default()
            } else {
                wf_mgr.list_all_workflow_runs(50).unwrap_or_default()
            };

        // Load steps for the currently selected run
        self.state.init_collapse_state();
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
        let run_len = self.state.visible_workflow_run_rows().len();
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
        if !self.state.selected_step_has_agent() {
            self.state.workflow_run_detail_focus = WorkflowRunDetailFocus::Steps;
        }
    }

    /// Open a workflow picker appropriate for the current context.
    fn handle_pick_workflow(&mut self) {
        use crate::state::WorkflowPickerTarget;

        // Determine the target based on current view/focus
        let target = if self.state.view == View::WorktreeDetail {
            // WorktreeDetail: target is the current worktree
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
            match self.worktree_picker_target(&wt) {
                Some(t) => t,
                None => {
                    self.state.status_message =
                        Some("Repo not found for this worktree".to_string());
                    return;
                }
            }
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == crate::state::RepoDetailFocus::Prs
        {
            // RepoDetail PRs pane: target is the selected PR
            let pr = match self.state.detail_prs.get(self.state.detail_pr_index) {
                Some(pr) => pr.clone(),
                None => {
                    self.state.status_message = Some("No PR selected".to_string());
                    return;
                }
            };
            WorkflowPickerTarget::Pr {
                pr_number: pr.number,
                pr_title: pr.title.clone(),
            }
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == crate::state::RepoDetailFocus::Info
        {
            // RepoDetail Info pane: target is the current repo
            let repo = match self
                .state
                .selected_repo_id
                .as_ref()
                .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
            {
                Some(r) => r.clone(),
                None => {
                    self.state.status_message = Some("No repo selected".to_string());
                    return;
                }
            };
            self.repo_picker_target(&repo)
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == crate::state::RepoDetailFocus::Worktrees
        {
            // RepoDetail Worktrees pane: target is the highlighted worktree
            let wt = match self.state.detail_worktrees.get(self.state.detail_wt_index) {
                Some(w) => w.clone(),
                None => {
                    self.state.status_message = Some("No worktree selected".to_string());
                    return;
                }
            };
            match self.worktree_picker_target(&wt) {
                Some(t) => t,
                None => {
                    self.state.status_message =
                        Some("Repo not found for this worktree".to_string());
                    return;
                }
            }
        } else if self.state.view == View::Dashboard {
            let rows = self.state.dashboard_rows();
            match rows.get(self.state.dashboard_index) {
                Some(&DashboardRow::Repo(repo_idx)) => {
                    let repo = match self.state.data.repos.get(repo_idx) {
                        Some(r) => r.clone(),
                        None => {
                            self.state.status_message = Some("No repo selected".to_string());
                            return;
                        }
                    };
                    self.repo_picker_target(&repo)
                }
                Some(&DashboardRow::Worktree(wt_idx)) => {
                    let wt = match self.state.data.worktrees.get(wt_idx) {
                        Some(w) => w.clone(),
                        None => {
                            self.state.status_message = Some("No worktree selected".to_string());
                            return;
                        }
                    };
                    match self.worktree_picker_target(&wt) {
                        Some(t) => t,
                        None => {
                            self.state.status_message =
                                Some("Repo not found for this worktree".to_string());
                            return;
                        }
                    }
                }
                None => {
                    self.state.status_message = Some("No item selected".to_string());
                    return;
                }
            }
        } else if self.state.view == View::WorkflowRunDetail {
            // WorkflowRunDetail: target is the current workflow run
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
            // Resolve repo_path from worktree or repo_id
            let repo_path = if let Some(wt_id) = &run.worktree_id {
                self.state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| &w.id == wt_id)
                    .and_then(|wt| {
                        self.state
                            .data
                            .repos
                            .iter()
                            .find(|r| r.id == wt.repo_id)
                            .map(|r| r.local_path.clone())
                    })
            } else if let Some(repo_id) = &run.repo_id {
                self.state
                    .data
                    .repos
                    .iter()
                    .find(|r| &r.id == repo_id)
                    .map(|r| r.local_path.clone())
            } else {
                None
            };
            let repo_path = match repo_path {
                Some(p) => p,
                None => {
                    self.state.status_message =
                        Some("Cannot determine repo for this workflow run".to_string());
                    return;
                }
            };
            let worktree_path = run.worktree_id.as_ref().and_then(|wt_id| {
                self.state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| &w.id == wt_id)
                    .map(|w| w.path.clone())
            });
            WorkflowPickerTarget::WorkflowRun {
                workflow_run_id: run.id.clone(),
                workflow_name: run.workflow_name.clone(),
                worktree_id: run.worktree_id.clone(),
                worktree_path,
                repo_path,
            }
        } else {
            // Ticket list contexts: target is the selected ticket itself (RepoDetail only)
            let ticket = match self.state.view {
                View::RepoDetail
                    if self.state.repo_detail_focus == crate::state::RepoDetailFocus::Tickets =>
                {
                    self.state
                        .filtered_detail_tickets
                        .get(self.state.detail_ticket_index)
                }
                _ => {
                    self.state.status_message = Some("No ticket selected".to_string());
                    return;
                }
            };
            let ticket = match ticket {
                Some(t) => t.clone(),
                None => {
                    self.state.status_message = Some("No ticket selected".to_string());
                    return;
                }
            };
            let repo = self
                .state
                .data
                .repos
                .iter()
                .find(|r| r.id == ticket.repo_id);
            let repo_path = match repo {
                Some(r) => r.local_path.clone(),
                None => {
                    self.state.modal = Modal::Error {
                        message: "Cannot run workflow: ticket's repository is not registered in Conductor.".to_string(),
                    };
                    return;
                }
            };
            WorkflowPickerTarget::Ticket {
                ticket_id: ticket.id.clone(),
                ticket_title: ticket.title.clone(),
                ticket_url: ticket.url.clone(),
                repo_path,
                repo_id: ticket.repo_id.clone(),
            }
        };

        // Filter workflow defs based on target type
        let defs: Vec<conductor_core::workflow::WorkflowDef> = match &target {
            WorkflowPickerTarget::Pr { .. } => self
                .state
                .data
                .workflow_defs
                .iter()
                .filter(|d| d.targets.iter().any(|t| t == "pr"))
                .cloned()
                .collect(),
            WorkflowPickerTarget::Worktree { .. } => self
                .state
                .data
                .workflow_defs
                .iter()
                .filter(|d| d.targets.iter().any(|t| t == "worktree"))
                .cloned()
                .collect(),
            WorkflowPickerTarget::Ticket { repo_path, .. } => {
                conductor_core::workflow::WorkflowManager::list_defs("", repo_path)
                    .unwrap_or_default()
                    .0
                    .into_iter()
                    .filter(|d| d.targets.iter().any(|t| t == "ticket"))
                    .collect()
            }
            WorkflowPickerTarget::Repo { repo_path, .. } => {
                conductor_core::workflow::WorkflowManager::list_defs("", repo_path)
                    .unwrap_or_default()
                    .0
                    .into_iter()
                    .filter(|d| d.targets.iter().any(|t| t == "repo"))
                    .collect()
            }
            WorkflowPickerTarget::WorkflowRun { repo_path, .. } => {
                conductor_core::workflow::WorkflowManager::list_defs("", repo_path)
                    .unwrap_or_default()
                    .0
                    .into_iter()
                    .filter(|d| d.targets.iter().any(|t| t == "workflow_run"))
                    .collect()
            }
        };

        if defs.is_empty() {
            let kind = match &target {
                WorkflowPickerTarget::Pr { .. } => "PR",
                WorkflowPickerTarget::Worktree { .. } => "worktree",
                WorkflowPickerTarget::Ticket { .. } => "ticket",
                WorkflowPickerTarget::Repo { .. } => "repo",
                WorkflowPickerTarget::WorkflowRun { .. } => "workflow_run",
            };
            self.state.modal = Modal::Error {
                message: format!(
                    "No {kind}-compatible workflows found.\nAdd targets: [{kind}] to a workflow definition."
                ),
            };
            return;
        }

        self.state.modal = Modal::WorkflowPicker {
            target,
            workflow_defs: defs,
            selected: 0,
        };
    }

    /// Confirm the workflow selection from the generic WorkflowPicker modal.
    fn handle_workflow_picker_confirm(&mut self) {
        let (target, def) = if let Modal::WorkflowPicker {
            ref target,
            ref workflow_defs,
            selected,
            ..
        } = self.state.modal
        {
            let def = match workflow_defs.get(selected) {
                Some(d) => d.clone(),
                None => return,
            };
            (target.clone(), def)
        } else {
            return;
        };

        self.state.modal = Modal::None;

        self.show_workflow_inputs_or_run(target, def, std::collections::HashMap::new());
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

        // Block if a workflow run is already active on this worktree
        {
            use conductor_core::workflow::WorkflowManager;
            let wf_mgr = WorkflowManager::new(&self.conn);
            match wf_mgr.get_active_run_for_worktree(&wt.id) {
                Ok(Some(active)) => {
                    self.state.status_message = Some(format!(
                        "Workflow '{}' is already running — cancel it before starting another",
                        active.workflow_name
                    ));
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    self.state.status_message =
                        Some(format!("Failed to check active workflow run: {e}"));
                    return;
                }
            }
        }

        let Some(target) = self.worktree_picker_target(&wt) else {
            self.state.status_message = Some("Repo not found for worktree".to_string());
            return;
        };
        self.show_workflow_inputs_or_run(target, def, std::collections::HashMap::new());
    }

    /// Spawn a workflow execution in a background thread, reporting result via bg_tx.
    #[allow(clippy::too_many_arguments)]
    fn spawn_workflow_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        worktree_id: String,
        worktree_path: String,
        repo_path: String,
        ticket_id: Option<String>,
        inputs: std::collections::HashMap<String, String>,
        target_label: String,
    ) {
        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let shutdown = Arc::clone(&self.workflow_shutdown);

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let params = WorkflowExecStandalone {
                config,
                workflow: def.clone(),
                worktree_id: Some(worktree_id),
                working_dir: worktree_path,
                repo_path,
                ticket_id,
                repo_id: None,
                model: None,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(target_label),
                run_id_notify: None,
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!("Starting workflow '{workflow_name}'…"));
    }

    fn spawn_ticket_workflow_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        ticket_id: String,
        repo_id: String,
        repo_path: String,
        target_label: String,
        inputs: std::collections::HashMap<String, String>,
    ) {
        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let shutdown = Arc::clone(&self.workflow_shutdown);

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let working_dir = repo_path.clone();

            let params = WorkflowExecStandalone {
                config,
                workflow: def.clone(),
                worktree_id: None,
                working_dir,
                repo_path,
                ticket_id: Some(ticket_id),
                repo_id: Some(repo_id),
                model: None,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(target_label),
                run_id_notify: None,
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!("Starting workflow '{workflow_name}' on ticket…"));
    }

    fn spawn_repo_workflow_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        repo_id: String,
        repo_path: String,
        repo_name: String,
        inputs: std::collections::HashMap<String, String>,
    ) {
        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let shutdown = Arc::clone(&self.workflow_shutdown);

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let params = WorkflowExecStandalone {
                config,
                workflow: def.clone(),
                worktree_id: None,
                working_dir: repo_path.clone(),
                repo_path,
                ticket_id: None,
                repo_id: Some(repo_id),
                model: None,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(repo_name),
                run_id_notify: None,
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!("Starting workflow '{workflow_name}' on repo…"));
    }

    fn spawn_workflow_run_target_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        repo_path: String,
        inputs: std::collections::HashMap<String, String>,
        target_label: String,
    ) {
        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let shutdown = Arc::clone(&self.workflow_shutdown);

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let params = WorkflowExecStandalone {
                config,
                workflow: def.clone(),
                worktree_id: None,
                working_dir: repo_path.clone(),
                repo_path,
                ticket_id: None,
                repo_id: None,
                model: None,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(target_label),
                run_id_notify: None,
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!(
            "Starting workflow '{workflow_name}' on workflow run…"
        ));
    }

    fn handle_run_pr_workflow(&mut self) {
        let pr = match self.state.detail_prs.get(self.state.detail_pr_index) {
            Some(pr) => pr.clone(),
            None => {
                self.state.status_message = Some("No PR selected".to_string());
                return;
            }
        };

        // Load defs directly from the selected repo's local path so the PR workflow
        // picker works even when no worktrees are registered (the background poller
        // only scans worktrees, leaving workflow_defs empty in that case).
        let repo_local_path = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
            .map(|r| r.local_path.clone());

        let all_defs: Vec<conductor_core::workflow::WorkflowDef> =
            if let Some(ref rp) = repo_local_path {
                conductor_core::workflow::WorkflowManager::list_defs(rp, rp)
                    .unwrap_or_default()
                    .0
            } else {
                self.state.data.workflow_defs.clone()
            };

        let pr_defs: Vec<conductor_core::workflow::WorkflowDef> = all_defs
            .into_iter()
            .filter(|d| d.targets.iter().any(|t| t == "pr"))
            .collect();

        if pr_defs.is_empty() {
            self.state.modal = Modal::Error {
                message:
                    "No PR-compatible workflows found.\nAdd targets: [pr] to a workflow definition."
                        .to_string(),
            };
            return;
        }

        self.state.modal = Modal::PrWorkflowPicker {
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            workflow_defs: pr_defs,
            selected: 0,
        };
    }

    fn handle_pr_workflow_picker_confirm(&mut self) {
        use crate::state::WorkflowPickerTarget;

        let (pr_number, def) = if let Modal::PrWorkflowPicker {
            pr_number,
            ref workflow_defs,
            selected,
            ..
        } = self.state.modal
        {
            let def = match workflow_defs.get(selected) {
                Some(d) => d.clone(),
                None => return,
            };
            (pr_number, def)
        } else {
            return;
        };

        self.state.modal = Modal::None;

        let target = WorkflowPickerTarget::Pr {
            pr_number,
            pr_title: String::new(),
        };
        self.show_workflow_inputs_or_run(target, def, std::collections::HashMap::new());
    }

    /// Spawn an ephemeral PR workflow execution in a background thread.
    fn spawn_pr_workflow_in_background(
        &mut self,
        pr_ref: conductor_core::workflow_ephemeral::PrRef,
        def: conductor_core::workflow::WorkflowDef,
        inputs: std::collections::HashMap<String, String>,
    ) {
        use conductor_core::config::db_path;
        use conductor_core::db::open_database;

        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let pr_label = format!("{}#{}", pr_ref.repo_slug(), pr_ref.number);
        let shutdown = Arc::clone(&self.workflow_shutdown);

        self.state.status_message = Some(format!(
            "Starting workflow '{workflow_name}' on {pr_label}…"
        ));

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{WorkflowExecConfig, WorkflowResult};
            use conductor_core::workflow_ephemeral::run_workflow_on_pr;

            let db = db_path();
            let conn = match open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    if let Some(ref tx) = bg_tx {
                        let _ = tx.send(Action::BackgroundError {
                            message: format!("Failed to open database: {e}"),
                        });
                    }
                    return;
                }
            };

            let exec_config = WorkflowExecConfig {
                shutdown: Some(shutdown),
                ..WorkflowExecConfig::default()
            };

            let result = run_workflow_on_pr(
                &conn,
                &config,
                &pr_ref,
                &def.name,
                None,
                exec_config,
                inputs,
                false,
            );

            if let Some(ref tx) = bg_tx {
                match result {
                    Ok(WorkflowResult { all_succeeded, .. }) => {
                        let msg = if all_succeeded {
                            format!(
                                "Workflow '{workflow_name}' on {pr_label} completed successfully"
                            )
                        } else {
                            format!(
                                "Workflow '{workflow_name}' on {pr_label} completed with failures"
                            )
                        };
                        let _ = tx.send(Action::BackgroundSuccess { message: msg });
                    }
                    Err(e) => {
                        let _ = tx.send(Action::BackgroundError {
                            message: format!(
                                "Workflow '{workflow_name}' on {pr_label} failed: {e}"
                            ),
                        });
                    }
                }
            }
        });

        self.workflow_threads.push(handle);
    }

    fn handle_resume_workflow(&mut self) {
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

        if let Err(e) =
            conductor_core::workflow::validate_resume_preconditions(&run.status, false, None)
        {
            self.state.status_message = Some(e.to_string());
            return;
        }

        self.state.modal = Modal::Confirm {
            title: "Resume Workflow".to_string(),
            message: format!("Resume workflow run '{}'?", run.workflow_name),
            on_confirm: ConfirmAction::ResumeWorkflow {
                workflow_run_id: run.id.clone(),
            },
        };
    }

    fn handle_resume_worktree_workflow(&mut self) {
        let worktree_id = match self.state.selected_worktree_id.as_deref() {
            Some(id) => id.to_string(),
            None => return,
        };

        let run = match self
            .state
            .data
            .latest_workflow_runs_by_worktree
            .get(&worktree_id)
            .cloned()
        {
            Some(r) => r,
            None => {
                self.state.status_message = Some("No workflow runs for this worktree".to_string());
                return;
            }
        };

        if let Err(e) =
            conductor_core::workflow::validate_resume_preconditions(&run.status, false, None)
        {
            self.state.status_message = Some(format!("Cannot resume: {e}"));
            return;
        }

        self.state.modal = Modal::Confirm {
            title: "Resume Workflow".to_string(),
            message: format!("Resume workflow run '{}'?", run.workflow_name),
            on_confirm: ConfirmAction::ResumeWorkflow {
                workflow_run_id: run.id,
            },
        };
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
            match wf_mgr.reject_gate(step_id, "tui-user", None) {
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
        let effective_path = if std::path::Path::new(&wt.path).exists() {
            wt.path.clone()
        } else {
            repo_path.clone()
        };
        Some((effective_path, repo_path))
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

    /// Resolve a worktree to a `WorkflowPickerTarget::Worktree`, looking up the repo path from
    /// `self.state.data.repos`. Returns `None` if the repo is not found.
    fn worktree_picker_target(
        &self,
        wt: &conductor_core::worktree::Worktree,
    ) -> Option<crate::state::WorkflowPickerTarget> {
        let repo_path = self
            .state
            .data
            .repos
            .iter()
            .find(|r| r.id == wt.repo_id)
            .map(|r| r.local_path.clone())?;
        Some(crate::state::WorkflowPickerTarget::Worktree {
            worktree_id: wt.id.clone(),
            worktree_path: wt.path.clone(),
            repo_path,
        })
    }

    /// Construct a `WorkflowPickerTarget::Repo` from a `&Repo`.
    fn repo_picker_target(
        &self,
        repo: &conductor_core::repo::Repo,
    ) -> crate::state::WorkflowPickerTarget {
        crate::state::WorkflowPickerTarget::Repo {
            repo_id: repo.id.clone(),
            repo_path: repo.local_path.clone(),
            repo_name: repo.slug.clone(),
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

    fn make_test_app() -> App {
        let conn = conductor_core::test_helpers::create_test_conn();
        App::new(
            conn,
            conductor_core::config::Config::default(),
            crate::theme::Theme::default(),
        )
    }

    fn make_test_run(id: &str) -> conductor_core::workflow::WorkflowRun {
        conductor_core::workflow::WorkflowRun {
            id: id.into(),
            workflow_name: "test".into(),
            worktree_id: Some("w1".into()),
            parent_run_id: String::new(),
            status: conductor_core::workflow::WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
        }
    }

    #[test]
    fn test_toggle_workflow_column_off_moves_focus_to_content() {
        let mut app = make_test_app();
        app.state.workflow_column_visible = true;
        app.state.column_focus = crate::state::ColumnFocus::Workflow;
        app.handle_action(Action::ToggleWorkflowColumn);
        assert!(!app.state.workflow_column_visible);
        assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Content);
    }

    #[test]
    fn test_toggle_workflow_column_on_preserves_focus() {
        let mut app = make_test_app();
        app.state.workflow_column_visible = false;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.handle_action(Action::ToggleWorkflowColumn);
        assert!(app.state.workflow_column_visible);
        assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Content);
    }

    #[test]
    fn test_workflow_column_select_run_enters_detail_view() {
        let mut app = make_test_app();
        app.state.selected_worktree_id = Some("w1".into());
        app.state.data.workflow_runs = vec![make_test_run("run1")];
        app.state.column_focus = crate::state::ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        app.state.workflow_run_index = 0;
        app.handle_action(Action::Select);
        assert_eq!(app.state.view, View::WorkflowRunDetail);
        assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Content);
        assert_eq!(app.state.selected_workflow_run_id.as_deref(), Some("run1"));
    }

    #[test]
    fn test_workflow_column_select_header_row_is_noop() {
        // Global mode (selected_worktree_id = None): first visible row is a group header.
        // Pressing Enter on a header should be a no-op.
        let mut app = make_test_app();
        let mut run = make_test_run("run1");
        run.worktree_id = None;
        app.state.data.workflow_runs = vec![run];
        app.state.column_focus = crate::state::ColumnFocus::Workflow;
        app.state.workflows_focus = WorkflowsFocus::Runs;
        app.state.workflow_run_index = 0; // points at repo/target header in global mode
        app.handle_action(Action::Select);
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_workflow_run_id.is_none());
    }

    #[test]
    fn test_back_from_workflow_run_detail_restores_workflow_column_focus() {
        let mut app = make_test_app();
        app.state.view = View::WorkflowRunDetail;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.state.selected_workflow_run_id = Some("run1".into());
        app.handle_action(Action::Back);
        assert_eq!(app.state.view, View::Dashboard);
        assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Workflow);
        assert_eq!(app.state.workflows_focus, WorkflowsFocus::Runs);
        assert!(app.state.selected_workflow_run_id.is_none());
    }

    #[test]
    fn test_back_from_workflow_run_detail_restores_previous_view() {
        let mut app = make_test_app();
        app.state.view = View::WorkflowRunDetail;
        app.state.previous_view = Some(View::RepoDetail);
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.handle_action(Action::Back);
        assert_eq!(app.state.view, View::RepoDetail);
        assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Workflow);
        assert!(app.state.selected_workflow_run_id.is_none());
        assert!(app.state.previous_view.is_none());
    }

    #[test]
    fn test_focus_workflow_column_ignored_when_hidden() {
        let mut state = crate::state::AppState::new();
        state.workflow_column_visible = false;
        state.column_focus = crate::state::ColumnFocus::Content;
        // FocusWorkflowColumn should be a no-op when column is hidden
        if state.workflow_column_visible {
            state.column_focus = crate::state::ColumnFocus::Workflow;
        }
        assert_eq!(state.column_focus, crate::state::ColumnFocus::Content);
    }

    #[test]
    fn test_focus_workflow_column_allowed_when_visible() {
        let mut state = crate::state::AppState::new();
        state.workflow_column_visible = true;
        state.column_focus = crate::state::ColumnFocus::Content;
        if state.workflow_column_visible {
            state.column_focus = crate::state::ColumnFocus::Workflow;
        }
        assert_eq!(state.column_focus, crate::state::ColumnFocus::Workflow);
    }
}

#[cfg(test)]
mod action_handler_tests {
    use super::*;

    fn make_app() -> App {
        let conn = conductor_core::db::open_database(std::path::Path::new(":memory:")).unwrap();
        App::new(conn, Config::default(), Theme::default())
    }

    // Action::Quit with an open modal immediately sets should_quit = true
    // (bypasses the confirm dialog which only shows when modal is None).
    #[test]
    fn quit_sets_should_quit() {
        let mut app = make_app();
        app.state.modal = Modal::Help;
        app.update(Action::Quit);
        assert!(app.state.should_quit);
    }

    #[test]
    fn help_modal_opens_and_dismisses() {
        let mut app = make_app();
        assert!(matches!(app.state.modal, Modal::None));

        app.update(Action::ShowHelp);
        assert!(matches!(app.state.modal, Modal::Help));

        app.update(Action::DismissModal);
        assert!(matches!(app.state.modal, Modal::None));
    }

    #[test]
    fn filter_state_lifecycle() {
        let mut app = make_app();

        // Enter filter mode
        app.update(Action::EnterFilter);
        assert!(app.state.filter.active);
        assert!(app.state.filter.text.is_empty());

        // Type two chars
        app.update(Action::FilterChar('f'));
        app.update(Action::FilterChar('o'));
        assert_eq!(app.state.filter.text, "fo");

        // Backspace removes one char
        app.update(Action::FilterBackspace);
        assert_eq!(app.state.filter.text, "f");

        // Exit clears active flag (text is preserved until next Enter)
        app.update(Action::ExitFilter);
        assert!(!app.state.filter.active);
    }

    #[test]
    fn worktree_created_action_updates_status() {
        let mut app = make_app();
        app.update(Action::WorktreeCreated {
            wt_id: "01TEST".to_string(),
            wt_path: "/tmp/my-wt".to_string(),
            wt_slug: "my-wt".to_string(),
            wt_repo_id: "01REPO".to_string(),
            warnings: vec![],
            ticket_id: None,
        });
        assert!(matches!(app.state.modal, Modal::None));
        assert!(app.state.status_message.is_some());
        let msg = app.state.status_message.as_deref().unwrap();
        assert!(msg.contains("my-wt"), "expected wt slug in message: {msg}");
    }

    #[test]
    fn data_refreshed_updates_repos() {
        let mut app = make_app();
        assert!(app.state.data.repos.is_empty());

        let repos = vec![
            conductor_core::repo::Repo {
                id: "01AAA".to_string(),
                slug: "repo-a".to_string(),
                local_path: "/tmp/repo-a".to_string(),
                remote_url: "https://github.com/x/a".to_string(),
                default_branch: "main".to_string(),
                workspace_dir: "/tmp".to_string(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
                model: None,
                allow_agent_issue_creation: false,
            },
            conductor_core::repo::Repo {
                id: "01BBB".to_string(),
                slug: "repo-b".to_string(),
                local_path: "/tmp/repo-b".to_string(),
                remote_url: "https://github.com/x/b".to_string(),
                default_branch: "main".to_string(),
                workspace_dir: "/tmp".to_string(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
                model: None,
                allow_agent_issue_creation: false,
            },
        ];

        app.update(Action::DataRefreshed(Box::new(
            crate::action::DataRefreshedPayload {
                repos,
                worktrees: vec![],
                tickets: vec![],
                ticket_labels: std::collections::HashMap::new(),
                latest_agent_runs: std::collections::HashMap::new(),
                ticket_agent_totals: std::collections::HashMap::new(),
                latest_workflow_runs_by_worktree: std::collections::HashMap::new(),
                workflow_step_summaries: std::collections::HashMap::new(),
                active_non_worktree_workflow_runs: vec![],
                pending_feedback_requests: vec![],
                waiting_gate_steps: vec![],
            },
        )));

        assert_eq!(app.state.data.repos.len(), 2);
    }

    #[test]
    fn confirm_no_clears_modal_without_side_effect() {
        let mut app = make_app();
        app.state.modal = Modal::Confirm {
            title: "Delete?".to_string(),
            message: "Are you sure?".to_string(),
            on_confirm: crate::state::ConfirmAction::Quit,
        };
        app.update(Action::ConfirmNo);
        assert!(matches!(app.state.modal, Modal::None));
        assert!(
            !app.state.should_quit,
            "ConfirmNo must not trigger the action"
        );
    }
}
