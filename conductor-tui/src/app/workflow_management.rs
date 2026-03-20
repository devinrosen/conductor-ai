use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use conductor_core::worktree::WorktreeManager;

use crate::action::Action;
use crate::state::{ConfirmAction, Modal, View, WorkflowRunDetailFocus};

/// Error type for [`App::resolve_workflow_target`].
///
/// Most errors are transient status messages, but some (e.g. a missing repo
/// for a ticket) should be surfaced as a modal to ensure the user sees them.
pub(super) enum WorkflowTargetError {
    /// Show as a transient status-bar message.
    Status(String),
    /// Show as a blocking error modal.
    Modal(String),
}

impl From<String> for WorkflowTargetError {
    fn from(s: String) -> Self {
        WorkflowTargetError::Status(s)
    }
}

use super::helpers::{
    build_form_fields, collapse_loop_iterations, send_workflow_result,
    workflow_parse_warning_message,
};
use super::App;

/// Resolve a feature ID for a workflow run in a background thread.
///
/// Opens a fresh DB connection and calls `resolve_feature_id_for_run`.
/// Returns an error string on failure so the caller can surface it to the
/// user (e.g. via `bg_tx`) instead of silently proceeding without feature
/// context.
fn resolve_feature_id_for_bg(
    config: &conductor_core::config::Config,
    feature_name: Option<&str>,
    repo_slug: Option<&str>,
    ticket_id: Option<&str>,
    worktree_slug: Option<&str>,
) -> Result<Option<String>, String> {
    let db_path = conductor_core::config::db_path();
    let conn = conductor_core::db::open_database(&db_path)
        .map_err(|e| format!("feature resolution: failed to open database: {e}"))?;
    conductor_core::feature::FeatureManager::new(&conn, config)
        .resolve_feature_id_for_run(feature_name, repo_slug, ticket_id, worktree_slug)
        .map_err(|e| format!("Feature resolution failed: {e}"))
}

