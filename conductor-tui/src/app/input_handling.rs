use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::state::{ConfirmAction, FormAction, FormField, FormFieldType, InputAction, Modal};

use super::helpers::advance_form_field;
use super::App;

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
                self.start_agent_tmux(
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
}
