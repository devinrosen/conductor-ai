use std::sync::Arc;

use conductor_core::agent::{AgentManager, AgentRun, FeedbackRequest};
use conductor_core::config::AutoStartAgent;
use conductor_core::tickets::build_agent_prompt;
use conductor_core::worktree::{WorktreeCreateOptions, WorktreeManager};

use crate::action::Action;
use crate::state::{InputAction, Modal, WorkflowPickerItem};

use super::App;

impl App {
    pub(super) fn handle_toggle_agent_issues(&mut self) {
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

    pub(super) fn selected_worktree_run(&self) -> Option<&AgentRun> {
        self.state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.latest_agent_runs.get(id))
    }

    pub(super) fn refresh_pending_feedback(&mut self) {
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
    pub(super) fn agent_busy_guard(&mut self, worktree_id: &str) -> bool {
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

    pub(super) fn handle_launch_agent(&mut self) {
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

    /// Stop the running worktree agent.
    /// Runs blocking subprocess calls on a background thread per the TUI threading rule.
    pub(super) fn handle_stop_agent(&mut self) {
        let run = self.selected_worktree_run().cloned();

        let Some(run) = run else {
            return;
        };

        if !run.is_active() {
            return;
        }

        let Some(ref tx) = self.bg_tx else { return };
        let tx = tx.clone();
        let run_id = run.id.clone();
        let subprocess_pid = run.subprocess_pid;

        self.state.modal = crate::state::Modal::Progress {
            message: "Stopping agent…".into(),
        };

        std::thread::spawn(move || {
            let db = conductor_core::config::db_path();
            let conn = match conductor_core::db::open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Action::AgentStopComplete {
                        result: Err(format!("Failed to open database: {e}")),
                    });
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);

            let result = mgr
                .cancel_run(&run_id, subprocess_pid)
                .map(|()| "Agent cancelled".to_string())
                .map_err(|e| format!("Failed to cancel agent: {e}"));

            let _ = tx.send(Action::AgentStopComplete { result });
        });
    }

    pub(super) fn require_pending_feedback(&mut self) -> Option<FeedbackRequest> {
        match self.state.data.pending_feedback.clone() {
            Some(fb) => Some(fb),
            None => {
                self.state.status_message = Some("No pending feedback request".to_string());
                None
            }
        }
    }

    pub(super) fn handle_submit_feedback(&mut self) {
        let Some(fb) = self.require_pending_feedback() else {
            return;
        };
        self.open_feedback_modal(&fb, "Agent Feedback");
    }

    /// Open the feedback modal for a given request, with a configurable title prefix.
    fn open_feedback_modal(&mut self, fb: &FeedbackRequest, title_prefix: &str) {
        use conductor_core::agent::FeedbackType;

        let format_opts = |opts: &[conductor_core::agent::FeedbackOption]| -> String {
            opts.iter()
                .enumerate()
                .map(|(i, o)| format!("{}. {}", i + 1, o.label))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let placeholder = match fb.feedback_type {
            FeedbackType::Confirm => "Type y or n...".to_string(),
            FeedbackType::SingleSelect => {
                let opts_text = fb.options.as_deref().map(format_opts).unwrap_or_default();
                format!("Type the number of your choice:\n{opts_text}")
            }
            FeedbackType::MultiSelect => {
                let opts_text = fb.options.as_deref().map(format_opts).unwrap_or_default();
                format!("Type numbers separated by commas (e.g. 1,3):\n{opts_text}")
            }
            FeedbackType::Text => "Type your feedback response...".to_string(),
        };

        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_placeholder_text(&placeholder);

        self.state.modal = Modal::AgentPrompt {
            title: format!("{title_prefix}: {}", &fb.prompt),
            prompt: fb.prompt.clone(),
            textarea: Box::new(textarea),
            on_submit: InputAction::FeedbackResponse {
                feedback_id: fb.id.clone(),
            },
        };
    }

    pub(super) fn handle_dismiss_feedback(&mut self) {
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

    pub(super) fn handle_orchestrate_agent(&mut self) {
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

    pub(super) fn start_orchestrate_headless(
        &mut self,
        prompt: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
    ) {
        let Some(ref tx) = self.bg_tx else { return };
        let tx = tx.clone();

        // Resolve model on main thread (pure in-memory lookup)
        let (model, _) = self.resolve_model_for_worktree(&worktree_id);

        self.state.modal = Modal::Progress {
            message: "Launching orchestrator…".into(),
        };

        std::thread::spawn(move || {
            let db = conductor_core::config::db_path();
            let conn = match conductor_core::db::open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Action::OrchestrateLaunchComplete {
                        result: Err(e.to_string()),
                    });
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);

            let run = match mgr.create_run(
                Some(&worktree_id),
                &prompt,
                Some(&worktree_slug),
                model.as_deref(),
            ) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Action::OrchestrateLaunchComplete {
                        result: Err(format!("Failed to create agent run: {e}")),
                    });
                    return;
                }
            };

            let args = conductor_core::agent_runtime::build_orchestrate_args(
                &run.id,
                &worktree_path,
                model.as_deref(),
                false,
                None,
            );

            let handle = match conductor_core::agent_runtime::spawn_headless(
                &args,
                std::path::Path::new(&worktree_path),
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = mgr.update_run_failed(&run.id, &e);
                    let _ = tx.send(Action::OrchestrateLaunchComplete { result: Err(e) });
                    return;
                }
            };

            if let Err(e) = mgr.update_run_subprocess_pid(&run.id, handle.pid) {
                tracing::warn!("failed to store subprocess PID for run {}: {e}", run.id);
            }

            let _ = tx.send(Action::OrchestrateLaunchComplete {
                result: Ok("Orchestrator launched (headless)".to_string()),
            });

            let run_id = run.id.clone();
            let log_path = conductor_core::config::agent_log_path(&run_id);
            let tx2 = tx.clone();
            conductor_core::agent_runtime::drain_stream_json(
                handle.stdout,
                &run_id,
                &log_path,
                &mgr,
                |event| {
                    let _ = tx2.send(Action::AgentEvent {
                        run_id: run_id.clone(),
                        event: event.clone(),
                    });
                },
            );

            drop(handle.stderr);
            let mut child = handle.child;
            let _ = child.wait();
            let _ = tx.send(Action::AgentComplete { run_id });
        });
    }

    pub(super) fn spawn_worktree_create(
        &mut self,
        repo_slug: String,
        name: String,
        opts: WorktreeCreateOptions,
    ) {
        // Guard before setting the non-dismissable Progress modal: if bg_tx is
        // None (only possible before init() completes), show an error rather than
        // permanently locking the UI with no recovery path.
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: super::BG_TX_NOT_READY.into(),
            };
            return;
        };
        self.state.modal = Modal::Progress {
            message: if opts.from_pr.is_some() {
                "Fetching PR branch…".to_string()
            } else {
                "Creating worktree…".to_string()
            },
        };
        let ticket_id = opts.ticket_id.clone();
        let config = self.config.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let db = conductor_core::config::db_path();
                let conn = conductor_core::db::open_database(&db)?;
                let wt_mgr = WorktreeManager::new(&conn, &config);
                let (wt, warnings) = wt_mgr.create(&repo_slug, &name, opts)?;

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

    pub(super) fn maybe_start_agent_for_worktree(
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

            let mut items = vec![WorkflowPickerItem::StartAgent];
            items.extend(crate::app::workflow_management::insert_group_headers(
                manual_defs,
            ));
            items.push(WorkflowPickerItem::Skip);

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

    pub(super) fn show_agent_prompt_for_ticket(
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
    pub(super) fn open_agent_prompt_modal(
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

    pub(super) fn start_agent_headless(
        &mut self,
        prompt: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
        model: Option<String>,
    ) {
        let Some(ref tx) = self.bg_tx else { return };
        let tx = tx.clone();

        self.state.modal = Modal::Progress {
            message: "Launching agent…".into(),
        };

        std::thread::spawn(move || {
            let db = conductor_core::config::db_path();
            let conn = match conductor_core::db::open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Action::AgentLaunchComplete {
                        result: Err(e.to_string()),
                    });
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);

            let run = match mgr.create_run(
                Some(&worktree_id),
                &prompt,
                Some(&worktree_slug),
                model.as_deref(),
            ) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Action::AgentLaunchComplete {
                        result: Err(format!("Failed to create agent run: {e}")),
                    });
                    return;
                }
            };

            let (args, prompt_file) = match conductor_core::agent_runtime::build_headless_agent_args(
                &run.id,
                &worktree_path,
                &prompt,
                resume_session_id.as_deref(),
                model.as_deref(),
                None,
                None,
                &[],
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = mgr.update_run_failed(&run.id, &e);
                    let _ = tx.send(Action::AgentLaunchComplete { result: Err(e) });
                    return;
                }
            };

            let handle = match conductor_core::agent_runtime::spawn_headless(
                &args,
                std::path::Path::new(&worktree_path),
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = mgr.update_run_failed(&run.id, &e);
                    let _ = std::fs::remove_file(&prompt_file);
                    let _ = tx.send(Action::AgentLaunchComplete { result: Err(e) });
                    return;
                }
            };

            if let Err(e) = mgr.update_run_subprocess_pid(&run.id, handle.pid) {
                tracing::warn!("failed to store subprocess PID for run {}: {e}", run.id);
            }

            let _ = tx.send(Action::AgentLaunchComplete {
                result: Ok("Agent launched (headless)".to_string()),
            });

            let run_id = run.id.clone();
            let log_path = conductor_core::config::agent_log_path(&run_id);
            let tx2 = tx.clone();
            conductor_core::agent_runtime::drain_stream_json(
                handle.stdout,
                &run_id,
                &log_path,
                &mgr,
                |event| {
                    let _ = tx2.send(Action::AgentEvent {
                        run_id: run_id.clone(),
                        event: event.clone(),
                    });
                },
            );

            let _ = std::fs::remove_file(&prompt_file);
            drop(handle.stderr);
            let mut child = handle.child;
            let _ = child.wait();
            let _ = tx.send(Action::AgentComplete { run_id });
        });
    }

    pub(super) fn handle_prompt_repo_agent(&mut self) {
        let repo = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
            .cloned();

        let Some(repo) = repo else {
            self.state.status_message = Some("No repo selected".to_string());
            return;
        };

        // Look up the latest repo-scoped run for session resume
        let resume_session_id = self
            .state
            .data
            .latest_repo_agent_runs
            .get(&repo.id)
            .and_then(|run| run.claude_session_id.clone());

        let title = if resume_session_id.is_some() {
            "Repo Agent (Resume)".to_string()
        } else {
            "Repo Agent (read-only)".to_string()
        };

        let lines = vec![String::new()];
        let mut textarea = tui_textarea::TextArea::new(lines);
        textarea.set_cursor_line_style(ratatui::style::Style::default());
        textarea.set_placeholder_text("Ask the repo agent a question (read-only)...");

        self.state.modal = Modal::AgentPrompt {
            title,
            prompt: "Enter prompt for Claude:".to_string(),
            textarea: Box::new(textarea),
            on_submit: InputAction::RepoAgentPrompt {
                repo_id: repo.id.clone(),
                repo_path: repo.local_path.clone(),
                repo_slug: repo.slug.clone(),
                resume_session_id,
            },
        };
    }

    pub(super) fn start_repo_agent_headless(
        &mut self,
        prompt: String,
        repo_id: String,
        repo_path: String,
        _repo_slug: String,
        resume_session_id: Option<String>,
    ) {
        let Some(ref tx) = self.bg_tx else { return };
        let tx = tx.clone();

        self.state.modal = Modal::Progress {
            message: "Launching repo agent…".into(),
        };

        std::thread::spawn(move || {
            let db = conductor_core::config::db_path();
            let conn = match conductor_core::db::open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Action::RepoAgentLaunched {
                        result: Err(e.to_string()),
                    });
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);

            let run = match mgr.create_repo_run(&repo_id, &prompt, None, None) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Action::RepoAgentLaunched {
                        result: Err(format!("Failed to create repo agent run: {e}")),
                    });
                    return;
                }
            };

            let plan_mode = conductor_core::config::AgentPermissionMode::RepoSafe;
            let (args, prompt_file) = match conductor_core::agent_runtime::build_headless_agent_args(
                &run.id,
                &repo_path,
                &prompt,
                resume_session_id.as_deref(),
                None,
                None,
                Some(&plan_mode),
                &[],
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = mgr.update_run_failed(&run.id, &e);
                    let _ = tx.send(Action::RepoAgentLaunched { result: Err(e) });
                    return;
                }
            };

            let handle = match conductor_core::agent_runtime::spawn_headless(
                &args,
                std::path::Path::new(&repo_path),
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = mgr.update_run_failed(&run.id, &e);
                    let _ = std::fs::remove_file(&prompt_file);
                    let _ = tx.send(Action::RepoAgentLaunched { result: Err(e) });
                    return;
                }
            };

            if let Err(e) = mgr.update_run_subprocess_pid(&run.id, handle.pid) {
                tracing::warn!("failed to store subprocess PID for run {}: {e}", run.id);
            }

            let _ = tx.send(Action::RepoAgentLaunched {
                result: Ok("Repo agent launched (headless)".to_string()),
            });

            let run_id = run.id.clone();
            let log_path = conductor_core::config::agent_log_path(&run_id);
            let tx2 = tx.clone();
            conductor_core::agent_runtime::drain_stream_json(
                handle.stdout,
                &run_id,
                &log_path,
                &mgr,
                |event| {
                    let _ = tx2.send(Action::AgentEvent {
                        run_id: run_id.clone(),
                        event: event.clone(),
                    });
                },
            );

            let _ = std::fs::remove_file(&prompt_file);
            drop(handle.stderr);
            let mut child = handle.child;
            let _ = child.wait();
            let _ = tx.send(Action::AgentComplete { run_id });
        });
    }

    /// Restart a failed/cancelled agent run by creating a new run with the same
    /// config and re-spawning a headless subprocess.
    /// Runs on a background thread per the TUI threading rule.
    pub(super) fn handle_restart_agent(&mut self) {
        let run = self.selected_worktree_run().cloned();

        let Some(run) = run else {
            self.state.status_message = Some("No agent run to restart".to_string());
            return;
        };

        if run.is_active() {
            self.state.status_message = Some("Agent is still active — stop it first".to_string());
            return;
        }

        // Resolve worktree path on the main thread (from cached data)
        let worktree_path = run.worktree_id.as_ref().and_then(|wt_id| {
            self.state
                .data
                .worktrees
                .iter()
                .find(|w| &w.id == wt_id)
                .map(|w| w.path.clone())
        });

        let Some(worktree_path) = worktree_path else {
            self.state.status_message =
                Some("Cannot restart: no worktree found for this run".to_string());
            return;
        };

        let Some(ref tx) = self.bg_tx else { return };
        let tx = tx.clone();
        let run_id = run.id.clone();

        self.state.modal = crate::state::Modal::Progress {
            message: "Restarting agent…".into(),
        };

        std::thread::spawn(move || {
            let db = conductor_core::config::db_path();
            let conn = match conductor_core::db::open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Action::AgentRestartComplete {
                        result: Err(e.to_string()),
                    });
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);

            let new_run = match mgr.restart_run(&run_id) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Action::AgentRestartComplete {
                        result: Err(format!("Failed to restart agent run: {e}")),
                    });
                    return;
                }
            };

            let (handle, prompt_file) = match conductor_core::agent_runtime::try_spawn_headless_run(
                &new_run.id,
                &worktree_path,
                &new_run.prompt,
                None,
                new_run.model.as_deref(),
                new_run.bot_name.as_deref(),
                None,
                &[],
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = mgr.update_run_failed(&new_run.id, &e);
                    let _ = tx.send(Action::AgentRestartComplete { result: Err(e) });
                    return;
                }
            };

            if let Err(e) = mgr.update_run_subprocess_pid(&new_run.id, handle.pid) {
                tracing::warn!("failed to store subprocess PID for run {}: {e}", new_run.id);
            }

            let _ = tx.send(Action::AgentRestartComplete {
                result: Ok("Agent restarted (headless)".to_string()),
            });

            let new_run_id = new_run.id.clone();
            let log_path = conductor_core::config::agent_log_path(&new_run_id);
            let tx2 = tx.clone();
            conductor_core::agent_runtime::drain_stream_json(
                handle.stdout,
                &new_run_id,
                &log_path,
                &mgr,
                |event| {
                    let _ = tx2.send(Action::AgentEvent {
                        run_id: new_run_id.clone(),
                        event: event.clone(),
                    });
                },
            );

            let _ = std::fs::remove_file(&prompt_file);
            drop(handle.stderr);
            let mut child = handle.child;
            let _ = child.wait();
            let _ = tx.send(Action::AgentComplete { run_id: new_run_id });
        });
    }

    /// Returns true if the current context is the repo agent pane in RepoDetail.
    pub(super) fn is_repo_agent_context(&self) -> bool {
        self.state.view == crate::state::View::RepoDetail
            && self.state.repo_detail_focus == crate::state::RepoDetailFocus::RepoAgent
    }

    /// Stop the running repo-scoped agent for the currently selected repo.
    /// Runs blocking subprocess calls on a background thread per the TUI threading rule.
    pub(super) fn handle_stop_repo_agent(&mut self) {
        let run = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.latest_repo_agent_runs.get(id))
            .cloned();

        let Some(run) = run else { return };
        if !run.is_active() {
            return;
        }

        let Some(ref tx) = self.bg_tx else { return };
        let tx = tx.clone();
        let run_id = run.id.clone();
        let subprocess_pid = run.subprocess_pid;

        self.state.modal = crate::state::Modal::Progress {
            message: "Stopping repo agent…".into(),
        };

        std::thread::spawn(move || {
            let db = conductor_core::config::db_path();
            let conn = match conductor_core::db::open_database(&db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Action::RepoAgentStopComplete {
                        result: Err(format!("Failed to open database: {e}")),
                    });
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);

            let result = mgr
                .cancel_run(&run_id, subprocess_pid)
                .map(|()| "Repo agent cancelled".to_string())
                .map_err(|e| format!("Failed to cancel repo agent: {e}"));

            let _ = tx.send(Action::RepoAgentStopComplete { result });
        });
    }

    /// Submit feedback for the repo-scoped agent.
    pub(super) fn handle_submit_repo_feedback(&mut self) {
        let Some(fb) = self.state.data.pending_repo_feedback.clone() else {
            self.state.status_message = Some("No pending feedback request".to_string());
            return;
        };
        self.open_feedback_modal(&fb, "Repo Agent Feedback");
    }

    /// Dismiss feedback for the repo-scoped agent.
    pub(super) fn handle_dismiss_repo_feedback(&mut self) {
        let Some(fb) = self.state.data.pending_repo_feedback.clone() else {
            self.state.status_message = Some("No pending feedback request".to_string());
            return;
        };

        let mgr = AgentManager::new(&self.conn);
        match mgr.dismiss_feedback(&fb.id) {
            Ok(()) => {
                self.state.status_message =
                    Some("Feedback dismissed — repo agent resumed".to_string());
                self.state.data.pending_repo_feedback = None;
                self.refresh_data();
                self.reload_repo_agent_events();
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to dismiss feedback: {e}"));
            }
        }
    }

    /// Expand a repo agent event detail modal.
    pub(super) fn handle_expand_repo_agent_event(&mut self) {
        let idx = self
            .state
            .repo_agent_list_state
            .borrow()
            .selected()
            .unwrap_or(0);
        let Some(ev) = self.state.data.repo_agent_event_at_visual_index(idx) else {
            return;
        };

        let title = format!("[{}] {}", ev.kind, ev.started_at);
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
}

// Suppress unused import for Arc — it's used indirectly via the workflow_shutdown field
#[allow(unused_imports)]
use std::sync::atomic::AtomicBool;
const _: Option<Arc<AtomicBool>> = None;
