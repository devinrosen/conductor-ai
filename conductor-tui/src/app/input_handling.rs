use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::state::{
    BranchPickerItem, ConfirmAction, FormAction, FormField, FormFieldType, InputAction, Modal,
};

use super::helpers::advance_form_field;
use super::App;

/// Build the "From PR (optional)" input modal used to transition into the PR step
/// of worktree creation. Centralised here to avoid duplicating the title/prompt strings.
fn pr_step_modal(
    repo_slug: String,
    wt_name: String,
    ticket_id: Option<String>,
    from_branch: Option<String>,
) -> Modal {
    Modal::Input {
        title: "From PR (optional)".to_string(),
        prompt: "PR number to check out (leave blank for new branch):".to_string(),
        value: String::new(),
        on_submit: InputAction::CreateWorktreePrStep {
            repo_slug,
            wt_name,
            ticket_id,
            from_branch,
        },
    }
}

impl App {
    pub(super) fn handle_form_char(&mut self, c: char) {
        let config = &self.config;
        if let Modal::Form {
            ref mut fields,
            active_field,
            ref on_submit,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                if !field.readonly {
                    field.value.push(c);
                    field.manually_edited = true;
                }
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

    pub(super) fn handle_form_backspace(&mut self) {
        let config = &self.config;
        if let Modal::Form {
            ref mut fields,
            active_field,
            ref on_submit,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                if !field.readonly {
                    field.value.pop();
                    // If field emptied and it's a derived field, reset to auto-derive
                    if field.value.is_empty() && active_field > 0 {
                        field.manually_edited = false;
                    }
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

    pub(super) fn handle_form_next_field(&mut self) {
        if let Modal::Form {
            ref fields,
            ref mut active_field,
            ..
        } = self.state.modal
        {
            if let Some(next) = advance_form_field(fields, *active_field, true) {
                *active_field = next;
            }
        }
    }

    pub(super) fn handle_form_prev_field(&mut self) {
        if let Modal::Form {
            ref fields,
            ref mut active_field,
            ..
        } = self.state.modal
        {
            if let Some(prev) = advance_form_field(fields, *active_field, false) {
                *active_field = prev;
            }
        }
    }

    pub(super) fn auto_derive_register_repo_fields(
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
    pub(super) fn sync_issue_source_form_fields(fields: &mut Vec<FormField>) {
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
                readonly: false,
                field_type: FormFieldType::Text,
            });
            fields.push(FormField {
                label: "Jira URL".to_string(),
                value: String::new(),
                placeholder: "e.g. https://mycompany.atlassian.net".to_string(),
                manually_edited: false,
                required: true,
                readonly: false,
                field_type: FormFieldType::Text,
            });
        } else if !is_jira && fields.len() > 1 {
            // Remove extra fields when switching away from Jira
            fields.truncate(1);
        }
    }

    pub(super) fn handle_form_submit(&mut self) {
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

    pub(super) fn handle_form_toggle(&mut self) {
        if let Modal::Form {
            ref mut fields,
            active_field,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                if !field.readonly {
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
    }

    pub(super) fn handle_input_submit(&mut self) {
        // ThemePicker: persist the selected theme to config
        if let Modal::ThemePicker { selected, .. } = self.state.modal {
            self.handle_theme_picker_confirm(selected);
            return;
        }

        // WorkflowPicker: confirm the selected workflow
        if matches!(self.state.modal, Modal::WorkflowPicker { .. }) {
            self.handle_workflow_picker_confirm();
            return;
        }

        // TemplatePicker: confirm the selected template
        if matches!(self.state.modal, Modal::TemplatePicker { .. }) {
            self.handle_template_picker_confirm();
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
                allow_default,
            } => {
                let models = conductor_core::models::KNOWN_MODELS;
                let offset = usize::from(allow_default);
                let value = if custom_active {
                    custom_input
                } else if allow_default && selected == 0 {
                    // "Default" row selected — empty string maps to model: None
                    String::new()
                } else if selected < offset + models.len() {
                    // Selected a known model — use its alias
                    models[selected - offset].alias.to_string()
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
                        allow_default,
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
                let wt_name = value;
                if let Some(ref tx) = self.bg_tx {
                    let tx = tx.clone();
                    let slug = repo_slug.clone();
                    let name = wt_name.clone();
                    let tid = ticket_id.clone();
                    std::thread::spawn(move || {
                        use crate::action::Action;
                        use crate::state::BranchPickerItem;
                        use conductor_core::config::{db_path, load_config};
                        use conductor_core::db::open_database;
                        use conductor_core::repo::RepoManager;
                        use conductor_core::worktree::WorktreeManager;

                        let result = (|| {
                            let conn = open_database(&db_path())
                                .map_err(|e| format!("Failed to open database: {e}"))?;
                            let config =
                                load_config().map_err(|e| format!("Failed to load config: {e}"))?;
                            let repo = RepoManager::new(&conn, &config)
                                .get_by_slug(&slug)
                                .map_err(|e| format!("Failed to get repo '{slug}': {e}"))?;
                            let worktrees = WorktreeManager::new(&conn, &config)
                                .list_by_repo_id(&repo.id, true)
                                .map_err(|e| format!("Failed to list worktrees: {e}"))?;
                            let items = worktrees
                                .into_iter()
                                .map(|wt| BranchPickerItem {
                                    branch: Some(wt.branch),
                                    worktree_count: 0,
                                    ticket_count: 0,
                                    base_branch: None,
                                    stale_days: None,
                                    inferred_from: Some(wt.slug),
                                })
                                .collect();
                            Ok::<Vec<BranchPickerItem>, String>(items)
                        })();
                        match result {
                            Ok(items) => {
                                let _ = tx.send(Action::WorktreeBranchesLoaded {
                                    repo_slug: slug,
                                    wt_name: name,
                                    ticket_id: tid,
                                    items,
                                });
                            }
                            Err(error) => {
                                let _ = tx.send(Action::WorktreeBranchesFailed { error });
                            }
                        }
                    });
                    self.state.modal = Modal::Progress {
                        message: "Loading worktrees…".into(),
                    };
                } else {
                    self.state.modal = pr_step_modal(repo_slug, wt_name, ticket_id, None);
                }
            }
            InputAction::CreateWorktreePrStep {
                repo_slug,
                wt_name,
                ticket_id,
                from_branch,
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
                            from_branch,
                            force_dirty: false,
                        },
                    };
                } else {
                    self.spawn_main_health_check(
                        repo_slug,
                        wt_name,
                        ticket_id,
                        from_pr,
                        from_branch,
                    );
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
                let wt = self
                    .state
                    .data
                    .worktrees
                    .iter()
                    .find(|w| w.id == worktree_id);
                let wt_model = wt.and_then(|w| w.model.as_deref());
                let repo_model = wt
                    .and_then(|w| self.state.data.repos.iter().find(|r| r.id == w.repo_id))
                    .and_then(|r| r.model.as_deref());
                let resolved_default = conductor_core::models::resolve_model(
                    wt_model,
                    repo_model,
                    self.config.general.model.as_deref(),
                );

                // Suggest a model based on the prompt text
                let suggested = conductor_core::models::suggest_model(&value);

                // Pre-select the suggested model in the picker
                let initial_selected = conductor_core::models::KNOWN_MODELS
                    .iter()
                    .position(|m| m.alias == suggested)
                    .unwrap_or(1); // default to sonnet

                let (effective_default, effective_source) = match &resolved_default {
                    Some(m) => {
                        let source = if wt_model.is_some() {
                            "worktree"
                        } else if repo_model.is_some() {
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
                    allow_default: true,
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
                self.start_agent_headless(
                    prompt,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    resume_session_id,
                    model,
                );
            }
            InputAction::WorkflowModelOverride { action, inputs } => {
                // Empty value means "Default" — use per-agent frontmatter / no override
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value)
                };
                self.do_dispatch_workflow(action.target, action.workflow_def, inputs, model);
            }
            InputAction::RepoAgentPrompt {
                repo_id,
                repo_path,
                repo_slug,
                resume_session_id,
            } => {
                if value.is_empty() {
                    return;
                }
                self.start_repo_agent_headless(
                    value,
                    repo_id,
                    repo_path,
                    repo_slug,
                    resume_session_id,
                );
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
            InputAction::SetRepoModel { slug } => {
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                self.spawn_set_repo_model(slug, model);
            }
            InputAction::FeedbackResponse { feedback_id } => {
                if value.is_empty() {
                    return;
                }
                // Convert user input based on feedback type (select → option value)
                let resolved_value = if let Some(ref fb) = self.state.data.pending_feedback {
                    use conductor_core::agent::normalize_feedback_response;
                    match normalize_feedback_response(
                        &fb.feedback_type,
                        fb.options.as_deref(),
                        &value,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("Failed to encode feedback response: {e}"),
                            };
                            return;
                        }
                    }
                } else {
                    value.clone()
                };
                let mgr = AgentManager::new(&self.conn);
                match mgr.submit_feedback(&feedback_id, &resolved_value) {
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
            InputAction::SettingsSetModel | InputAction::SettingsSetSyncInterval => {
                self.handle_settings_input_submit(on_submit, value);
            }
        }
    }

    /// Helper: submit a value + action directly (used when clearing model via Backspace).
    pub(super) fn handle_input_submit_with_value(&mut self, value: String, on_submit: InputAction) {
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
            InputAction::SetRepoModel { slug } => {
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                self.spawn_set_repo_model(slug, model);
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
                self.start_agent_headless(
                    prompt,
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    resume_session_id,
                    model,
                );
            }
            InputAction::SettingsSetModel | InputAction::SettingsSetSyncInterval => {
                self.handle_settings_input_submit(on_submit, value);
            }
            _ => {}
        }
    }

    /// Handle branch selection from the BranchPicker modal.
    /// `index = None` means use the modal's `selected` field (Enter key);
    /// `index = Some(i)` means a direct numeric pick.
    /// Trigger the base branch change flow: open picker with empty items list.
    pub(super) fn handle_set_base_branch(&mut self) {
        // Only valid in WorktreeDetail view
        if self.state.view != crate::state::View::WorktreeDetail {
            return;
        }

        // Find the current worktree & its repo slug
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|wt_id| self.state.data.worktrees.iter().find(|w| w.id == *wt_id))
            .cloned();
        let Some(wt) = wt else { return };

        let repo_slug = self
            .state
            .data
            .repo_slug_map
            .get(&wt.repo_id)
            .cloned()
            .unwrap_or_default();

        // No feature branches — open picker with empty items (just the default branch sentinel).
        self.handle_base_branches_loaded(repo_slug, wt.slug, vec![]);
    }

    /// Handle the result of loading branches for base branch change.
    pub(super) fn handle_base_branches_loaded(
        &mut self,
        repo_slug: String,
        wt_slug: String,
        items: Vec<BranchPickerItem>,
    ) {
        let mut items_with_sentinel = vec![BranchPickerItem {
            branch: None,
            worktree_count: 0,
            ticket_count: 0,
            base_branch: None,
            stale_days: None,
            inferred_from: None,
        }];
        items_with_sentinel.extend(items);
        let (ordered, tree_positions) =
            crate::state::build_branch_picker_tree(&items_with_sentinel);
        self.state.modal = Modal::BaseBranchPicker {
            repo_slug,
            wt_slug,
            items: ordered,
            tree_positions,
            selected: 0,
        };
    }

    /// Handle base branch selection from the BaseBranchPicker modal.
    pub(super) fn handle_base_branch_pick(&mut self, index: Option<usize>) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::BaseBranchPicker {
            repo_slug,
            wt_slug,
            items,
            selected,
            ..
        } = modal
        {
            let idx = index.unwrap_or(selected);
            let new_base = items.get(idx).and_then(|item| item.branch.clone());

            let wm = WorktreeManager::new(&self.conn, &self.config);
            match wm.set_base_branch(&repo_slug, &wt_slug, new_base.as_deref()) {
                Ok(()) => {
                    let label = new_base.as_deref().unwrap_or("(repo default)");
                    self.state.status_message = Some(format!("Base branch set to {label}"));
                    self.refresh_data();
                }
                Err(e) => {
                    self.state.modal = Modal::Error {
                        message: format!("Failed to set base branch: {e}"),
                    };
                }
            }
        }
    }

    pub(super) fn handle_branch_pick(&mut self, index: Option<usize>) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::BranchPicker {
            repo_slug,
            wt_name,
            ticket_id,
            items,
            selected,
            ..
        } = modal
        {
            let idx = index.unwrap_or(selected);
            let from_branch = items.get(idx).and_then(|item| item.branch.clone());
            // Transition to PR input step, carrying the selected branch.
            self.state.modal = pr_step_modal(repo_slug, wt_name, ticket_id, from_branch);
        }
    }

    /// Handle background result: existing worktree branches loaded for the branch picker.
    pub(super) fn handle_worktree_branches_loaded(
        &mut self,
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
        items: Vec<crate::state::BranchPickerItem>,
    ) {
        let mut items_with_sentinel = vec![crate::state::BranchPickerItem {
            branch: None,
            worktree_count: 0,
            ticket_count: 0,
            base_branch: None,
            stale_days: None,
            inferred_from: None,
        }];
        items_with_sentinel.extend(items);
        let (ordered, tree_positions) =
            crate::state::build_branch_picker_tree(&items_with_sentinel);
        self.state.modal = Modal::BranchPicker {
            repo_slug,
            wt_name,
            ticket_id,
            items: ordered,
            tree_positions,
            selected: 0,
        };
    }

    /// Spawn a background thread to set the repo model via file I/O,
    /// keeping the TUI main thread unblocked.
    fn spawn_set_repo_model(&mut self, slug: String, model: Option<String>) {
        if let Some(ref tx) = self.bg_tx {
            let tx = tx.clone();
            let model_clone = model.clone();
            let slug_clone = slug.clone();
            std::thread::spawn(move || {
                use crate::action::Action;
                use conductor_core::config::{db_path, load_config};
                use conductor_core::db::open_database;
                use conductor_core::repo::RepoManager;

                let result = (|| {
                    let db = db_path();
                    let conn =
                        open_database(&db).map_err(|e| format!("Failed to open database: {e}"))?;
                    let config =
                        load_config().map_err(|e| format!("Failed to load config: {e}"))?;
                    let mgr = RepoManager::new(&conn, &config);
                    mgr.set_model(&slug_clone, model_clone.as_deref())
                        .map_err(|e| format!("{e}"))?;
                    Ok(model_clone)
                })();
                let _ = tx.send(Action::SetRepoModelComplete { slug, result });
            });
            self.state.modal = Modal::Progress {
                message: "Setting repo model…".into(),
            };
        } else {
            // Fallback: no background sender (e.g. in tests), run synchronously.
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{InputAction, Modal};
    use crate::theme::Theme;
    use conductor_core::{config::Config, models::KNOWN_MODELS};

    /// Creates an App backed by an in-memory DB that already contains:
    /// - repo `r1` (slug `test-repo`)
    /// - worktree `w1` (slug `feat-test`, branch `feat/test`, status `active`)
    fn make_app() -> App {
        let conn = conductor_core::test_helpers::setup_db();
        App::new(conn, Config::default(), Theme::default())
    }

    /// on_submit action that targets the pre-seeded worktree so DB writes succeed.
    fn set_wt_model_action() -> InputAction {
        InputAction::SetWorktreeModel {
            worktree_id: "w1".to_string(),
            repo_slug: "test-repo".to_string(),
            slug: "feat-test".to_string(),
        }
    }

    fn model_picker(selected: usize, allow_default: bool) -> Modal {
        Modal::ModelPicker {
            context_label: "test".to_string(),
            effective_default: None,
            effective_source: "not set".to_string(),
            selected,
            custom_input: String::new(),
            custom_active: false,
            suggested: None,
            on_submit: set_wt_model_action(),
            allow_default,
        }
    }

    // allow_default=false, selected=0 → offset=0, picks models[0]
    #[test]
    fn model_picker_no_default_selected_0_picks_first_known_model() {
        let mut app = make_app();
        app.state.modal = model_picker(0, false);
        app.handle_input_submit();
        let expected = format!("Model for feat-test set to: {}", KNOWN_MODELS[0].alias);
        assert_eq!(app.state.status_message.as_deref(), Some(expected.as_str()));
    }

    // allow_default=false, selected=1 → offset=0, picks models[1]
    #[test]
    fn model_picker_no_default_selected_1_picks_second_known_model() {
        let mut app = make_app();
        app.state.modal = model_picker(1, false);
        app.handle_input_submit();
        let expected = format!("Model for feat-test set to: {}", KNOWN_MODELS[1].alias);
        assert_eq!(app.state.status_message.as_deref(), Some(expected.as_str()));
    }

    // allow_default=true, selected=0 → "Default" row → empty string → model cleared
    #[test]
    fn model_picker_with_default_selected_0_clears_model() {
        let mut app = make_app();
        app.state.modal = model_picker(0, true);
        app.handle_input_submit();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Model for feat-test cleared")
        );
    }

    // allow_default=true, selected=1 → offset=1, picks models[0] (first known model)
    #[test]
    fn model_picker_with_default_selected_1_picks_first_known_model() {
        let mut app = make_app();
        app.state.modal = model_picker(1, true);
        app.handle_input_submit();
        let expected = format!("Model for feat-test set to: {}", KNOWN_MODELS[0].alias);
        assert_eq!(app.state.status_message.as_deref(), Some(expected.as_str()));
    }

    // selected at the "custom…" row → re-opens picker with custom_active=true
    #[test]
    fn model_picker_custom_row_reopens_picker_with_custom_active() {
        let mut app = make_app();
        let offset = 1_usize; // allow_default=true
        let custom_row_index = offset + KNOWN_MODELS.len();
        app.state.modal = model_picker(custom_row_index, true);
        app.handle_input_submit();
        assert!(
            matches!(
                app.state.modal,
                Modal::ModelPicker {
                    custom_active: true,
                    ..
                }
            ),
            "expected picker to re-open with custom_active=true, got {:?}",
            app.state.modal
        );
    }

    // custom_active=true → value comes from custom_input, not the selected index
    #[test]
    fn model_picker_custom_active_submits_custom_input_value() {
        let mut app = make_app();
        app.state.modal = Modal::ModelPicker {
            context_label: "test".to_string(),
            effective_default: None,
            effective_source: "not set".to_string(),
            selected: 99, // index is irrelevant when custom_active=true
            custom_input: "my-custom-model".to_string(),
            custom_active: true,
            suggested: None,
            on_submit: set_wt_model_action(),
            allow_default: false,
        };
        app.handle_input_submit();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Model for feat-test set to: my-custom-model")
        );
    }

    // ---------- Branch picker tests ----------

    fn branch_picker_items() -> Vec<crate::state::BranchPickerItem> {
        vec![
            crate::state::BranchPickerItem {
                branch: None,
                worktree_count: 0,
                ticket_count: 0,
                base_branch: None,
                stale_days: None,
                inferred_from: None,
            },
            crate::state::BranchPickerItem {
                branch: Some("feat/notifications".to_string()),
                worktree_count: 0,
                ticket_count: 0,
                base_branch: Some("main".to_string()),
                stale_days: None,
                inferred_from: None,
            },
        ]
    }

    fn branch_picker_modal(selected: usize) -> Modal {
        let items = branch_picker_items();
        let (ordered, tree_positions) = crate::state::build_branch_picker_tree(&items);
        Modal::BranchPicker {
            repo_slug: "test-repo".to_string(),
            wt_name: "my-wt".to_string(),
            ticket_id: None,
            items: ordered,
            tree_positions,
            selected,
        }
    }

    #[test]
    fn branch_pick_explicit_index_selects_that_item() {
        let mut app = make_app();
        app.state.modal = branch_picker_modal(0);
        // Pick index 1 explicitly (feat/notifications).
        app.handle_branch_pick(Some(1));
        match &app.state.modal {
            Modal::Input { on_submit, .. } => match on_submit {
                InputAction::CreateWorktreePrStep { from_branch, .. } => {
                    assert_eq!(from_branch.as_deref(), Some("feat/notifications"));
                }
                other => panic!("expected CreateWorktreePrStep, got {:?}", other),
            },
            other => panic!("expected Input modal, got {:?}", other),
        }
    }

    #[test]
    fn branch_pick_none_uses_selected_field() {
        let mut app = make_app();
        app.state.modal = branch_picker_modal(1); // selected = 1 (feat/notifications)
                                                  // None means "use the modal's selected field".
        app.handle_branch_pick(None);
        match &app.state.modal {
            Modal::Input { on_submit, .. } => match on_submit {
                InputAction::CreateWorktreePrStep { from_branch, .. } => {
                    assert_eq!(from_branch.as_deref(), Some("feat/notifications"));
                }
                other => panic!("expected CreateWorktreePrStep, got {:?}", other),
            },
            other => panic!("expected Input modal, got {:?}", other),
        }
    }

    #[test]
    fn branch_pick_default_branch_sets_none() {
        let mut app = make_app();
        app.state.modal = branch_picker_modal(0);
        // Pick index 0 (default branch → None).
        app.handle_branch_pick(Some(0));
        match &app.state.modal {
            Modal::Input { on_submit, .. } => match on_submit {
                InputAction::CreateWorktreePrStep { from_branch, .. } => {
                    assert!(from_branch.is_none());
                }
                other => panic!("expected CreateWorktreePrStep, got {:?}", other),
            },
            other => panic!("expected Input modal, got {:?}", other),
        }
    }

    #[test]
    fn branch_pick_out_of_bounds_falls_back_to_default_branch() {
        let mut app = make_app();
        app.state.modal = branch_picker_modal(0);
        // Pick an index beyond the items list → items.get() returns None → from_branch = None.
        app.handle_branch_pick(Some(999));
        match &app.state.modal {
            Modal::Input { on_submit, .. } => match on_submit {
                InputAction::CreateWorktreePrStep { from_branch, .. } => {
                    assert!(
                        from_branch.is_none(),
                        "out-of-bounds index should yield None (default branch)"
                    );
                }
                other => panic!("expected CreateWorktreePrStep, got {:?}", other),
            },
            other => panic!("expected Input modal, got {:?}", other),
        }
    }

    #[test]
    fn branch_pick_noop_when_not_branch_picker_modal() {
        let mut app = make_app();
        app.state.modal = Modal::None;
        app.handle_branch_pick(Some(0));
        // Modal should stay None (no-op).
        assert!(matches!(app.state.modal, Modal::None));
    }

    #[test]
    fn worktree_branches_loaded_always_has_default_branch_sentinel() {
        let mut app = make_app();
        let worktree_items = vec![crate::state::BranchPickerItem {
            branch: Some("feat/notifications".to_string()),
            worktree_count: 0,
            ticket_count: 0,
            base_branch: None,
            stale_days: None,
            inferred_from: Some("feat-notifications".to_string()),
        }];
        app.handle_worktree_branches_loaded(
            "repo".to_string(),
            "my-wt".to_string(),
            None,
            worktree_items,
        );
        match &app.state.modal {
            Modal::BranchPicker { items, .. } => {
                assert!(
                    items[0].branch.is_none(),
                    "first item must be the default-branch sentinel (branch: None)"
                );
                assert_eq!(items.len(), 2, "sentinel + one worktree branch");
            }
            other => panic!("expected BranchPicker modal, got {:?}", other),
        }
    }

    #[test]
    fn worktree_branches_loaded_no_worktrees_shows_picker_with_only_sentinel() {
        let mut app = make_app();
        app.handle_worktree_branches_loaded("repo".to_string(), "my-wt".to_string(), None, vec![]);
        match &app.state.modal {
            Modal::BranchPicker { items, .. } => {
                assert_eq!(items.len(), 1, "only the default-branch sentinel");
                assert!(items[0].branch.is_none());
            }
            other => panic!("expected BranchPicker modal, got {:?}", other),
        }
    }
}