impl App {
    /// Dispatch workflow data loading to a background thread. The result
    /// arrives as a `WorkflowDataRefreshed` action, avoiding synchronous
    /// FS + DB I/O on the main loop tick.
    /// When no worktree is selected (global mode), loads all runs across worktrees.
    pub(super) fn poll_workflow_data_async(&self) {
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

    pub(super) fn reload_workflow_data(&mut self) {
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

    pub(super) fn reload_workflow_steps(&mut self) {
        use conductor_core::workflow::WorkflowManager;

        if let Some(ref run_id) = self.state.selected_workflow_run_id {
            let wf_mgr = WorkflowManager::new(&self.conn);
            self.state.data.workflow_steps =
                collapse_loop_iterations(wf_mgr.get_workflow_steps(run_id).unwrap_or_default());
        } else {
            self.state.data.workflow_steps.clear();
        }
        // Clear stale agent event cache; the background poller will refresh it.
        self.state.data.step_agent_events.clear();
        self.state.data.step_agent_run = None;
    }

    /// Get the child_run_id of the currently selected workflow step.
    pub(super) fn selected_step_child_run_id(&self) -> Option<String> {
        self.state
            .data
            .workflow_steps
            .get(self.state.workflow_step_index)
            .and_then(|s| s.child_run_id.clone())
    }

    pub(super) fn clamp_workflow_indices(&mut self) {
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

    /// Resolve the workflow target for the current view/focus context.
    ///
    /// Consolidates the dispatch logic used by both `handle_pick_workflow()` and
    /// `handle_run_workflow()`. Returns `Ok(target)` or `Err(message)` for the
    /// caller to surface (e.g. via `status_message`).
    pub(super) fn resolve_workflow_target(
        &self,
    ) -> Result<crate::state::WorkflowPickerTarget, WorkflowTargetError> {
        use crate::state::{RepoDetailFocus, WorkflowPickerTarget};

        if self.state.view == View::WorktreeDetail {
            let wt = self
                .state
                .selected_worktree_id
                .as_ref()
                .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                .ok_or_else(|| "No worktree selected".to_string())?
                .clone();
            self.worktree_picker_target(&wt).ok_or_else(|| {
                WorkflowTargetError::Status("Repo not found for this worktree".to_string())
            })
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == RepoDetailFocus::Prs
        {
            let pr = self
                .state
                .detail_prs
                .get(self.state.detail_pr_index)
                .ok_or_else(|| "No PR selected".to_string())?;
            Ok(WorkflowPickerTarget::Pr {
                pr_number: pr.number,
                pr_title: pr.title.clone(),
            })
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == RepoDetailFocus::Info
        {
            let repo = self
                .state
                .selected_repo_id
                .as_ref()
                .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
                .ok_or_else(|| "No repo selected".to_string())?
                .clone();
            Ok(self.repo_picker_target(&repo))
        } else if self.state.column_focus == crate::state::ColumnFocus::Workflow
            && self.state.workflows_focus == crate::state::WorkflowsFocus::Runs
        {
            // Resolve the selected workflow run from the runs pane (non-mutating)
            let rows = self.state.visible_workflow_run_rows();
            let run_id = rows
                .get(self.state.workflow_run_index)
                .and_then(|r| r.run_id())
                .ok_or_else(|| "No workflow run selected".to_string())?
                .to_string();
            let run = self
                .state
                .data
                .workflow_runs
                .iter()
                .find(|r| r.id == run_id)
                .ok_or_else(|| "Workflow run not found".to_string())?
                .clone();
            self.workflow_run_picker_target(&run).ok_or_else(|| {
                WorkflowTargetError::Status(
                    "Cannot determine repo for this workflow run".to_string(),
                )
            })
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == RepoDetailFocus::Worktrees
        {
            let wt = self
                .state
                .detail_worktrees
                .get(self.state.detail_wt_index)
                .ok_or_else(|| "No worktree selected".to_string())?
                .clone();
            self.worktree_picker_target(&wt).ok_or_else(|| {
                WorkflowTargetError::Status("Repo not found for this worktree".to_string())
            })
        } else if self.state.view == View::Dashboard {
            use crate::state::DashboardRow;
            let rows = self.state.dashboard_rows();
            match rows.get(self.state.dashboard_index) {
                Some(&DashboardRow::Repo(repo_idx)) => {
                    let repo = self
                        .state
                        .data
                        .repos
                        .get(repo_idx)
                        .ok_or_else(|| format!("Internal error: repo index {repo_idx} not found"))?
                        .clone();
                    Ok(self.repo_picker_target(&repo))
                }
                Some(&DashboardRow::Worktree { idx: wt_idx, .. }) => {
                    let wt = self
                        .state
                        .data
                        .worktrees
                        .get(wt_idx)
                        .ok_or_else(|| "No worktree selected".to_string())?
                        .clone();
                    self.worktree_picker_target(&wt).ok_or_else(|| {
                        WorkflowTargetError::Status("Repo not found for this worktree".to_string())
                    })
                }
                None => Err(WorkflowTargetError::Status("No item selected".to_string())),
            }
        } else if self.state.view == View::WorkflowRunDetail {
            let run = self
                .state
                .selected_workflow_run_id
                .as_ref()
                .and_then(|id| self.state.data.workflow_runs.iter().find(|r| &r.id == id))
                .ok_or_else(|| "No workflow run selected".to_string())?
                .clone();
            self.workflow_run_picker_target(&run).ok_or_else(|| {
                WorkflowTargetError::Status(
                    "Cannot determine repo for this workflow run".to_string(),
                )
            })
        } else if self.state.view == View::RepoDetail
            && self.state.repo_detail_focus == RepoDetailFocus::Tickets
        {
            let ticket = self
                .state
                .filtered_detail_tickets
                .get(self.state.detail_ticket_index)
                .ok_or_else(|| "No ticket selected".to_string())?
                .clone();
            let repo_path = self
                .state
                .data
                .repos
                .iter()
                .find(|r| r.id == ticket.repo_id)
                .map(|r| r.local_path.clone())
                .ok_or_else(|| {
                    WorkflowTargetError::Modal(
                        "Cannot run workflow: ticket's repository is not registered in Conductor."
                            .to_string(),
                    )
                })?;
            Ok(WorkflowPickerTarget::Ticket {
                ticket_id: ticket.id.clone(),
                ticket_title: ticket.title.clone(),
                ticket_url: ticket.url.clone(),
                repo_path,
                repo_id: ticket.repo_id.clone(),
            })
        } else {
            Err(WorkflowTargetError::Status(
                "No valid target for workflow in this context".to_string(),
            ))
        }
    }

    /// Open a workflow picker appropriate for the current context.
    pub(super) fn handle_pick_workflow(&mut self) {
        use crate::state::WorkflowPickerTarget;

        let target = match self.resolve_workflow_target() {
            Ok(t) => t,
            Err(WorkflowTargetError::Status(msg)) => {
                self.state.status_message = Some(msg);
                return;
            }
            Err(WorkflowTargetError::Modal(msg)) => {
                self.state.modal = Modal::Error { message: msg };
                return;
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
    pub(super) fn handle_workflow_picker_confirm(&mut self) {
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

        let mut prefill = std::collections::HashMap::new();
        if let crate::state::WorkflowPickerTarget::Worktree {
            ref worktree_id, ..
        } = target
        {
            if let Some(wt) = self
                .state
                .data
                .worktrees
                .iter()
                .find(|w| &w.id == worktree_id)
            {
                if let Some(tid) = &wt.ticket_id {
                    prefill.insert("ticket_id".to_string(), tid.clone());
                }
            }
        } else if let crate::state::WorkflowPickerTarget::WorkflowRun {
            ref workflow_run_id,
            ..
        } = target
        {
            prefill.insert("workflow_run_id".to_string(), workflow_run_id.clone());
        }
        self.show_workflow_inputs_or_run(target, def, prefill);
    }

    pub(super) fn handle_run_workflow(&mut self) {
        use crate::state::WorkflowPickerTarget;

        // Resolve target using the consolidated method
        let target = match self.resolve_workflow_target() {
            Ok(t) => t,
            Err(WorkflowTargetError::Status(msg)) => {
                self.state.status_message = Some(msg);
                return;
            }
            Err(WorkflowTargetError::Modal(msg)) => {
                self.state.modal = Modal::Error { message: msg };
                return;
            }
        };

        // For WorkflowRun targets, look up the workflow def by name from the
        // run rather than using the currently-highlighted index, which may
        // point to an unrelated workflow.
        let def = if let WorkflowPickerTarget::WorkflowRun {
            ref workflow_name, ..
        } = target
        {
            match self
                .state
                .data
                .workflow_defs
                .iter()
                .find(|d| &d.name == workflow_name)
            {
                Some(d) => d.clone(),
                None => {
                    self.state.status_message =
                        Some(format!("Workflow definition '{}' not found", workflow_name));
                    return;
                }
            }
        } else {
            match self
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
            }
        };

        // Block if a workflow run is already active on the target worktree
        if let WorkflowPickerTarget::Worktree {
            ref worktree_id, ..
        } = target
        {
            use conductor_core::workflow::WorkflowManager;
            let wf_mgr = WorkflowManager::new(&self.conn);
            match wf_mgr.get_active_run_for_worktree(worktree_id) {
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

        let mut prefill = std::collections::HashMap::new();
        if let WorkflowPickerTarget::Worktree {
            ref worktree_id, ..
        } = target
        {
            if let Some(wt) = self
                .state
                .data
                .worktrees
                .iter()
                .find(|w| &w.id == worktree_id)
            {
                if let Some(tid) = &wt.ticket_id {
                    prefill.insert("ticket_id".to_string(), tid.clone());
                }
            }
        } else if let WorkflowPickerTarget::WorkflowRun {
            ref workflow_run_id,
            ..
        } = target
        {
            prefill.insert("workflow_run_id".to_string(), workflow_run_id.clone());
        }

        self.show_workflow_inputs_or_run(target, def, prefill);
    }

    /// Show the input form if the workflow declares inputs, otherwise dispatch immediately.
    /// This is the shared entry point from both `handle_workflow_picker_confirm` and
    /// `handle_pr_workflow_picker_confirm`.
    pub(super) fn show_workflow_inputs_or_run(
        &mut self,
        target: crate::state::WorkflowPickerTarget,
        def: conductor_core::workflow::WorkflowDef,
        prefill: std::collections::HashMap<String, String>,
    ) {
        use conductor_core::workflow::ENGINE_INJECTED_KEYS;

        if !def.inputs.is_empty() {
            let mut fields = build_form_fields(&def.inputs);
            for field in &mut fields {
                if let Some(v) = prefill.get(&field.label) {
                    field.value = v.clone();
                    field.manually_edited = true;
                }
            }
            // Mark engine-injected fields as readonly when they have been pre-populated
            for field in &mut fields {
                if ENGINE_INJECTED_KEYS.contains(&field.label.as_str()) && !field.value.is_empty() {
                    field.readonly = true;
                    field.manually_edited = false;
                }
            }
            // Start cursor on the first editable field (or 0 as fallback)
            let first_editable = fields.iter().position(|f| !f.readonly).unwrap_or(0);
            self.state.modal = Modal::Form {
                title: format!("Inputs for '{}'", def.name),
                fields,
                active_field: first_editable,
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

    pub(super) fn submit_run_workflow_with_inputs(
        &mut self,
        inputs: std::collections::HashMap<String, String>,
        target: crate::state::WorkflowPickerTarget,
        def: conductor_core::workflow::WorkflowDef,
    ) {
        use crate::state::WorkflowPickerTarget;

        // Active-run check must happen before showing the model picker
        if let WorkflowPickerTarget::Worktree {
            ref worktree_id, ..
        } = target
        {
            if self.active_run_blocks_dispatch(worktree_id) {
                return;
            }
        }

        // Resolve the effective model from the per-worktree → per-repo → global config chain
        let (effective_default, effective_source) = match &target {
            WorkflowPickerTarget::Worktree { worktree_id, .. } => {
                self.resolve_model_for_worktree(worktree_id)
            }
            _ => match self.config.general.model.clone() {
                Some(m) => (Some(m), "global config".to_string()),
                None => (None, "not set".to_string()),
            },
        };

        self.state.modal = Modal::ModelPicker {
            context_label: format!("workflow: {}", def.name),
            effective_default,
            effective_source,
            selected: 0, // "Default" row pre-selected
            custom_input: String::new(),
            custom_active: false,
            suggested: None,
            allow_default: true,
            on_submit: crate::state::InputAction::WorkflowModelOverride {
                action: Box::new(crate::state::RunWorkflowAction {
                    target,
                    workflow_def: def,
                }),
                inputs,
            },
        };
    }

    /// Dispatch a workflow to the appropriate spawn function based on the target type.
    /// Called from the WorkflowModelOverride input handler after the model picker is submitted.
    pub(super) fn do_dispatch_workflow(
        &mut self,
        target: crate::state::WorkflowPickerTarget,
        def: conductor_core::workflow::WorkflowDef,
        inputs: std::collections::HashMap<String, String>,
        model: Option<String>,
    ) {
        use crate::state::WorkflowPickerTarget;

        match target {
            WorkflowPickerTarget::Worktree {
                worktree_id,
                worktree_path,
                repo_path,
            } => {
                // Re-check active run at dispatch time to close the race window between
                // the model picker being shown and the user submitting their selection.
                if self.active_run_blocks_dispatch(&worktree_id) {
                    return;
                }

                let (wt_target_label, wt_ticket_id, repo_slug, wt_slug) = self
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
                            .map(|r| {
                                (
                                    format!("{}/{}", r.slug, w.slug),
                                    w.ticket_id.clone(),
                                    Some(r.slug.clone()),
                                    Some(w.slug.clone()),
                                )
                            })
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
                    model,
                    repo_slug,
                    wt_slug,
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
                self.spawn_pr_workflow_in_background(pr_ref, def, inputs, model);
            }
            WorkflowPickerTarget::Ticket {
                ticket_id,
                ticket_title,
                repo_id,
                repo_path,
                ..
            } => {
                // Feature resolution happens off-thread inside spawn_ticket_workflow_in_background.
                self.spawn_ticket_workflow_in_background(
                    def,
                    ticket_id,
                    repo_id,
                    repo_path,
                    ticket_title,
                    inputs,
                    model,
                );
            }
            WorkflowPickerTarget::Repo {
                repo_id,
                repo_path,
                repo_name,
            } => {
                self.spawn_repo_workflow_in_background(
                    def, repo_id, repo_path, repo_name, inputs, model,
                );
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
                    // Look up worktree data for feature auto-detection (resolved off-thread).
                    let (repo_slug, ticket_id, wt_slug) = self
                        .state
                        .data
                        .worktrees
                        .iter()
                        .find(|w| w.id == wt_id)
                        .map(|w| {
                            let repo = self.state.data.repos.iter().find(|r| r.id == w.repo_id);
                            (
                                repo.map(|r| r.slug.clone()),
                                w.ticket_id.clone(),
                                Some(w.slug.clone()),
                            )
                        })
                        .unwrap_or((None, None, None));
                    self.spawn_workflow_in_background(
                        def,
                        wt_id,
                        working_dir,
                        repo_path,
                        ticket_id,
                        run_inputs,
                        format!("workflow_run:{workflow_run_id}"),
                        model,
                        repo_slug,
                        wt_slug,
                    );
                } else {
                    self.spawn_workflow_run_target_in_background(
                        def,
                        repo_path,
                        run_inputs,
                        workflow_run_id,
                        model,
                    );
                }
            }
        }
    }

    /// Spawn a workflow execution in a background thread, reporting result via bg_tx.
    ///
    /// Feature auto-detection is performed off-thread using `repo_slug`,
    /// `ticket_id`, and `wt_slug` to avoid blocking the TUI main thread with
    /// synchronous DB queries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_workflow_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        worktree_id: String,
        worktree_path: String,
        repo_path: String,
        ticket_id: Option<String>,
        inputs: std::collections::HashMap<String, String>,
        target_label: String,
        model: Option<String>,
        repo_slug: Option<String>,
        wt_slug: Option<String>,
    ) {
        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let shutdown = Arc::clone(&self.workflow_shutdown);

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let feature_id = match resolve_feature_id_for_bg(
                &config,
                None,
                repo_slug.as_deref(),
                ticket_id.as_deref(),
                wt_slug.as_deref(),
            ) {
                Ok(id) => id,
                Err(e) => {
                    if let Some(ref tx) = bg_tx {
                        tx.send(crate::action::Action::BackgroundError { message: e });
                    }
                    return;
                }
            };

            let params = WorkflowExecStandalone {
                config,
                workflow: def.clone(),
                worktree_id: Some(worktree_id),
                working_dir: worktree_path,
                repo_path,
                ticket_id,
                repo_id: None,
                model,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(target_label),
                feature_id,
                run_id_notify: None,
                triggered_by_hook: false,
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, None, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!("Starting workflow '{workflow_name}'…"));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_ticket_workflow_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        ticket_id: String,
        repo_id: String,
        repo_path: String,
        target_label: String,
        inputs: std::collections::HashMap<String, String>,
        model: Option<String>,
    ) {
        let config = self.config.clone();
        let bg_tx = self.bg_tx.clone();
        let workflow_name = def.name.clone();
        let shutdown = Arc::clone(&self.workflow_shutdown);

        let handle = std::thread::spawn(move || {
            use conductor_core::workflow::{
                execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone,
            };

            let feature_id =
                match resolve_feature_id_for_bg(&config, None, None, Some(&ticket_id), None) {
                    Ok(id) => id,
                    Err(e) => {
                        if let Some(ref tx) = bg_tx {
                            tx.send(crate::action::Action::BackgroundError { message: e });
                        }
                        return;
                    }
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
                model,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(target_label),
                feature_id,
                run_id_notify: None,
                triggered_by_hook: false,
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, None, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!("Starting workflow '{workflow_name}' on ticket…"));
    }

    pub(super) fn spawn_repo_workflow_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        repo_id: String,
        repo_path: String,
        repo_name: String,
        inputs: std::collections::HashMap<String, String>,
        model: Option<String>,
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
                model,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(repo_name),
                feature_id: None,
                run_id_notify: None,
                triggered_by_hook: false,
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, None, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!("Starting workflow '{workflow_name}' on repo…"));
    }

    pub(super) fn spawn_workflow_run_target_in_background(
        &mut self,
        def: conductor_core::workflow::WorkflowDef,
        repo_path: String,
        inputs: std::collections::HashMap<String, String>,
        target_label: String,
        model: Option<String>,
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
                model,
                exec_config: WorkflowExecConfig {
                    shutdown: Some(shutdown),
                    ..WorkflowExecConfig::default()
                },
                inputs,
                target_label: Some(target_label),
                feature_id: None,
                run_id_notify: None,
                triggered_by_hook: false,
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            };

            let result = execute_workflow_standalone(&params);

            send_workflow_result(&bg_tx, &def.name, None, result);
        });

        self.workflow_threads.push(handle);
        self.state.status_message = Some(format!(
            "Starting workflow '{workflow_name}' on workflow run…"
        ));
    }

    pub(super) fn handle_run_pr_workflow(&mut self) {
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

    pub(super) fn handle_pr_workflow_picker_confirm(&mut self) {
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
    pub(super) fn spawn_pr_workflow_in_background(
        &mut self,
        pr_ref: conductor_core::workflow_ephemeral::PrRef,
        def: conductor_core::workflow::WorkflowDef,
        inputs: std::collections::HashMap<String, String>,
        model: Option<String>,
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
            use conductor_core::workflow::WorkflowExecConfig;
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
                model.as_deref(),
                exec_config,
                inputs,
                false,
                conductor_core::workflow::resolve_conductor_bin_dir(),
            );

            send_workflow_result(&bg_tx, &workflow_name, Some(&pr_label), result);
        });

        self.workflow_threads.push(handle);
    }

    pub(super) fn handle_resume_workflow(&mut self) {
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

    pub(super) fn handle_resume_worktree_workflow(&mut self) {
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

    pub(super) fn handle_cancel_workflow(&mut self) {
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

    pub(super) fn handle_approve_gate(&mut self) {
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

    pub(super) fn handle_reject_gate(&mut self) {
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
    pub(super) fn handle_view_workflow_def(&mut self) {
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

    /// Open the selected workflow definition's source file in $EDITOR.
    pub(super) fn handle_edit_workflow_def(&mut self) {
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

    /// Return `(worktree_path, repo_local_path)` for the given worktree ID,
    /// or `None` if the worktree (or its repo) is not found in the data cache.
    pub(super) fn resolve_worktree_paths(&self, wt_id: &str) -> Option<(String, String)> {
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
    pub(super) fn selected_workflow_def(&self) -> Option<conductor_core::workflow::WorkflowDef> {
        self.state
            .data
            .workflow_defs
            .get(self.state.workflow_def_index)
            .cloned()
    }

    /// Resolve a worktree to a `WorkflowPickerTarget::Worktree`, looking up the repo path from
    /// `self.state.data.repos`. Returns `None` if the repo is not found.
    pub(super) fn worktree_picker_target(
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
    pub(super) fn repo_picker_target(
        &self,
        repo: &conductor_core::repo::Repo,
    ) -> crate::state::WorkflowPickerTarget {
        crate::state::WorkflowPickerTarget::Repo {
            repo_id: repo.id.clone(),
            repo_path: repo.local_path.clone(),
            repo_name: repo.slug.clone(),
        }
    }

    /// Build a `WorkflowPickerTarget::WorkflowRun` from a `WorkflowRun`, resolving repo_path.
    pub(super) fn workflow_run_picker_target(
        &self,
        run: &conductor_core::workflow::WorkflowRun,
    ) -> Option<crate::state::WorkflowPickerTarget> {
        // Resolve repo_id: try in-memory worktrees first, then DB fallback for
        // deleted/merged worktrees, then the run's own repo_id field.
        let resolved_repo_id = if let Some(wt_id) = &run.worktree_id {
            self.state
                .data
                .worktrees
                .iter()
                .find(|w| &w.id == wt_id)
                .map(|wt| wt.repo_id.clone())
                .or_else(|| {
                    // Worktree not in memory (deleted/merged) — look up via manager.
                    WorktreeManager::new(&self.conn, &self.config)
                        .get_by_id(wt_id)
                        .ok()
                        .map(|wt| wt.repo_id)
                })
                .or(run.repo_id.clone())
        } else {
            run.repo_id.clone()
        };
        let repo_path = resolved_repo_id.as_ref().and_then(|repo_id| {
            self.state
                .data
                .repos
                .iter()
                .find(|r| &r.id == repo_id)
                .map(|r| r.local_path.clone())
        })?;
        let worktree_path = run.worktree_id.as_ref().and_then(|wt_id| {
            self.state
                .data
                .worktrees
                .iter()
                .find(|w| &w.id == wt_id)
                .map(|w| w.path.clone())
        });
        Some(crate::state::WorkflowPickerTarget::WorkflowRun {
            workflow_run_id: run.id.clone(),
            workflow_name: run.workflow_name.clone(),
            worktree_id: run.worktree_id.clone(),
            worktree_path,
            repo_path,
        })
    }

    /// Check whether a workflow is already active for `worktree_id`.
    ///
    /// Returns `true` and sets `self.state.status_message` when the check detects
    /// an active run (or fails), signalling the caller to abort the dispatch.
    /// Returns `false` when no active run is found and dispatch may proceed.
    fn active_run_blocks_dispatch(&mut self, worktree_id: &str) -> bool {
        use conductor_core::workflow::WorkflowManager;
        let wf_mgr = WorkflowManager::new(&self.conn);
        match wf_mgr.get_active_run_for_worktree(worktree_id) {
            Ok(Some(active)) => {
                self.state.status_message = Some(format!(
                    "Workflow '{}' is already running — cancel it before starting another",
                    active.workflow_name
                ));
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.state.status_message =
                    Some(format!("Failed to check active workflow run: {e}"));
                true
            }
        }
    }

    /// Resolve the effective model for `worktree_id` using the
    /// per-worktree → per-repo → global config precedence chain.
    ///
    /// Returns `(model, source_label)` where `source_label` is one of
    /// `"worktree"`, `"repo"`, `"global config"`, or `"not set"`.
    pub(super) fn resolve_model_for_worktree(&self, worktree_id: &str) -> (Option<String>, String) {
        let wt = self
            .state
            .data
            .worktrees
            .iter()
            .find(|w| w.id == worktree_id);
        let wt_model = wt.and_then(|w| w.model.clone());
        let repo_model = wt
            .and_then(|w| self.state.data.repos.iter().find(|r| r.id == w.repo_id))
            .and_then(|r| r.model.clone());
        let is_wt = wt_model.is_some();
        let is_repo = !is_wt && repo_model.is_some();
        let model = conductor_core::models::resolve_model(
            wt_model.as_deref(),
            repo_model.as_deref(),
            self.config.general.model.as_deref(),
        );
        match model {
            Some(m) => {
                let source = if is_wt {
                    "worktree"
                } else if is_repo {
                    "repo"
                } else {
                    "global config"
                };
                (Some(m), source.to_string())
            }
            None => (None, "not set".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use conductor_core::{config::Config, repo::Repo, worktree::Worktree};

    fn make_app() -> App {
        let conn = conductor_core::test_helpers::create_test_conn();
        App::new(conn, Config::default(), Theme::default())
    }

    fn make_worktree(id: &str, repo_id: &str, model: Option<&str>) -> Worktree {
        Worktree {
            id: id.to_string(),
            repo_id: repo_id.to_string(),
            slug: "feat-test".to_string(),
            branch: "feat/test".to_string(),
            path: "/tmp/wt".to_string(),
            ticket_id: None,
            status: conductor_core::worktree::WorktreeStatus::Active,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            completed_at: None,
            model: model.map(String::from),
            base_branch: None,
        }
    }

    fn make_repo(id: &str, model: Option<&str>) -> Repo {
        Repo {
            id: id.to_string(),
            slug: "test-repo".to_string(),
            local_path: "/tmp/repo".to_string(),
            remote_url: "https://github.com/test/repo.git".to_string(),
            default_branch: "main".to_string(),
            workspace_dir: "/tmp/ws".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            model: model.map(String::from),
            allow_agent_issue_creation: false,
        }
    }

    #[test]
    fn resolve_model_worktree_level_wins_over_repo_and_global() {
        let mut app = make_app();
        app.config.general.model = Some("haiku".to_string());
        app.state.data.repos = vec![make_repo("r1", Some("sonnet"))];
        app.state.data.worktrees = vec![make_worktree("w1", "r1", Some("opus"))];
        let (model, source) = app.resolve_model_for_worktree("w1");
        assert_eq!(model.as_deref(), Some("opus"));
        assert_eq!(source, "worktree");
    }

    #[test]
    fn resolve_model_repo_level_when_no_worktree_model() {
        let mut app = make_app();
        app.config.general.model = Some("haiku".to_string());
        app.state.data.repos = vec![make_repo("r1", Some("sonnet"))];
        app.state.data.worktrees = vec![make_worktree("w1", "r1", None)];
        let (model, source) = app.resolve_model_for_worktree("w1");
        assert_eq!(model.as_deref(), Some("sonnet"));
        assert_eq!(source, "repo");
    }

    #[test]
    fn resolve_model_global_config_fallback_when_no_wt_or_repo_model() {
        let mut app = make_app();
        app.config.general.model = Some("haiku".to_string());
        app.state.data.repos = vec![make_repo("r1", None)];
        app.state.data.worktrees = vec![make_worktree("w1", "r1", None)];
        let (model, source) = app.resolve_model_for_worktree("w1");
        assert_eq!(model.as_deref(), Some("haiku"));
        assert_eq!(source, "global config");
    }

    #[test]
    fn resolve_model_not_set_when_nothing_configured() {
        let mut app = make_app();
        app.state.data.repos = vec![make_repo("r1", None)];
        app.state.data.worktrees = vec![make_worktree("w1", "r1", None)];
        let (model, source) = app.resolve_model_for_worktree("w1");
        assert!(model.is_none());
        assert_eq!(source, "not set");
    }

    #[test]
    fn resolve_model_unknown_worktree_id_returns_not_set() {
        let mut app = make_app();
        app.state.data.repos = vec![make_repo("r1", Some("opus"))];
        app.state.data.worktrees = vec![make_worktree("w1", "r1", Some("sonnet"))];
        let (model, source) = app.resolve_model_for_worktree("nonexistent");
        assert!(model.is_none());
        assert_eq!(source, "not set");
    }

    #[test]
    fn workflow_run_target_prefills_workflow_run_id_readonly() {
        use conductor_core::workflow::{InputDecl, InputType, WorkflowDef, WorkflowTrigger};

        let mut app = make_app();

        let target = crate::state::WorkflowPickerTarget::WorkflowRun {
            workflow_run_id: "run-123".into(),
            workflow_name: "test-wf".into(),
            worktree_id: None,
            worktree_path: None,
            repo_path: "/tmp/repo".into(),
        };

        let def = WorkflowDef {
            name: "test-wf".into(),
            description: "test".into(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            inputs: vec![InputDecl {
                name: "workflow_run_id".into(),
                required: false,
                default: None,
                description: None,
                input_type: InputType::String,
            }],
            body: vec![],
            always: vec![],
            source_path: "test.wf".into(),
        };

        let mut prefill = std::collections::HashMap::new();
        prefill.insert("workflow_run_id".to_string(), "run-123".to_string());
        app.show_workflow_inputs_or_run(target, def, prefill);

        match &app.state.modal {
            Modal::Form {
                fields,
                active_field,
                ..
            } => {
                let field = fields
                    .iter()
                    .find(|f| f.label == "workflow_run_id")
                    .unwrap();
                assert_eq!(field.value, "run-123");
                assert!(field.readonly, "workflow_run_id should be readonly");
                // active_field should not point to a readonly field
                assert!(!fields[*active_field].readonly || fields.iter().all(|f| f.readonly));
            }
            other => panic!("Expected Modal::Form, got {:?}", other),
        }
    }
}
