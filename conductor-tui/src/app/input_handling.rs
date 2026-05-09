use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::state::{
    BranchPickerItem, ConfirmAction, FormAction, FormField, FormFieldType, InputAction, Modal,
    RuntimeSection,
};

use super::helpers::advance_form_field;
use super::App;

/// Build the `Vec<RuntimeSection>` shown in the model picker from the current config.
///
/// The built-in `"claude"` section is always first, combining `KNOWN_MODELS` aliases
/// with any models in `config.runtimes["claude"].supported_models`. Additional
/// user-defined runtimes follow in sorted order by name.
pub(crate) fn build_runtime_sections(config: &Config) -> Vec<RuntimeSection> {
    use conductor_core::models::KNOWN_MODELS;

    // Build "claude" section: KNOWN_MODELS aliases + migrated custom models
    let mut claude_models: Vec<String> = KNOWN_MODELS.iter().map(|m| m.alias.to_string()).collect();
    if let Some(claude_rt) = config.runtimes.get("claude") {
        for m in &claude_rt.supported_models {
            let already_known = KNOWN_MODELS
                .iter()
                .any(|km| km.alias == m.as_str() || km.id == m.as_str());
            if !already_known && !claude_models.contains(m) {
                claude_models.push(m.clone());
            }
        }
    }

    let mut sections = vec![RuntimeSection {
        name: "claude".to_string(),
        models: claude_models,
    }];

    // Additional user runtimes sorted by name (skip "claude" — already handled)
    for (name, rt) in crate::state::user_runtimes_sorted(&config.runtimes) {
        if !rt.supported_models.is_empty() {
            sections.push(RuntimeSection {
                name: name.clone(),
                models: rt.supported_models.clone(),
            });
        }
    }

    sections
}

/// Resolve flat selectable index → `(runtime_name, model_string)`.
///
/// `offset` is 1 when `allow_default` is true (Default row at index 0).
/// Returns `None` when the index is out of range.
pub(crate) fn resolve_picker_selection(
    sections: &[RuntimeSection],
    selected: usize,
    allow_default: bool,
) -> Option<(String, String)> {
    let offset = usize::from(allow_default);
    if allow_default && selected == 0 {
        return None; // "Default" row
    }
    let mut flat = offset;
    for section in sections {
        for model in &section.models {
            if flat == selected {
                return Some((section.name.clone(), model.clone()));
            }
            flat += 1;
        }
    }
    None
}

