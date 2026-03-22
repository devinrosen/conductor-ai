use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use rusqlite::Connection;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::workflow::parse_workflow_str;
use conductor_core::worktree::WorktreeManager;

use crate::action::Action;
use crate::background;
use crate::event::{BackgroundSender, EventLoop};
use crate::input;
use crate::state::{AppState, Modal, View, WorkflowDefFocus};
use crate::theme::Theme;
use crate::ui;

mod agent_events;
mod agent_execution;
mod crud_operations;
mod git_operations;
mod github_discovery;
mod helpers;
mod info_pane;
mod input_handling;
mod modal_dialog;
mod navigation;
mod theme_management;
mod url_operations;
mod workflow_management;

use helpers::{collapse_loop_iterations, max_scroll, workflow_parse_warning_message};

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
            Action::ShowNotifications => {
                // Load recent notifications from DB off-thread to avoid blocking.
                if let Some(ref bg_tx) = self.bg_tx {
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        use conductor_core::config::db_path;
                        use conductor_core::db::open_database;
                        use conductor_core::notification_manager::NotificationManager;
                        match open_database(&db_path()) {
                            Ok(conn) => {
                                let mgr = NotificationManager::new(&conn);
                                let notifications = mgr.list_recent(50, 0).unwrap_or_default();
                                // Mark all as read when viewing
                                let _ = mgr.mark_all_read();
                                let _ = tx.send(Action::NotificationsLoaded { notifications });
                            }
                            Err(e) => {
                                let _ = tx.send(Action::NotificationsLoaded {
                                    notifications: vec![],
                                });
                                tracing::warn!("failed to open database for notifications: {e}");
                            }
                        }
                    });
                    self.state.modal = Modal::Progress {
                        message: "Loading notifications\u{2026}".into(),
                    };
                }
            }
            Action::NotificationsLoaded { notifications } => {
                self.state.unread_notification_count = 0;
                self.state.modal = Modal::Notifications {
                    notifications,
                    selected: 0,
                };
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
            Action::FeatureBranchesLoaded {
                repo_slug,
                wt_name,
                ticket_id,
                items,
            } => {
                self.handle_feature_branches_loaded(repo_slug, wt_name, ticket_id, items);
            }
            Action::FeatureBranchesFailed { error } => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to load feature branches: {error}"),
                };
            }
            Action::SelectBranch(index) => self.handle_branch_pick(index),
            Action::SelectWorkflowItem(index) => {
                if let Modal::WorkflowPicker {
                    ref mut selected, ..
                } = self.state.modal
                {
                    *selected = index;
                }
                self.handle_workflow_picker_confirm();
            }
            Action::WorkflowPickerDefsLoaded {
                target,
                defs,
                error,
            } => {
                self.handle_workflow_picker_defs_loaded(target, defs, error);
            }
            Action::WorkflowDefsReloaded { defs, warnings } => {
                self.handle_workflow_defs_reloaded(defs, warnings);
            }
            Action::PostCreatePickerReady {
                items,
                worktree_id,
                worktree_path,
                worktree_slug,
                ticket_id,
                repo_path,
            } => {
                self.state.modal = Modal::WorkflowPicker {
                    target: crate::state::WorkflowPickerTarget::PostCreate {
                        worktree_id,
                        worktree_path,
                        worktree_slug,
                        ticket_id,
                        repo_path,
                    },
                    items,
                    selected: 0,
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

            // Base branch change
            Action::SetBaseBranch => self.handle_set_base_branch(),
            Action::BaseBranchesLoaded {
                repo_slug,
                wt_slug,
                items,
            } => {
                self.handle_base_branches_loaded(repo_slug, wt_slug, items);
            }
            Action::BaseBranchesFailed { error } => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to load branches: {error}"),
                };
            }
            Action::SelectBaseBranch(index) => self.handle_base_branch_pick(index),

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
            Action::WorkflowRunDetailCopy => self.handle_workflow_run_detail_copy(),

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
                        match parse_workflow_str(snapshot, "") {
                            Ok(def) => Some((run.id.clone(), def.inputs)),
                            Err(e) => {
                                tracing::warn!(
                                    run_id = %run.id,
                                    "failed to parse workflow definition snapshot: {e}"
                                );
                                None
                            }
                        }
                    })
                    .collect();
                self.state.data.workflow_runs = payload.workflow_runs;
                self.state.data.workflow_steps = collapse_loop_iterations(payload.workflow_steps);
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

            Action::ToggleWorkflowDefsCollapse => {
                self.state.workflow_defs_collapsed = !self.state.workflow_defs_collapsed;
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
                self.state.data.live_turns_by_worktree = payload.live_turns_by_worktree;
                self.state.data.features_by_repo = payload.features_by_repo;
                self.state.unread_notification_count = payload.unread_notification_count;
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
            Action::SetRepoModelComplete { slug, result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(model) => {
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

    fn rebuild_detail_gates(&mut self) {
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

    fn reload_agent_events(&mut self) {
        use conductor_core::agent::{parse_agent_log, AgentManager, AgentRunEvent};

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
}

#[cfg(test)]
mod tests {
    use super::agent_events::extract_last_code_block;
    use super::helpers::{
        clamp_increment, collapse_loop_iterations, wrap_decrement, wrap_increment,
    };
    use super::*;
    use crate::state::WorkflowsFocus;
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
            iteration: 0,
            blocked_on: None,
            feature_id: None,
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

    fn make_step(
        step_name: &str,
        iteration: i64,
        position: i64,
    ) -> conductor_core::workflow::WorkflowRunStep {
        crate::state::tests::make_iter_step("run1", step_name, iteration, position)
    }

    #[test]
    fn collapse_loop_iterations_single_iteration_passthrough() {
        let steps = vec![make_step("a", 0, 0), make_step("b", 0, 1)];
        let result = collapse_loop_iterations(steps);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|s| s.iteration == 0));
    }

    #[test]
    fn collapse_loop_iterations_keeps_latest_per_step_name() {
        // "a" appears in iterations 0, 1, 2 — only 2 should survive.
        // "b" appears only in iteration 0 — should survive.
        let steps = vec![
            make_step("a", 0, 0),
            make_step("b", 0, 1),
            make_step("a", 1, 0),
            make_step("a", 2, 0),
        ];
        let result = collapse_loop_iterations(steps);
        // Should keep "a" at iter 2 and "b" at iter 0.
        assert_eq!(result.len(), 2);
        let a = result.iter().find(|s| s.step_name == "a").unwrap();
        assert_eq!(a.iteration, 2);
        let b = result.iter().find(|s| s.step_name == "b").unwrap();
        assert_eq!(b.iteration, 0);
    }

    #[test]
    fn collapse_loop_iterations_empty_input() {
        let result = collapse_loop_iterations(vec![]);
        assert!(result.is_empty());
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
    use super::helpers::advance_form_field;
    use super::*;
    use crate::state::FormField;

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
                live_turns_by_worktree: std::collections::HashMap::new(),
                features_by_repo: std::collections::HashMap::new(),
                unread_notification_count: 0,
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

    #[test]
    fn workflow_data_refreshed_populates_declared_inputs() {
        let mut app = make_app();
        assert!(app.state.data.workflow_run_declared_inputs.is_empty());

        // A minimal workflow DSL snapshot that declares one required input.
        let snapshot = r#"
workflow my-wf {
    meta { trigger = "manual" targets = ["worktree"] }
    inputs {
        pr_url required
    }
    call agent
}
"#;

        let mut run = conductor_core::workflow::WorkflowRun {
            id: "run-abc".to_string(),
            workflow_name: "my-wf".to_string(),
            worktree_id: None,
            parent_run_id: String::new(),
            status: conductor_core::workflow::WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: Some(snapshot.to_string()),
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            feature_id: None,
        };
        run.inputs
            .insert("pr_url".to_string(), "https://example.com".to_string());

        app.update(Action::WorkflowDataRefreshed(Box::new(
            crate::action::WorkflowDataPayload {
                workflow_defs: None,
                workflow_def_slugs: None,
                workflow_runs: vec![run],
                workflow_steps: vec![],
                step_agent_events: vec![],
                step_agent_run: None,
                workflow_parse_warnings: vec![],
                all_run_steps: std::collections::HashMap::new(),
            },
        )));

        let decls = app
            .state
            .data
            .workflow_run_declared_inputs
            .get("run-abc")
            .expect("declared inputs should be populated for run-abc");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "pr_url");
        assert!(decls[0].required);
    }

    fn make_field(readonly: bool) -> FormField {
        FormField {
            label: String::new(),
            value: String::new(),
            placeholder: String::new(),
            manually_edited: false,
            required: false,
            readonly,
            field_type: crate::state::FormFieldType::Text,
        }
    }

    #[test]
    fn test_advance_form_field_forward_skips_readonly() {
        // [editable, readonly, editable] — from 0 forward should land on 2
        let fields = vec![make_field(false), make_field(true), make_field(false)];
        assert_eq!(advance_form_field(&fields, 0, true), Some(2));
    }

    #[test]
    fn test_advance_form_field_backward_skips_readonly() {
        // [editable, readonly, editable] — from 2 backward should land on 0
        let fields = vec![make_field(false), make_field(true), make_field(false)];
        assert_eq!(advance_form_field(&fields, 2, false), Some(0));
    }

    #[test]
    fn test_advance_form_field_wraps_forward() {
        // [editable, editable, editable] — from last position wraps to 0
        let fields = vec![make_field(false), make_field(false), make_field(false)];
        assert_eq!(advance_form_field(&fields, 2, true), Some(0));
    }

    #[test]
    fn test_advance_form_field_wraps_backward() {
        // [editable, editable] — from 0 backward wraps to last
        let fields = vec![make_field(false), make_field(false)];
        assert_eq!(advance_form_field(&fields, 0, false), Some(1));
    }

    #[test]
    fn test_advance_form_field_all_readonly_returns_none() {
        let fields = vec![make_field(true), make_field(true), make_field(true)];
        assert_eq!(advance_form_field(&fields, 0, true), None);
        assert_eq!(advance_form_field(&fields, 0, false), None);
    }

    #[test]
    fn test_advance_form_field_empty_returns_none() {
        let fields: Vec<FormField> = vec![];
        assert_eq!(advance_form_field(&fields, 0, true), None);
        assert_eq!(advance_form_field(&fields, 0, false), None);
    }

    #[test]
    fn test_advance_form_field_only_start_editable() {
        // All others are readonly — should stay at start
        let fields = vec![make_field(false), make_field(true), make_field(true)];
        assert_eq!(advance_form_field(&fields, 0, true), Some(0));
        assert_eq!(advance_form_field(&fields, 0, false), Some(0));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 2: Navigation tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn back_from_repo_detail_goes_to_dashboard() {
        let mut app = make_app();
        app.state.view = View::RepoDetail;
        app.state.selected_repo_id = Some("r1".into());
        app.update(Action::Back);
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_repo_id.is_none());
    }

    #[test]
    fn back_from_worktree_detail_with_repo_goes_to_repo_detail() {
        let mut app = make_app();
        app.state.view = View::WorktreeDetail;
        app.state.selected_repo_id = Some("r1".into());
        app.state.selected_worktree_id = Some("w1".into());
        app.update(Action::Back);
        assert_eq!(app.state.view, View::RepoDetail);
        assert!(app.state.selected_worktree_id.is_none());
    }

    #[test]
    fn back_from_worktree_detail_without_repo_goes_to_dashboard() {
        let mut app = make_app();
        app.state.view = View::WorktreeDetail;
        app.state.selected_repo_id = None;
        app.state.selected_worktree_id = Some("w1".into());
        app.update(Action::Back);
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_worktree_id.is_none());
    }

    #[test]
    fn back_from_workflow_def_detail_restores_previous_view() {
        let mut app = make_app();
        app.state.view = View::WorkflowDefDetail;
        app.state.previous_view = Some(View::RepoDetail);
        app.update(Action::Back);
        assert_eq!(app.state.view, View::RepoDetail);
        assert!(app.state.selected_workflow_def.is_none());
        assert_eq!(app.state.column_focus, crate::state::ColumnFocus::Workflow);
        assert_eq!(
            app.state.workflows_focus,
            crate::state::WorkflowsFocus::Defs
        );
    }

    #[test]
    fn back_from_workflow_step_tree_exits_pane_not_view() {
        let mut app = make_app();
        app.state.column_focus = crate::state::ColumnFocus::Workflow;
        app.state.workflows_focus = crate::state::WorkflowsFocus::Defs;
        app.state.workflow_def_focus = crate::state::WorkflowDefFocus::Steps;
        app.state.view = View::Dashboard;
        app.update(Action::Back);
        // Should exit the step tree pane, not the view
        assert_eq!(
            app.state.workflow_def_focus,
            crate::state::WorkflowDefFocus::List
        );
        assert_eq!(app.state.view, View::Dashboard);
    }

    #[test]
    fn next_panel_cycles_repo_detail_focus() {
        let mut app = make_app();
        app.state.view = View::RepoDetail;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.state.repo_detail_focus = crate::state::RepoDetailFocus::Info;
        // Cycle: Info → Worktrees → Prs → Tickets → Info
        app.update(Action::NextPanel);
        assert_eq!(
            app.state.repo_detail_focus,
            crate::state::RepoDetailFocus::Worktrees
        );
        app.update(Action::NextPanel);
        assert_eq!(
            app.state.repo_detail_focus,
            crate::state::RepoDetailFocus::Prs
        );
        app.update(Action::NextPanel);
        assert_eq!(
            app.state.repo_detail_focus,
            crate::state::RepoDetailFocus::Tickets
        );
    }

    #[test]
    fn prev_panel_cycles_repo_detail_focus_backward() {
        let mut app = make_app();
        app.state.view = View::RepoDetail;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.state.repo_detail_focus = crate::state::RepoDetailFocus::Worktrees;
        app.update(Action::PrevPanel);
        assert_eq!(
            app.state.repo_detail_focus,
            crate::state::RepoDetailFocus::Info
        );
    }

    #[test]
    fn next_panel_toggles_worktree_detail_focus() {
        let mut app = make_app();
        app.state.view = View::WorktreeDetail;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.state.worktree_detail_focus = crate::state::WorktreeDetailFocus::InfoPanel;
        app.update(Action::NextPanel);
        assert_eq!(
            app.state.worktree_detail_focus,
            crate::state::WorktreeDetailFocus::LogPanel
        );
        app.update(Action::NextPanel);
        assert_eq!(
            app.state.worktree_detail_focus,
            crate::state::WorktreeDetailFocus::InfoPanel
        );
    }

    #[test]
    fn clamp_indices_handles_empty_lists() {
        let mut app = make_app();
        app.state.dashboard_index = 5;
        // With no data, dashboard_rows is empty → index stays as-is (clamp only when len > 0)
        app.clamp_indices();
        // dashboard_rows is empty so the clamp block doesn't fire
        assert_eq!(app.state.dashboard_index, 5);
    }

    #[test]
    fn move_down_dashboard_clamps_at_end() {
        let mut app = make_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        // No repos/worktrees → dashboard_rows is empty
        app.update(Action::MoveDown);
        assert_eq!(app.state.dashboard_index, 0);
    }

    #[test]
    fn move_up_dashboard_clamps_at_zero() {
        let mut app = make_app();
        app.state.view = View::Dashboard;
        app.state.column_focus = crate::state::ColumnFocus::Content;
        app.state.dashboard_index = 0;
        app.update(Action::MoveUp);
        assert_eq!(app.state.dashboard_index, 0);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 3: Modal dialog tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn confirm_quit_sets_should_quit() {
        let mut app = make_app();
        app.state.modal = Modal::Confirm {
            title: "Confirm Quit".into(),
            message: "Quit?".into(),
            on_confirm: crate::state::ConfirmAction::Quit,
        };
        app.update(Action::ConfirmYes);
        assert!(app.state.should_quit);
    }

    #[test]
    fn show_confirm_quit_no_agents_generic_message() {
        let mut app = make_app();
        app.show_confirm_quit();
        if let Modal::Confirm { message, .. } = &app.state.modal {
            assert_eq!(message, "Quit conductor?");
        } else {
            panic!("expected Confirm modal");
        }
    }

    #[test]
    fn show_confirm_quit_with_running_agents_includes_count() {
        let mut app = make_app();
        // Insert a running agent run
        app.state.data.latest_agent_runs.insert(
            "wt1".into(),
            conductor_core::agent::AgentRun {
                id: "run1".into(),
                worktree_id: Some("wt1".into()),
                claude_session_id: None,
                prompt: String::new(),
                status: conductor_core::agent::AgentRunStatus::Running,
                result_text: None,
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                started_at: "2024-01-01T00:00:00Z".into(),
                ended_at: None,
                tmux_window: None,
                log_file: None,
                model: None,
                plan: None,
                parent_run_id: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                bot_name: None,
            },
        );
        app.show_confirm_quit();
        if let Modal::Confirm { message, .. } = &app.state.modal {
            assert!(
                message.contains("1 agent is running"),
                "expected agent count in message: {message}"
            );
        } else {
            panic!("expected Confirm modal");
        }
    }

    #[test]
    fn delete_worktree_no_bg_tx_no_crash() {
        let mut app = make_app();
        assert!(app.bg_tx.is_none());
        app.execute_confirm_action(crate::state::ConfirmAction::DeleteWorktree {
            repo_slug: "test".into(),
            wt_slug: "test-wt".into(),
        });
        // No crash, modal should not change to Progress (because bg_tx is None → early return)
        assert!(matches!(app.state.modal, Modal::None));
    }

    #[test]
    fn unregister_repo_no_bg_tx_no_crash() {
        let mut app = make_app();
        assert!(app.bg_tx.is_none());
        app.execute_confirm_action(crate::state::ConfirmAction::UnregisterRepo {
            repo_slug: "test".into(),
        });
        assert!(matches!(app.state.modal, Modal::None));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 4: Git operations result handling tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn push_complete_ok_clears_modal_sets_status() {
        let mut app = make_app();
        app.state.modal = Modal::Progress {
            message: "Pushing…".into(),
        };
        app.update(Action::PushComplete {
            result: Ok("Pushed to origin/feat-x".into()),
        });
        assert!(matches!(app.state.modal, Modal::None));
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Pushed to origin/feat-x")
        );
    }

    #[test]
    fn push_complete_err_shows_error_modal() {
        let mut app = make_app();
        app.update(Action::PushComplete {
            result: Err("auth failed".into()),
        });
        if let Modal::Error { message } = &app.state.modal {
            assert!(message.contains("auth failed"));
        } else {
            panic!("expected Error modal");
        }
    }

    #[test]
    fn pr_create_complete_ok_sets_status() {
        let mut app = make_app();
        app.update(Action::PrCreateComplete {
            result: Ok("https://github.com/x/y/pull/1".into()),
        });
        assert!(matches!(app.state.modal, Modal::None));
        let msg = app.state.status_message.as_deref().unwrap();
        assert!(msg.contains("PR created"));
    }

    #[test]
    fn pr_create_complete_err_shows_error() {
        let mut app = make_app();
        app.update(Action::PrCreateComplete {
            result: Err("no commits".into()),
        });
        assert!(matches!(app.state.modal, Modal::Error { .. }));
    }

    #[test]
    fn worktree_delete_complete_ok_navigates_to_dashboard() {
        let mut app = make_app();
        app.state.view = View::WorktreeDetail;
        app.state.selected_worktree_id = Some("w1".into());
        app.update(Action::WorktreeDeleteComplete {
            wt_slug: "feat-x".into(),
            result: Ok("Merged".into()),
        });
        assert!(matches!(app.state.modal, Modal::None));
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_worktree_id.is_none());
        let msg = app.state.status_message.as_deref().unwrap();
        assert!(msg.contains("feat-x") && msg.contains("Merged"));
    }

    #[test]
    fn worktree_delete_complete_err_shows_error() {
        let mut app = make_app();
        app.update(Action::WorktreeDeleteComplete {
            wt_slug: "feat-x".into(),
            result: Err("worktree busy".into()),
        });
        assert!(matches!(app.state.modal, Modal::Error { .. }));
    }

    #[test]
    fn repo_unregister_complete_ok_navigates_to_dashboard() {
        let mut app = make_app();
        app.state.view = View::RepoDetail;
        app.state.selected_repo_id = Some("r1".into());
        app.update(Action::RepoUnregisterComplete {
            repo_slug: "my-repo".into(),
            result: Ok(()),
        });
        assert_eq!(app.state.view, View::Dashboard);
        assert!(app.state.selected_repo_id.is_none());
        let msg = app.state.status_message.as_deref().unwrap();
        assert!(msg.contains("my-repo"));
    }

    #[test]
    fn repo_unregister_complete_err_shows_error() {
        let mut app = make_app();
        app.update(Action::RepoUnregisterComplete {
            repo_slug: "my-repo".into(),
            result: Err("has worktrees".into()),
        });
        assert!(matches!(app.state.modal, Modal::Error { .. }));
    }

    #[test]
    fn background_error_shows_error_modal() {
        let mut app = make_app();
        app.update(Action::BackgroundError {
            message: "something broke".into(),
        });
        if let Modal::Error { message } = &app.state.modal {
            assert_eq!(message, "something broke");
        } else {
            panic!("expected Error modal");
        }
    }

    #[test]
    fn background_success_sets_status_message() {
        let mut app = make_app();
        app.update(Action::BackgroundSuccess {
            message: "done".into(),
        });
        assert_eq!(app.state.status_message.as_deref(), Some("done"));
    }

    #[test]
    fn handle_push_no_worktree_selected() {
        let mut app = make_app();
        app.state.selected_worktree_id = None;
        app.handle_push();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Select a worktree first")
        );
    }

    #[test]
    fn handle_create_pr_no_worktree_selected() {
        let mut app = make_app();
        app.state.selected_worktree_id = None;
        app.handle_create_pr();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Select a worktree first")
        );
    }

    #[test]
    fn handle_sync_tickets_already_in_progress() {
        let mut app = make_app();
        app.state.ticket_sync_in_progress = true;
        app.handle_sync_tickets();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Sync already in progress...")
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 5: Input handling tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn form_char_appends_to_active_field() {
        let mut app = make_app();
        app.state.modal = Modal::Form {
            title: "Test".into(),
            fields: vec![FormField {
                label: "Name".into(),
                value: String::new(),
                placeholder: String::new(),
                manually_edited: false,
                required: false,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            }],
            active_field: 0,
            on_submit: crate::state::FormAction::RegisterRepo,
        };
        app.update(Action::FormChar('x'));
        if let Modal::Form { ref fields, .. } = app.state.modal {
            assert_eq!(fields[0].value, "x");
            assert!(fields[0].manually_edited);
        } else {
            panic!("expected Form modal");
        }
    }

    #[test]
    fn form_backspace_removes_last_char() {
        let mut app = make_app();
        app.state.modal = Modal::Form {
            title: "Test".into(),
            fields: vec![FormField {
                label: "Name".into(),
                value: "abc".into(),
                placeholder: String::new(),
                manually_edited: true,
                required: false,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            }],
            active_field: 0,
            on_submit: crate::state::FormAction::RegisterRepo,
        };
        app.update(Action::FormBackspace);
        if let Modal::Form { ref fields, .. } = app.state.modal {
            assert_eq!(fields[0].value, "ab");
        } else {
            panic!("expected Form modal");
        }
    }

    #[test]
    fn form_next_prev_field_skips_readonly() {
        let mut app = make_app();
        app.state.modal = Modal::Form {
            title: "Test".into(),
            fields: vec![
                FormField {
                    label: "A".into(),
                    value: String::new(),
                    placeholder: String::new(),
                    manually_edited: false,
                    required: false,
                    readonly: false,
                    field_type: crate::state::FormFieldType::Text,
                },
                FormField {
                    label: "B".into(),
                    value: String::new(),
                    placeholder: String::new(),
                    manually_edited: false,
                    required: false,
                    readonly: true,
                    field_type: crate::state::FormFieldType::Text,
                },
                FormField {
                    label: "C".into(),
                    value: String::new(),
                    placeholder: String::new(),
                    manually_edited: false,
                    required: false,
                    readonly: false,
                    field_type: crate::state::FormFieldType::Text,
                },
            ],
            active_field: 0,
            on_submit: crate::state::FormAction::RegisterRepo,
        };
        // Next from 0 should skip readonly field 1 and land on 2
        app.update(Action::FormNextField);
        if let Modal::Form { active_field, .. } = app.state.modal {
            assert_eq!(active_field, 2);
        } else {
            panic!("expected Form modal");
        }
        // Prev from 2 should skip readonly field 1 and land on 0
        app.update(Action::FormPrevField);
        if let Modal::Form { active_field, .. } = app.state.modal {
            assert_eq!(active_field, 0);
        } else {
            panic!("expected Form modal");
        }
    }

    #[test]
    fn input_char_appends_to_modal_value() {
        let mut app = make_app();
        app.state.modal = Modal::Input {
            title: "Test".into(),
            prompt: "Enter:".into(),
            value: "hel".into(),
            on_submit: crate::state::InputAction::CreateWorktree {
                repo_slug: "r".into(),
                ticket_id: None,
            },
        };
        app.update(Action::InputChar('l'));
        app.update(Action::InputChar('o'));
        if let Modal::Input { ref value, .. } = app.state.modal {
            assert_eq!(value, "hello");
        } else {
            panic!("expected Input modal");
        }
    }

    #[test]
    fn input_backspace_removes_from_modal_value() {
        let mut app = make_app();
        app.state.modal = Modal::Input {
            title: "Test".into(),
            prompt: "Enter:".into(),
            value: "abc".into(),
            on_submit: crate::state::InputAction::CreateWorktree {
                repo_slug: "r".into(),
                ticket_id: None,
            },
        };
        app.update(Action::InputBackspace);
        if let Modal::Input { ref value, .. } = app.state.modal {
            assert_eq!(value, "ab");
        } else {
            panic!("expected Input modal");
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 6: Theme management tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn themes_loaded_opens_theme_picker_modal() {
        let mut app = make_app();
        let themes = vec![
            ("conductor".to_string(), "Conductor".to_string()),
            ("dark".to_string(), "Dark".to_string()),
        ];
        let loaded_themes = vec![Theme::default(), Theme::default()];
        app.handle_themes_loaded(themes.clone(), loaded_themes, vec![]);
        if let Modal::ThemePicker {
            themes: ref t,
            selected,
            ..
        } = app.state.modal
        {
            assert_eq!(t.len(), 2);
            // Default config theme is None → fallback "conductor" → should select idx 0
            assert_eq!(selected, 0);
        } else {
            panic!("expected ThemePicker modal");
        }
    }

    #[test]
    fn theme_preview_updates_theme() {
        let mut app = make_app();
        let default_theme = Theme::default();
        let other_theme = Theme::default(); // same type, different instance
        app.state.modal = Modal::ThemePicker {
            themes: vec![("a".into(), "A".into()), ("b".into(), "B".into())],
            loaded_themes: vec![default_theme, other_theme],
            selected: 0,
            original_theme: default_theme,
            original_name: "a".into(),
        };
        app.handle_theme_preview(1);
        if let Modal::ThemePicker { selected, .. } = app.state.modal {
            assert_eq!(selected, 1);
        } else {
            panic!("expected ThemePicker modal");
        }
    }

    #[test]
    fn theme_save_complete_ok_sets_status() {
        let mut app = make_app();
        app.update(Action::ThemeSaveComplete {
            result: Ok("Theme set to \"dark\"".into()),
        });
        assert!(matches!(app.state.modal, Modal::None));
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Theme set to \"dark\"")
        );
    }

    #[test]
    fn theme_save_complete_err_shows_error() {
        let mut app = make_app();
        app.update(Action::ThemeSaveComplete {
            result: Err("permission denied".into()),
        });
        if let Modal::Error { message } = &app.state.modal {
            assert!(message.contains("permission denied"));
        } else {
            panic!("expected Error modal");
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Task 7: URL operations tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn selected_ticket_url_from_ticket_info_modal() {
        let mut app = make_app();
        app.state.modal = Modal::TicketInfo {
            ticket: Box::new(conductor_core::tickets::Ticket {
                id: "t1".into(),
                repo_id: "r1".into(),
                source_type: "github".into(),
                source_id: "123".into(),
                title: "Test".into(),
                body: "body".into(),
                state: "open".into(),
                labels: "".into(),
                assignee: None,
                priority: None,
                url: "https://github.com/x/y/issues/123".into(),
                synced_at: "2024-01-01T00:00:00Z".into(),
                raw_json: "{}".into(),
            }),
        };
        assert_eq!(
            app.selected_ticket_url(),
            Some("https://github.com/x/y/issues/123".into())
        );
    }

    #[test]
    fn selected_ticket_url_no_ticket_available() {
        let app = make_app();
        assert!(app.selected_ticket_url().is_none());
    }

    #[test]
    fn repo_web_url_with_valid_github_remote() {
        let mut app = make_app();
        let repo = conductor_core::repo::Repo {
            id: "r1".into(),
            slug: "my-repo".into(),
            local_path: "/tmp/my-repo".into(),
            remote_url: "https://github.com/user/my-repo.git".into(),
            default_branch: "main".into(),
            workspace_dir: "/tmp".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            model: None,
            allow_agent_issue_creation: false,
        };
        app.state.selected_repo_id = Some("r1".into());
        app.state.data.repos = vec![repo];
        let url = app.repo_web_url();
        assert_eq!(url, Some("https://github.com/user/my-repo".into()));
    }

    #[test]
    fn repo_web_url_no_selected_repo() {
        let app = make_app();
        assert!(app.repo_web_url().is_none());
    }

    #[test]
    fn selected_pr_url_with_pr() {
        let mut app = make_app();
        app.state.detail_prs = vec![conductor_core::github::GithubPr {
            number: 1,
            title: "PR".into(),
            url: "https://github.com/x/y/pull/1".into(),
            author: "user".into(),
            head_ref_name: "feat-x".into(),
            state: "open".into(),
            is_draft: false,
            review_decision: None,
            ci_status: "success".into(),
        }];
        app.state.detail_pr_index = 0;
        assert_eq!(
            app.selected_pr_url(),
            Some("https://github.com/x/y/pull/1".into())
        );
    }

    #[test]
    fn selected_pr_url_empty_list() {
        let app = make_app();
        assert!(app.selected_pr_url().is_none());
    }

    #[test]
    fn selected_ticket_url_from_repo_detail_tickets() {
        let mut app = make_app();
        app.state.view = View::RepoDetail;
        app.state.repo_detail_focus = crate::state::RepoDetailFocus::Tickets;
        app.state.filtered_detail_tickets = vec![conductor_core::tickets::Ticket {
            id: "t1".into(),
            repo_id: "r1".into(),
            source_type: "github".into(),
            source_id: "42".into(),
            title: "A ticket".into(),
            body: "".into(),
            state: "open".into(),
            labels: "".into(),
            assignee: None,
            priority: None,
            url: "https://github.com/x/y/issues/42".into(),
            synced_at: "2024-01-01T00:00:00Z".into(),
            raw_json: "{}".into(),
        }];
        app.state.detail_ticket_index = 0;
        assert_eq!(
            app.selected_ticket_url(),
            Some("https://github.com/x/y/issues/42".into())
        );
    }
}
