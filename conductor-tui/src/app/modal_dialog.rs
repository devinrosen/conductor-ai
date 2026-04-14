use conductor_core::issue_source::IssueSourceManager;
use conductor_core::repo::RepoManager;
use conductor_core::worktree::{WorktreeCreateOptions, WorktreeManager};

use crate::action::Action;
use crate::state::{ConfirmAction, Modal};

use super::App;

impl App {
    pub(super) fn handle_confirm_yes(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::Confirm { on_confirm, .. } = modal {
            self.execute_confirm_action(on_confirm);
        }
    }

    pub(super) fn execute_confirm_action(&mut self, on_confirm: ConfirmAction) {
        match on_confirm {
            ConfirmAction::CreateWorktree {
                repo_slug,
                wt_name,
                ticket_id,
                from_pr,
                from_branch,
                force_dirty,
            } => {
                self.spawn_worktree_create(
                    repo_slug,
                    wt_name,
                    WorktreeCreateOptions {
                        ticket_id,
                        from_pr,
                        from_branch,
                        force_dirty,
                        ..Default::default()
                    },
                );
            }
            ConfirmAction::DeleteWorktree { repo_slug, wt_slug } => {
                let Some(bg_tx) = self.require_bg_tx() else {
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
                let Some(bg_tx) = self.require_bg_tx() else {
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
                let Some(bg_tx) = self.require_bg_tx() else {
                    return;
                };
                let run_id = workflow_run_id.clone();
                self.state.modal = Modal::Progress {
                    message: "Cancelling workflow…".to_string(),
                };
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<()> {
                        let db = conductor_core::config::db_path();
                        let conn = conductor_core::db::open_database(&db)?;
                        use conductor_core::workflow::WorkflowManager;
                        let mgr = WorkflowManager::new(&conn);
                        mgr.cancel_run(&run_id, "Cancelled by user")
                            .map_err(anyhow::Error::from)
                    })();
                    let _ = bg_tx.send(Action::WorkflowCancelComplete {
                        result: result.map_err(|e| e.to_string()),
                    });
                });
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
                        conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
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
            ConfirmAction::ClearConversation {
                repo_slug,
                wt_slug,
                wt_id,
            } => {
                let Some(bg_tx) = self.require_bg_tx() else {
                    return;
                };
                self.state.modal = Modal::Progress {
                    message: "Clearing conversation…".to_string(),
                };
                std::thread::spawn(move || {
                    use conductor_core::conversation::{ConversationManager, ConversationScope};
                    let result = (|| -> anyhow::Result<()> {
                        let db = conductor_core::config::db_path();
                        let conn = conductor_core::db::open_database(&db)?;
                        let conv_mgr = ConversationManager::new(&conn);
                        conv_mgr
                            .clear_for_scope(&ConversationScope::Worktree, &wt_id)
                            .map_err(anyhow::Error::from)
                    })();
                    let _ = bg_tx.send(crate::action::Action::ClearConversationComplete {
                        repo_slug,
                        wt_slug,
                        result: result.map_err(|e| e.to_string()),
                    });
                });
            }
            ConfirmAction::Quit => {
                self.state.should_quit = true;
            }
        }
    }

    pub(super) fn show_confirm_quit(&mut self) {
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
}
