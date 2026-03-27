use crate::state::{info_row, repo_info_row, WorktreeDetailFocus};

use super::App;

impl App {
    pub(super) fn handle_worktree_detail_copy(&mut self) {
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
                    info_row::PR => {
                        let url = self
                            .state
                            .find_pr_for_worktree(&wt.branch)
                            .map(|pr| pr.url.clone())
                            .unwrap_or_default();
                        if url.is_empty() {
                            self.state.status_message =
                                Some("No PR linked to this worktree".to_string());
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

    pub(super) fn handle_worktree_detail_open(&mut self) {
        if self.state.worktree_detail_focus != WorktreeDetailFocus::InfoPanel {
            return;
        }
        let row = self.state.worktree_detail_selected_row;
        match row {
            info_row::PATH => {
                let Some(path) = self.state.selected_worktree().map(|wt| wt.path.clone()) else {
                    return;
                };
                self.open_terminal_at_path(&path);
            }
            info_row::TICKET => {
                let url = self
                    .state
                    .selected_worktree()
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
            info_row::PR => {
                let branch = self
                    .state
                    .selected_worktree()
                    .map(|wt| wt.branch.clone())
                    .unwrap_or_default();
                let url = self
                    .state
                    .find_pr_for_worktree(&branch)
                    .map(|pr| pr.url.clone());
                match url {
                    Some(ref u) if !u.is_empty() => {
                        let u = u.clone();
                        self.open_url(&u, "PR");
                    }
                    _ => {
                        self.state.status_message =
                            Some("No PR found for this worktree's branch".to_string());
                    }
                }
            }
            _ => {
                self.state.status_message =
                    Some("No action for this row (try Path, Ticket, or PR row)".to_string());
            }
        }
    }

    pub(super) fn handle_repo_detail_info_open(&mut self) {
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

    pub(super) fn handle_repo_detail_info_copy(&mut self) {
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

    pub(super) fn handle_workflow_run_detail_copy(&mut self) {
        use crate::state::workflow_run_info_row;
        use crate::state::WorkflowRunDetailFocus;

        // When the Error pane is focused, copy the full result_summary text.
        if self.state.workflow_run_detail_focus == WorkflowRunDetailFocus::Error {
            let text = self
                .state
                .selected_workflow_run_id
                .as_ref()
                .and_then(|id| self.state.data.workflow_runs.iter().find(|r| &r.id == id))
                .and_then(|run| run.result_summary.clone());
            if let Some(text) = text {
                self.copy_text_to_clipboard(text);
            } else {
                self.state.status_message = Some("No error text to copy".to_string());
            }
            return;
        }

        let row = self.state.workflow_run_info_row;
        let run = self
            .state
            .selected_workflow_run_id
            .as_ref()
            .and_then(|id| self.state.data.workflow_runs.iter().find(|r| &r.id == id));
        let Some(run) = run.cloned() else { return };

        let worktree = run
            .worktree_id
            .as_ref()
            .and_then(|wid| self.state.data.worktrees.iter().find(|wt| &wt.id == wid));
        let ticket = worktree.and_then(|wt| {
            wt.ticket_id
                .as_ref()
                .and_then(|tid| self.state.data.ticket_map.get(tid))
        });

        let text = match row {
            workflow_run_info_row::RUN_ID => run.id.clone(),
            workflow_run_info_row::WORKFLOW => run.workflow_name.clone(),
            workflow_run_info_row::STATUS => run.status.to_string(),
            workflow_run_info_row::BRANCH => {
                if let Some(wt) = worktree {
                    wt.branch.clone()
                } else {
                    self.state.status_message = Some("No branch".to_string());
                    return;
                }
            }
            workflow_run_info_row::PATH => {
                if let Some(wt) = worktree {
                    wt.path.clone()
                } else {
                    self.state.status_message = Some("No path".to_string());
                    return;
                }
            }
            workflow_run_info_row::TICKET => {
                if let Some(t) = ticket {
                    if t.url.is_empty() {
                        t.source_id.clone()
                    } else {
                        t.url.clone()
                    }
                } else {
                    self.state.status_message = Some("No ticket".to_string());
                    return;
                }
            }
            workflow_run_info_row::STARTED => run.started_at.clone(),
            workflow_run_info_row::SUMMARY => {
                if let Some(ref s) = run.result_summary {
                    s.clone()
                } else {
                    self.state.status_message = Some("No summary".to_string());
                    return;
                }
            }
            _ => return,
        };
        self.copy_text_to_clipboard(text);
    }
}
