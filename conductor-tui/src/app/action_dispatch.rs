use std::time::Duration;

use conductor_core::models;
use conductor_core::workflow::parse_workflow_str;

use crate::action::Action;
use crate::state::{Modal, View, WorkflowDefFocus};

use super::helpers::{collapse_loop_iterations, max_scroll, workflow_parse_warning_message};
use super::App;

impl App {
    pub(super) fn handle_action(&mut self, action: Action) -> bool {
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
                // Periodically refresh the PR list when the RepoDetail or WorktreeDetail view is active.
                if self.state.view == View::RepoDetail || self.state.view == View::WorktreeDetail {
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
                                    crate::background::spawn_pr_fetch_once(
                                        tx.clone(),
                                        remote_url,
                                        rid,
                                    );
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
            Action::MainHealthCheckComplete {
                repo_slug,
                wt_name,
                ticket_id,
                from_pr,
                from_branch,
                status,
            } => match status {
                Err(e) => {
                    self.state.modal = Modal::Error {
                        message: format!("Main branch health check failed: {e}"),
                    };
                }
                Ok(health) if health.is_dirty => {
                    let mut message = format!(
                        "Base branch has {} uncommitted change(s):\n",
                        health.dirty_files.len()
                    );
                    for f in health.dirty_files.iter().take(10) {
                        message.push_str(&format!("  {f}\n"));
                    }
                    if health.dirty_files.len() > 10 {
                        message
                            .push_str(&format!("  … and {} more\n", health.dirty_files.len() - 10));
                    }
                    message.push_str("\nProceed anyway?");
                    self.state.modal = Modal::Confirm {
                        title: "Dirty Base Branch".to_string(),
                        message,
                        on_confirm: crate::state::ConfirmAction::CreateWorktree {
                            repo_slug,
                            wt_name,
                            ticket_id,
                            from_pr,
                            from_branch,
                            force_dirty: true,
                        },
                    };
                }
                Ok(health) => {
                    if health.commits_behind > 0 {
                        self.state.status_message = Some(format!(
                            "Base branch is {} commit(s) behind origin (will fast-forward)",
                            health.commits_behind
                        ));
                    }
                    self.spawn_worktree_create(
                        repo_slug,
                        wt_name,
                        conductor_core::worktree::WorktreeCreateOptions {
                            ticket_id,
                            from_pr,
                            from_branch,
                            pre_health: Some(health),
                            ..Default::default()
                        },
                    );
                }
            },
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
            Action::SelectListItem(index) => {
                if let Modal::WorkflowPicker {
                    ref items,
                    ref mut selected,
                    ..
                } = self.state.modal
                {
                    if !items.get(index).is_some_and(|i| i.is_selectable()) {
                        return false;
                    }
                    *selected = index;
                    self.handle_workflow_picker_confirm();
                } else if let Modal::TemplatePicker {
                    ref mut selected, ..
                } = self.state.modal
                {
                    *selected = index;
                    self.handle_template_picker_confirm();
                }
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
                    scroll_offset: 0,
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

            // Ticket tree collapse/expand toggle
            Action::ToggleTicketCollapse => {
                let idx = self.state.detail_ticket_index;
                let is_parent = self
                    .state
                    .detail_ticket_tree_positions
                    .get(idx)
                    .is_some_and(|p| p.is_parent);
                if is_parent {
                    if let Some(ticket_id) = self
                        .state
                        .filtered_detail_tickets
                        .get(idx)
                        .map(|t| t.id.clone())
                    {
                        if self.state.collapsed_ticket_ids.contains(&ticket_id) {
                            self.state.collapsed_ticket_ids.remove(&ticket_id);
                        } else {
                            self.state.collapsed_ticket_ids.insert(ticket_id);
                        }
                        self.state.rebuild_filtered_tickets();
                        // Clamp index after visibility change.
                        let len = self.state.filtered_detail_tickets.len();
                        if len > 0 && self.state.detail_ticket_index >= len {
                            self.state.detail_ticket_index = len - 1;
                        }
                    }
                }
            }

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
            Action::PromptRepoAgent => self.handle_prompt_repo_agent(),
            Action::OrchestrateAgent => self.handle_orchestrate_agent(),
            Action::StopAgent => {
                if self.is_repo_agent_context() {
                    self.handle_stop_repo_agent();
                } else {
                    self.handle_stop_agent();
                }
            }
            Action::RestartAgent => self.handle_restart_agent(),
            Action::SubmitFeedback => {
                if self.is_repo_agent_context() {
                    self.handle_submit_repo_feedback();
                } else {
                    self.handle_submit_feedback();
                }
            }
            Action::DismissFeedback => {
                if self.is_repo_agent_context() {
                    self.handle_dismiss_repo_feedback();
                } else {
                    self.handle_dismiss_feedback();
                }
            }
            Action::CopyLastCodeBlock => self.handle_copy_last_code_block(),
            Action::ExpandAgentEvent => {
                if self.is_repo_agent_context() {
                    self.handle_expand_repo_agent_event();
                } else {
                    self.handle_expand_agent_event();
                }
            }
            Action::AgentActivityDown => {
                if self.is_repo_agent_context() {
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
                } else {
                    let len = self.state.data.agent_activity_len();
                    let cur = self.state.agent_list_state.borrow().selected().unwrap_or(0);
                    if len > 0 && cur + 1 < len {
                        self.state.agent_list_state.borrow_mut().select_next();
                    }
                }
            }
            Action::AgentActivityUp => {
                if self.is_repo_agent_context() {
                    self.state
                        .repo_agent_list_state
                        .borrow_mut()
                        .select_previous();
                } else {
                    self.state.agent_list_state.borrow_mut().select_previous();
                }
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
                Modal::ModelPicker {
                    ref mut selected,
                    ref mut custom_active,
                    ..
                } => {
                    *selected = 0;
                    *custom_active = false;
                }
                Modal::BranchPicker {
                    ref mut selected, ..
                }
                | Modal::BaseBranchPicker {
                    ref mut selected, ..
                }
                | Modal::TemplatePicker {
                    ref mut selected, ..
                }
                | Modal::IssueSourceManager {
                    ref mut selected, ..
                }
                | Modal::Notifications {
                    ref mut selected, ..
                } => {
                    *selected = 0;
                }
                Modal::WorkflowPicker {
                    ref mut selected,
                    ref mut scroll_offset,
                    ..
                } => {
                    *selected = 0;
                    *scroll_offset = 0;
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
                Modal::ModelPicker {
                    ref mut selected,
                    ref mut custom_active,
                    allow_default,
                    ..
                } => {
                    let total = models::KNOWN_MODELS.len() + 1 + usize::from(allow_default);
                    *selected = total.saturating_sub(1);
                    *custom_active = false;
                }
                Modal::BranchPicker {
                    ref items,
                    ref mut selected,
                    ..
                } => {
                    *selected = items.len().saturating_sub(1);
                }
                Modal::BaseBranchPicker {
                    ref items,
                    ref mut selected,
                    ..
                } => {
                    *selected = items.len().saturating_sub(1);
                }
                Modal::WorkflowPicker {
                    ref items,
                    ref mut selected,
                    ref mut scroll_offset,
                    ..
                } => {
                    *selected = items.iter().rposition(|i| i.is_selectable()).unwrap_or(0);
                    *scroll_offset = u16::MAX;
                }
                Modal::TemplatePicker {
                    ref items,
                    ref mut selected,
                    ..
                } => {
                    *selected = items.len().saturating_sub(1);
                }
                Modal::IssueSourceManager {
                    ref sources,
                    ref mut selected,
                    ..
                } => {
                    *selected = sources.len().saturating_sub(1);
                }
                Modal::Notifications {
                    ref notifications,
                    ref mut selected,
                } => {
                    *selected = notifications.len().saturating_sub(1);
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
            Action::PickTemplate => self.handle_pick_template(),
            Action::TemplateInstantiateReady {
                template_name,
                prompt,
                suggested_filename,
            } => {
                self.state.modal = Modal::None;
                self.state.status_message = Some(format!(
                    "Template '{template_name}' ready — prompt prepared ({} chars), output: .conductor/workflows/{suggested_filename}",
                    prompt.len(),
                ));
            }
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
            Action::GateToggleOption => {
                if let Modal::GateAction {
                    ref options,
                    ref mut selected,
                    ref focused_option,
                    ..
                } = self.state.modal
                {
                    if *focused_option < options.len() {
                        selected[*focused_option] = !selected[*focused_option];
                    }
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
                let new_len = self.state.visible_workflow_run_rows_len();
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
                self.state.data.ticket_dependencies = payload.ticket_dependencies;
                self.state.data.latest_agent_runs = payload.latest_agent_runs;
                self.state.data.ticket_agent_totals = payload.ticket_agent_totals;
                self.state.data.latest_workflow_runs_by_worktree =
                    payload.latest_workflow_runs_by_worktree;
                self.state.data.workflow_step_summaries = payload.workflow_step_summaries;
                self.state.data.active_non_worktree_workflow_runs =
                    payload.active_non_worktree_workflow_runs;
                self.state.data.live_turns_by_worktree = payload.live_turns_by_worktree;
                self.state.data.features_by_repo = payload.features_by_repo;
                self.state.data.latest_repo_agent_runs = payload.latest_repo_agent_runs;
                self.state.data.all_worktree_agent_events = payload.worktree_agent_events;
                self.state.data.all_repo_agent_events = payload.repo_agent_events;
                self.state.unread_notification_count = payload.unread_notification_count;
                self.refresh_pending_feedback();
                self.refresh_pending_repo_feedback();
                self.state.data.rebuild_maps();
                self.reload_agent_events();
                self.reload_repo_agent_events();
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
            Action::AgentLaunchComplete { result }
            | Action::OrchestrateLaunchComplete { result }
            | Action::AgentRestartComplete { result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(msg) => {
                        self.state.status_message = Some(msg);
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error { message: e };
                    }
                }
            }
            Action::RepoAgentLaunched { result } | Action::RepoAgentStopComplete { result } => {
                self.handle_repo_agent_result(result);
            }
            Action::AgentStopComplete { result } => {
                self.state.modal = Modal::None;
                match result {
                    Ok(msg) => {
                        self.state.status_message = Some(msg);
                        self.refresh_data();
                    }
                    Err(e) => {
                        self.state.modal = Modal::Error { message: e };
                    }
                }
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

            // ── Graph view actions ─────────────────────────────────────────
            Action::OpenTicketGraphView => {
                self.open_ticket_graph_view();
            }

            Action::OpenWorkflowStepGraphView => {
                self.state.status_message = Some("Workflow step graph view coming soon".into());
            }

            Action::GraphNavLeft => {
                if let Modal::GraphView { ref mut nav, .. } = self.state.modal {
                    nav.selected_layer = nav.selected_layer.saturating_sub(1);
                    nav.selected_node_idx = 0;
                }
            }
            Action::GraphNavRight => {
                if let Modal::GraphView {
                    ref mut nav,
                    ref data,
                    ..
                } = self.state.modal
                {
                    let layers = crate::ui::graph::compute_layers(data);
                    let max_layer = layers.len().saturating_sub(1);
                    if nav.selected_layer < max_layer {
                        nav.selected_layer += 1;
                        nav.selected_node_idx = 0;
                    }
                }
            }
            Action::GraphNavUp => {
                if let Modal::GraphView { ref mut nav, .. } = self.state.modal {
                    nav.selected_node_idx = nav.selected_node_idx.saturating_sub(1);
                }
            }
            Action::GraphNavDown => {
                if let Modal::GraphView {
                    ref mut nav,
                    ref data,
                    ..
                } = self.state.modal
                {
                    let layers = crate::ui::graph::compute_layers(data);
                    let layer_len = layers.get(nav.selected_layer).map(|l| l.len()).unwrap_or(0);
                    if nav.selected_node_idx + 1 < layer_len {
                        nav.selected_node_idx += 1;
                    }
                }
            }
            Action::GraphPanLeft => {
                if let Modal::GraphView { ref mut nav, .. } = self.state.modal {
                    nav.pan_x = nav.pan_x.saturating_sub(4);
                    if nav.pan_x < 0 {
                        nav.pan_x = 0;
                    }
                }
            }
            Action::GraphPanRight => {
                if let Modal::GraphView { ref mut nav, .. } = self.state.modal {
                    nav.pan_x = nav.pan_x.saturating_add(4);
                }
            }
            Action::GraphPanUp => {
                if let Modal::GraphView { ref mut nav, .. } = self.state.modal {
                    nav.pan_y = nav.pan_y.saturating_sub(4);
                    if nav.pan_y < 0 {
                        nav.pan_y = 0;
                    }
                }
            }
            Action::GraphPanDown => {
                if let Modal::GraphView { ref mut nav, .. } = self.state.modal {
                    nav.pan_y = nav.pan_y.saturating_add(4);
                }
            }
        }
        true
    }

    /// Build and open the ticket dependency graph for the current repo.
    fn open_ticket_graph_view(&mut self) {
        use crate::ui::graph::{
            EdgeType, GraphData, GraphEdge, GraphNavState, GraphNodeType, TicketGraphNode,
        };

        let tickets = self.state.detail_tickets.clone();
        let deps = self.state.data.ticket_dependencies.clone();

        // Collect all node IDs that appear in at least one edge
        let mut connected_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        let mut edges: Vec<GraphEdge> = Vec::new();

        for ticket in &tickets {
            if let Some(d) = deps.get(&ticket.id) {
                for blocker in &d.blocked_by {
                    connected_ids.insert(ticket.id.clone());
                    connected_ids.insert(blocker.id.clone());
                    edges.push(GraphEdge {
                        from: blocker.id.clone(),
                        to: ticket.id.clone(),
                        edge_type: EdgeType::BlockedBy,
                    });
                }
                if let Some(parent) = &d.parent {
                    connected_ids.insert(ticket.id.clone());
                    connected_ids.insert(parent.id.clone());
                    edges.push(GraphEdge {
                        from: parent.id.clone(),
                        to: ticket.id.clone(),
                        edge_type: EdgeType::ParentChild,
                    });
                }
            }
        }

        // Deduplicate edges
        edges.sort_by(|a, b| a.from.cmp(&b.from).then(a.to.cmp(&b.to)));
        edges.dedup_by(|a, b| a.from == b.from && a.to == b.to);

        // Collect all unique ticket IDs we need nodes for (from tickets list + deps)
        let mut ticket_map: std::collections::HashMap<String, TicketGraphNode> =
            std::collections::HashMap::new();
        for t in &tickets {
            ticket_map.insert(t.id.clone(), TicketGraphNode::from_ticket(t));
        }
        // Also add tickets referenced in deps that may not be in detail_tickets
        for ticket in &tickets {
            if let Some(d) = deps.get(&ticket.id) {
                for blocker in &d.blocked_by {
                    ticket_map
                        .entry(blocker.id.clone())
                        .or_insert_with(|| TicketGraphNode::from_ticket(blocker));
                }
                if let Some(parent) = &d.parent {
                    ticket_map
                        .entry(parent.id.clone())
                        .or_insert_with(|| TicketGraphNode::from_ticket(parent));
                }
            }
        }

        let unconnected_count = tickets
            .iter()
            .filter(|t| !connected_ids.contains(&t.id))
            .count();

        // Annotate nodes with active-worktree status
        let ticket_worktrees = &self.state.data.ticket_worktrees;
        let nodes: Vec<GraphNodeType> = ticket_map
            .into_values()
            .map(|mut n| {
                n.has_worktree = ticket_worktrees
                    .get(&n.id)
                    .and_then(|wts| wts.iter().find(|w| w.is_active()))
                    .is_some();
                GraphNodeType::Ticket(n)
            })
            .collect();

        let repo_slug = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
            .map(|r| r.slug.clone())
            .unwrap_or_else(|| "repo".into());

        let title = format!("Dependency Graph — {repo_slug} tickets");

        self.state.modal = Modal::GraphView {
            data: GraphData {
                nodes,
                edges,
                unconnected_count,
            },
            nav: GraphNavState::default(),
            title,
        };
    }

    /// Handle the result of a repo-scoped agent launch or stop operation.
    fn handle_repo_agent_result(&mut self, result: Result<String, String>) {
        self.state.modal = Modal::None;
        match result {
            Ok(msg) => {
                self.state.status_message = Some(msg);
                self.refresh_data();
                self.reload_repo_agent_events();
            }
            Err(e) => {
                self.state.modal = Modal::Error { message: e };
            }
        }
    }
}