/// Compute the initial `selected` index for the picker given an effective default.
pub(crate) fn picker_initial_selected(
    sections: &[RuntimeSection],
    effective_default: Option<&str>,
    suggested: Option<&str>,
    allow_default: bool,
) -> usize {
    use conductor_core::models::KNOWN_MODELS;

    let offset = usize::from(allow_default);

    // Try to pre-select based on effective_default
    if let Some(default) = effective_default {
        let mut flat = offset;
        for section in sections {
            for model in &section.models {
                let matches = model == default
                    || KNOWN_MODELS
                        .iter()
                        .any(|km| (km.id == default || km.alias == default) && km.alias == model);
                if matches {
                    return flat;
                }
                flat += 1;
            }
        }
    }

    // Fall back to suggested model in claude section
    if let Some(sug) = suggested {
        let mut flat = offset;
        if let Some(claude_section) = sections.iter().find(|s| s.name == "claude") {
            for model in &claude_section.models {
                if model == sug {
                    return flat;
                }
                flat += 1;
            }
        }
    }

    // Default: sonnet (alias) if present in claude section, otherwise offset
    let mut flat = offset;
    if let Some(claude_section) = sections.iter().find(|s| s.name == "claude") {
        for model in &claude_section.models {
            if model == "sonnet" {
                return flat;
            }
            flat += 1;
        }
    }

    offset // fallback to first selectable row
}

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
                FormAction::AddRuntimeEnvVar { .. } => {}
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
                FormAction::AddRuntimeEnvVar { .. } => {}
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
                FormAction::AddRuntimeEnvVar { runtime } => {
                    self.submit_add_runtime_env_var(fields, &runtime);
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
                selected,
                runtime_sections,
                mut on_submit,
                allow_default,
                ..
            } => {
                let (runtime_opt, value) = if allow_default && selected == 0 {
                    // "Default" row selected — empty string maps to model: None
                    (None, String::new())
                } else {
                    match resolve_picker_selection(&runtime_sections, selected, allow_default) {
                        Some((rt, model)) => (Some(rt), model),
                        None => (None, String::new()),
                    }
                };
                // Inject the picker's runtime selection into model-bearing variants
                // that persist runtime context. SetWorktreeModel/SetRepoModel are
                // intentionally model-only; per-scope runtime persistence is
                // deferred per #2960.
                match on_submit {
                    InputAction::AgentModelOverride {
                        runtime: ref mut rt_field,
                        ..
                    } => {
                        *rt_field = runtime_opt;
                    }
                    InputAction::WorkflowModelOverride {
                        runtime: ref mut rt_field,
                        ..
                    } => {
                        *rt_field = runtime_opt;
                    }
                    _ => {}
                }
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
                        use conductor_core::repo::RepoManager;
                        use conductor_core::worktree::WorktreeManager;

                        let result = (|| {
                            let (conn, config) = load_db_and_config()?;
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

                // Build runtime sections from config
                let runtime_sections = build_runtime_sections(&self.config);

                // Pre-select the suggested model in the picker
                let initial_selected = picker_initial_selected(
                    &runtime_sections,
                    effective_default.as_deref(),
                    Some(suggested),
                    true,
                );

                self.state.modal = Modal::ModelPicker {
                    context_label: "agent run".to_string(),
                    effective_default,
                    effective_source,
                    selected: initial_selected,
                    runtime_sections,
                    suggested: Some(suggested.to_string()),
                    allow_default: true,
                    on_submit: InputAction::AgentModelOverride {
                        prompt: value,
                        worktree_id,
                        worktree_path,
                        worktree_slug,
                        resume_session_id,
                        resolved_default,
                        runtime: None, // Filled in from picker selection at submit time
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
                runtime,
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
                    runtime,
                );
            }
            InputAction::WorkflowModelOverride {
                action,
                inputs,
                runtime,
            } => {
                // Empty value means "Default" — use per-agent frontmatter / no override
                let model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value)
                };
                self.do_dispatch_workflow(
                    action.target,
                    action.workflow_def,
                    inputs,
                    model,
                    runtime,
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
            InputAction::SettingsSetModel
            | InputAction::SettingsSetSyncInterval
            | InputAction::SettingsSetStallTimeout
            | InputAction::SettingsSetMaxTurns
            | InputAction::SettingsAddRuntime
            | InputAction::SettingsAddModel { .. }
            | InputAction::SettingsEditModel { .. }
            | InputAction::SettingsEditEnvValue { .. } => {
                self.handle_settings_input_submit(on_submit, value);
            }
            InputAction::AdoptWorktree { repo_slug } => {
                if value.trim().is_empty() {
                    return;
                }
                self.spawn_worktree_adopt(repo_slug, value.trim().to_string());
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
                runtime,
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
                    runtime,
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
    /// Trigger the base branch change flow: enumerate remote branches off-thread,
    /// then open the picker via Action::BaseBranchesLoaded.
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

        let Some(tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot load branches: background sender not ready.".into(),
            };
            return;
        };

        let wt_branch = wt.branch.clone();
        let wt_slug = wt.slug.clone();

        self.state.modal = Modal::Progress {
            message: "Loading branches…".into(),
        };

        std::thread::spawn(move || {
            use crate::action::Action;
            use crate::state::BranchPickerItem;
            use conductor_core::repo::RepoManager;
            use conductor_core::worktree::WorktreeManager;

            let result = (|| {
                let (conn, config) = load_db_and_config()?;
                let repo = RepoManager::new(&conn, &config)
                    .get_by_slug(&repo_slug)
                    .map_err(|e| format!("Failed to get repo '{repo_slug}': {e}"))?;
                let default_branch = repo.default_branch.clone();

                // Get active worktrees for this repo.
                let active_worktrees = WorktreeManager::new(&conn, &config)
                    .list_by_repo_id(&repo.id, true)
                    .map_err(|e| format!("Failed to list worktrees: {e}"))?;

                // Build the candidate set: active worktree branches + repo default branch.
                // Exclude the worktree's own branch (selecting self as base is nonsensical).
                let mut branches: Vec<String> = active_worktrees
                    .iter()
                    .filter(|wt| wt.branch != wt_branch)
                    .map(|wt| wt.branch.clone())
                    .collect();

                // Add the default branch if it's not already included and not the current branch.
                if default_branch != wt_branch && !branches.contains(&default_branch) {
                    branches.push(default_branch);
                }

                // Sort alphabetically.
                branches.sort();

                let items = branches
                    .into_iter()
                    .map(|branch| BranchPickerItem {
                        branch: Some(branch),
                        ..Default::default()
                    })
                    .collect();

                Ok::<Vec<BranchPickerItem>, String>(items)
            })();

            match result {
                Ok(items) => {
                    let _ = tx.send(Action::BaseBranchesLoaded {
                        repo_slug,
                        wt_slug,
                        items,
                    });
                }
                Err(error) => {
                    let _ = tx.send(Action::BaseBranchesFailed { error });
                }
            }
        });
    }

    /// Handle the result of loading branches for base branch change.
    pub(super) fn handle_base_branches_loaded(
        &mut self,
        repo_slug: String,
        wt_slug: String,
        items: Vec<BranchPickerItem>,
    ) {
        let mut items_with_sentinel = vec![BranchPickerItem::default()];
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

            let Some(bg_tx) = self.bg_tx.clone() else {
                self.state.modal = Modal::Error {
                    message: "Cannot set base branch: background sender not ready.".into(),
                };
                return;
            };

            self.state.modal = Modal::Progress {
                message: "Updating base branch…".to_string(),
            };

            let config = self.config.clone();
            let label = new_base.as_deref().unwrap_or("(repo default)").to_string();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<String> {
                    let db = conductor_core::config::db_path();
                    let conn = conductor_core::db::open_database(&db)?;
                    let mgr = WorktreeManager::new(&conn, &config);
                    mgr.set_base_branch(
                        &repo_slug,
                        &wt_slug,
                        new_base.as_deref(),
                        conductor_core::worktree::SetBaseBranchOptions::default(),
                    )
                    .map_err(anyhow::Error::from)?;
                    Ok(format!("Base branch set to {label}"))
                })();
                let _ = bg_tx.send(crate::action::Action::SetBaseBranchComplete {
                    result: result.map_err(|e| e.to_string()),
                });
            });
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
        let mut items_with_sentinel = vec![crate::state::BranchPickerItem::default()];
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
                use conductor_core::repo::RepoManager;

                let result = (|| {
                    let (conn, config) = load_db_and_config()?;
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

fn load_db_and_config() -> Result<(rusqlite::Connection, Config), String> {
    use conductor_core::config::{db_path, load_config};
    use conductor_core::db::open_database;
    let conn = open_database(&db_path()).map_err(|e| format!("Failed to open database: {e}"))?;
    let config = load_config().map_err(|e| format!("Failed to load config: {e}"))?;
    Ok((conn, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{InputAction, Modal, View};
    use crate::theme::Theme;
    use conductor_core::{config::Config, models::KNOWN_MODELS};

    /// Creates an App backed by an in-memory DB that already contains:
    /// - repo `r1` (slug `test-repo`)
    /// - worktree `w1` (slug `feat-test`, branch `feat/test`, status `active`)
    fn make_app() -> App {
        crate::test_support::isolate_conductor_home();
        let conn = conductor_core::test_helpers::setup_db();
        App::new(
            conn,
            Config::default(),
            crate::config::TuiConfig::default(),
            Theme::default(),
        )
    }

    /// on_submit action that targets the pre-seeded worktree so DB writes succeed.
    fn set_wt_model_action() -> InputAction {
        InputAction::SetWorktreeModel {
            worktree_id: "w1".to_string(),
            repo_slug: "test-repo".to_string(),
            slug: "feat-test".to_string(),
        }
    }

    fn claude_only_sections() -> Vec<crate::state::RuntimeSection> {
        vec![crate::state::RuntimeSection {
            name: "claude".to_string(),
            models: KNOWN_MODELS.iter().map(|m| m.alias.to_string()).collect(),
        }]
    }

    fn model_picker(selected: usize, allow_default: bool) -> Modal {
        Modal::ModelPicker {
            context_label: "test".to_string(),
            effective_default: None,
            effective_source: "not set".to_string(),
            selected,
            runtime_sections: claude_only_sections(),
            suggested: None,
            on_submit: set_wt_model_action(),
            allow_default,
        }
    }

    fn model_picker_with_custom(
        selected: usize,
        allow_default: bool,
        custom: Vec<String>,
    ) -> Modal {
        // Custom models live inside the built-in claude section after KNOWN_MODELS aliases.
        let mut models: Vec<String> = KNOWN_MODELS.iter().map(|m| m.alias.to_string()).collect();
        models.extend(custom);
        Modal::ModelPicker {
            context_label: "test".to_string(),
            effective_default: None,
            effective_source: "not set".to_string(),
            selected,
            runtime_sections: vec![crate::state::RuntimeSection {
                name: "claude".to_string(),
                models,
            }],
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

    // The picker injects the resolved runtime into AgentModelOverride so non-claude
    // selections aren't silently dropped. Default row leaves runtime as None.
    #[test]
    fn model_picker_injects_runtime_into_agent_model_override() {
        use crate::state::RuntimeSection;
        let sections = vec![
            RuntimeSection {
                name: "claude".to_string(),
                models: vec!["opus".to_string(), "sonnet".to_string()],
            },
            RuntimeSection {
                name: "qwen-local".to_string(),
                models: vec!["qwen-72b".to_string()],
            },
        ];
        // selected=2 with allow_default=false → second section, first model (qwen-72b)
        let extracted = pick_and_extract_runtime(sections.clone(), 2, false);
        assert_eq!(
            extracted,
            Some("qwen-local".to_string()),
            "non-claude selection must surface the runtime name"
        );
        // selected=0 with allow_default=true → "Default" row → runtime stays None
        let extracted_default = pick_and_extract_runtime(sections, 0, true);
        assert_eq!(
            extracted_default, None,
            "Default row must leave runtime unset"
        );
    }

    /// Helper: simulate the ModelPicker submission shape and return the runtime
    /// field that was injected into the resulting `AgentModelOverride`.
    fn pick_and_extract_runtime(
        runtime_sections: Vec<crate::state::RuntimeSection>,
        selected: usize,
        allow_default: bool,
    ) -> Option<String> {
        use crate::state::InputAction;
        let mut on_submit = InputAction::AgentModelOverride {
            prompt: String::new(),
            worktree_id: "w1".into(),
            worktree_path: "/tmp/wt".into(),
            worktree_slug: "feat-test".into(),
            resume_session_id: None,
            resolved_default: None,
            runtime: None,
        };
        let runtime_opt = if allow_default && selected == 0 {
            None
        } else {
            resolve_picker_selection(&runtime_sections, selected, allow_default).map(|(rt, _)| rt)
        };
        if let InputAction::AgentModelOverride {
            runtime: ref mut rt_field,
            ..
        } = on_submit
        {
            *rt_field = runtime_opt;
        }
        match on_submit {
            InputAction::AgentModelOverride { runtime, .. } => runtime,
            _ => panic!("expected AgentModelOverride"),
        }
    }

    // selecting the first custom model row submits its value
    #[test]
    fn model_picker_custom_model_row_submits_value() {
        let mut app = make_app();
        let offset = 1_usize; // allow_default=true
        let custom_row_index = offset + KNOWN_MODELS.len(); // first custom model
        app.state.modal =
            model_picker_with_custom(custom_row_index, true, vec!["my-custom-model".to_string()]);
        app.handle_input_submit();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Model for feat-test set to: my-custom-model")
        );
    }

    // selecting the second custom model row submits its value
    #[test]
    fn model_picker_second_custom_model_submits_correct_value() {
        let mut app = make_app();
        let offset = 0_usize; // allow_default=false
        let custom_row_index = offset + KNOWN_MODELS.len() + 1; // second custom model
        app.state.modal = model_picker_with_custom(
            custom_row_index,
            false,
            vec!["first-model".to_string(), "second-model".to_string()],
        );
        app.handle_input_submit();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Model for feat-test set to: second-model")
        );
    }

    // ---------- Branch picker tests ----------

    fn branch_picker_items() -> Vec<crate::state::BranchPickerItem> {
        vec![
            crate::state::BranchPickerItem::default(),
            crate::state::BranchPickerItem {
                branch: Some("feat/notifications".to_string()),
                base_branch: Some("main".to_string()),
                ..Default::default()
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

    // ---------- build_runtime_sections tests ----------

    fn config_with_custom_runtimes(
        claude_custom: Vec<&str>,
        other: Vec<(&str, Vec<&str>)>,
    ) -> Config {
        use conductor_core::config::RuntimeConfig;
        crate::test_support::isolate_conductor_home();
        let mut config = Config::default();
        if !claude_custom.is_empty() {
            config.runtimes.insert(
                "claude".to_string(),
                RuntimeConfig {
                    supported_models: claude_custom.into_iter().map(String::from).collect(),
                    ..Default::default()
                },
            );
        }
        for (name, models) in other {
            config.runtimes.insert(
                name.to_string(),
                RuntimeConfig {
                    supported_models: models.into_iter().map(String::from).collect(),
                    ..Default::default()
                },
            );
        }
        config
    }

    #[test]
    fn build_runtime_sections_empty_config_returns_claude_with_known_models() {
        crate::test_support::isolate_conductor_home();
        let config = Config::default();
        let sections = build_runtime_sections(&config);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].name, "claude");
        for km in KNOWN_MODELS {
            assert!(
                sections[0].models.contains(&km.alias.to_string()),
                "expected alias {} in claude section",
                km.alias
            );
        }
        assert_eq!(sections[0].models.len(), KNOWN_MODELS.len());
    }

    #[test]
    fn build_runtime_sections_claude_custom_models_appended_without_duplicates() {
        let config = config_with_custom_runtimes(vec!["sonnet", "my-custom-model"], vec![]);
        let sections = build_runtime_sections(&config);
        assert_eq!(sections.len(), 1);
        let models = &sections[0].models;
        assert!(models.contains(&"my-custom-model".to_string()));
        assert_eq!(
            models.iter().filter(|m| m.as_str() == "sonnet").count(),
            1,
            "sonnet should appear exactly once (no duplicate from custom list)"
        );
        assert_eq!(models.len(), KNOWN_MODELS.len() + 1);
    }

    #[test]
    fn build_runtime_sections_custom_runtimes_appear_after_claude_sorted() {
        let config = config_with_custom_runtimes(
            vec![],
            vec![("zebra-rt", vec!["z-model"]), ("alpha-rt", vec!["a-model"])],
        );
        let sections = build_runtime_sections(&config);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "claude");
        assert_eq!(sections[1].name, "alpha-rt");
        assert_eq!(sections[2].name, "zebra-rt");
    }

    #[test]
    fn build_runtime_sections_empty_custom_runtime_excluded() {
        let config = config_with_custom_runtimes(vec![], vec![("empty-rt", vec![])]);
        let sections = build_runtime_sections(&config);
        assert_eq!(
            sections.len(),
            1,
            "runtime with no supported_models should be excluded"
        );
    }

    // ---------- resolve_picker_selection tests ----------

    fn two_section_sections() -> Vec<crate::state::RuntimeSection> {
        vec![
            crate::state::RuntimeSection {
                name: "claude".to_string(),
                models: vec!["opus".to_string(), "sonnet".to_string()],
            },
            crate::state::RuntimeSection {
                name: "gemini".to_string(),
                models: vec!["gemini-pro".to_string()],
            },
        ]
    }

    #[test]
    fn resolve_picker_no_default_idx0_returns_first_model() {
        let sections = two_section_sections();
        assert_eq!(
            resolve_picker_selection(&sections, 0, false),
            Some(("claude".to_string(), "opus".to_string()))
        );
    }

    #[test]
    fn resolve_picker_no_default_idx1_returns_second_model() {
        let sections = two_section_sections();
        assert_eq!(
            resolve_picker_selection(&sections, 1, false),
            Some(("claude".to_string(), "sonnet".to_string()))
        );
    }

    #[test]
    fn resolve_picker_no_default_idx2_crosses_section_boundary() {
        let sections = two_section_sections();
        assert_eq!(
            resolve_picker_selection(&sections, 2, false),
            Some(("gemini".to_string(), "gemini-pro".to_string()))
        );
    }

    #[test]
    fn resolve_picker_with_default_idx0_returns_none() {
        let sections = two_section_sections();
        assert_eq!(resolve_picker_selection(&sections, 0, true), None);
    }

    #[test]
    fn resolve_picker_with_default_idx1_returns_first_model_with_offset() {
        let sections = two_section_sections();
        assert_eq!(
            resolve_picker_selection(&sections, 1, true),
            Some(("claude".to_string(), "opus".to_string()))
        );
    }

    #[test]
    fn resolve_picker_out_of_range_returns_none() {
        let sections = two_section_sections();
        assert_eq!(resolve_picker_selection(&sections, 999, false), None);
    }

    // ---------- picker_initial_selected tests ----------

    fn sonnet_flat_idx(sections: &[crate::state::RuntimeSection], allow_default: bool) -> usize {
        let offset = usize::from(allow_default);
        let mut flat = offset;
        for s in sections {
            for m in &s.models {
                if m == "sonnet" {
                    return flat;
                }
                flat += 1;
            }
        }
        panic!("sonnet not found in sections");
    }

    #[test]
    fn picker_initial_selected_effective_default_alias_selects_correct_row() {
        let sections = claude_only_sections();
        let expected = sonnet_flat_idx(&sections, false);
        assert_eq!(
            picker_initial_selected(&sections, Some("sonnet"), None, false),
            expected
        );
    }

    #[test]
    fn picker_initial_selected_suggested_sonnet_selects_correct_row() {
        let sections = claude_only_sections();
        let expected = sonnet_flat_idx(&sections, false);
        assert_eq!(
            picker_initial_selected(&sections, None, Some("sonnet"), false),
            expected
        );
    }

    #[test]
    fn picker_initial_selected_no_hint_defaults_to_sonnet() {
        let sections = claude_only_sections();
        let expected = sonnet_flat_idx(&sections, false);
        assert_eq!(
            picker_initial_selected(&sections, None, None, false),
            expected
        );
    }

    #[test]
    fn picker_initial_selected_allow_default_adds_offset_to_sonnet() {
        let sections = claude_only_sections();
        let expected = sonnet_flat_idx(&sections, true);
        assert_eq!(
            picker_initial_selected(&sections, None, None, true),
            expected
        );
    }

    #[test]
    fn picker_initial_selected_empty_sections_returns_offset() {
        let empty: Vec<crate::state::RuntimeSection> = vec![];
        assert_eq!(picker_initial_selected(&empty, None, None, false), 0);
        assert_eq!(picker_initial_selected(&empty, None, None, true), 1);
    }

    // ---------- Base branch picker tests ----------

    fn base_branch_picker_modal(selected: usize) -> Modal {
        let items = vec![
            crate::state::BranchPickerItem::default(),
            crate::state::BranchPickerItem {
                branch: Some("main".to_string()),
                ..Default::default()
            },
        ];
        let (ordered, tree_positions) = crate::state::build_branch_picker_tree(&items);
        Modal::BaseBranchPicker {
            repo_slug: "test-repo".to_string(),
            wt_slug: "feat-test".to_string(),
            items: ordered,
            tree_positions,
            selected,
        }
    }

    #[test]
    fn base_branch_pick_bg_tx_none_shows_error_modal() {
        let mut app = make_app();
        // bg_tx is None in a freshly-created App (no event loop started).
        app.state.modal = base_branch_picker_modal(1);
        app.handle_base_branch_pick(Some(1));
        assert!(
            matches!(app.state.modal, Modal::Error { .. }),
            "expected Error modal when bg_tx is None, got {:?}",
            app.state.modal
        );
    }

    #[test]
    fn base_branch_pick_shows_progress_modal() {
        let mut app = make_app();
        let (bg_tx, _rx) = crate::event::BackgroundSender::channel_for_test();
        app.bg_tx = Some(bg_tx);
        app.state.modal = base_branch_picker_modal(1);
        app.handle_base_branch_pick(Some(1));
        assert!(
            matches!(app.state.modal, Modal::Progress { .. }),
            "expected Progress modal after dispatching background work, got {:?}",
            app.state.modal
        );
    }

    #[test]
    fn base_branch_pick_sends_complete_action_to_bg_tx() {
        let mut app = make_app();
        let (bg_tx, rx) = crate::event::BackgroundSender::channel_for_test();
        app.bg_tx = Some(bg_tx);
        app.state.modal = base_branch_picker_modal(1);
        app.handle_base_branch_pick(Some(1));
        // Give the spawned thread a moment to finish and send.
        let action = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("expected SetBaseBranchComplete action from background thread within 5s");
        assert!(
            matches!(action, crate::action::Action::SetBaseBranchComplete { .. }),
            "expected SetBaseBranchComplete, got {:?}",
            action
        );
    }

    #[test]
    fn base_branch_pick_out_of_bounds_uses_repo_default() {
        let mut app = make_app();
        let (bg_tx, rx) = crate::event::BackgroundSender::channel_for_test();
        app.bg_tx = Some(bg_tx);
        app.state.modal = base_branch_picker_modal(0);
        // index 999 is beyond the items list → new_base = None (repo default)
        app.handle_base_branch_pick(Some(999));
        // The thread sends SetBaseBranchComplete regardless; just verify it sends.
        let action = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("expected action from background thread within 5s");
        assert!(
            matches!(action, crate::action::Action::SetBaseBranchComplete { .. }),
            "expected SetBaseBranchComplete, got {:?}",
            action
        );
    }

    #[test]
    fn worktree_branches_loaded_always_has_default_branch_sentinel() {
        let mut app = make_app();
        let worktree_items = vec![crate::state::BranchPickerItem {
            branch: Some("feat/notifications".to_string()),
            inferred_from: Some("feat-notifications".to_string()),
            ..Default::default()
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

    // ---------- handle_set_base_branch tests ----------

    #[test]
    fn set_base_branch_not_in_worktree_detail_returns_early() {
        let mut app = make_app();
        app.state.view = View::Dashboard; // Not WorktreeDetail
        app.handle_set_base_branch();
        // Should return early without showing any modal
        assert!(matches!(app.state.modal, Modal::None));
    }

    #[test]
    fn set_base_branch_no_selected_worktree_returns_early() {
        let mut app = make_app();
        app.state.view = View::WorktreeDetail;
        app.state.selected_worktree_id = None;
        app.handle_set_base_branch();
        // Should return early without showing any modal
        assert!(matches!(app.state.modal, Modal::None));
    }

    #[test]
    fn set_base_branch_shows_progress_modal_and_spawns_thread() {
        let mut app = make_app();
        let (bg_tx, rx) = crate::event::BackgroundSender::channel_for_test();
        app.bg_tx = Some(bg_tx);
        app.state.view = View::WorktreeDetail;
        app.state.selected_worktree_id = Some("w1".to_string());

        // Populate in-memory data (the DB has the worktree, but state.data.worktrees is empty)
        app.state
            .data
            .worktrees
            .push(conductor_core::worktree::Worktree {
                id: "w1".to_string(),
                repo_id: "r1".to_string(),
                slug: "feat-test".to_string(),
                branch: "feat/test".to_string(),
                path: "/tmp/ws/feat-test".to_string(),
                ticket_id: None,
                status: conductor_core::worktree::WorktreeStatus::Active,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                completed_at: None,
                model: None,
                base_branch: None,
            });
        app.state.data.repos.push(conductor_core::repo::Repo {
            id: "r1".to_string(),
            slug: "test-repo".to_string(),
            remote_url: "https://github.com/test/repo.git".to_string(),
            local_path: "/tmp/repo".to_string(),
            default_branch: "main".to_string(),
            workspace_dir: "/tmp/ws".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            model: None,
            allow_agent_issue_creation: true,
            runtime_overrides: None,
        });
        app.state
            .data
            .repo_slug_map
            .insert("r1".to_string(), "test-repo".to_string());

        app.handle_set_base_branch();

        // Progress modal should be shown
        assert!(
            matches!(app.state.modal, Modal::Progress { .. }),
            "expected Progress modal, got {:?}",
            app.state.modal
        );

        // Background thread should send either BaseBranchesLoaded or BaseBranchesFailed
        let action = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("expected action from background thread within 5s");
        assert!(
            matches!(
                action,
                crate::action::Action::BaseBranchesLoaded { .. }
                    | crate::action::Action::BaseBranchesFailed { .. }
            ),
            "expected BaseBranchesLoaded or BaseBranchesFailed, got {:?}",
            action
        );
    }

    #[test]
    fn set_base_branch_bg_tx_none_shows_error_modal() {
        let mut app = make_app();
        app.state.view = View::WorktreeDetail;
        app.state.selected_worktree_id = Some("w1".to_string());

        // Populate in-memory data
        app.state
            .data
            .worktrees
            .push(conductor_core::worktree::Worktree {
                id: "w1".to_string(),
                repo_id: "r1".to_string(),
                slug: "feat-test".to_string(),
                branch: "feat/test".to_string(),
                path: "/tmp/ws/feat-test".to_string(),
                ticket_id: None,
                status: conductor_core::worktree::WorktreeStatus::Active,
                created_at: "2024-01-01T00:00:00Z".to_string(),
                completed_at: None,
                model: None,
                base_branch: None,
            });
        app.state.data.repos.push(conductor_core::repo::Repo {
            id: "r1".to_string(),
            slug: "test-repo".to_string(),
            remote_url: "https://github.com/test/repo.git".to_string(),
            local_path: "/tmp/repo".to_string(),
            default_branch: "main".to_string(),
            workspace_dir: "/tmp/ws".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            model: None,
            allow_agent_issue_creation: true,
            runtime_overrides: None,
        });
        app.state
            .data
            .repo_slug_map
            .insert("r1".to_string(), "test-repo".to_string());

        // bg_tx is None (fresh app)
        app.handle_set_base_branch();

        assert!(
            matches!(app.state.modal, Modal::Error { .. }),
            "expected Error modal when bg_tx is None, got {:?}",
            app.state.modal
        );
    }
}
