use conductor_core::github;
use conductor_core::issue_source::IssueSourceManager;
use conductor_core::repo::{derive_local_path, RepoManager};

use crate::action::Action;
use crate::state::{
    ConfirmAction, FormAction, FormField, FormFieldType, InputAction, Modal, RepoDetailFocus, View,
};

use super::helpers::derive_worktree_slug;
use super::App;

impl App {
    pub(super) fn handle_create(&mut self) {
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

    pub(super) fn handle_register_repo(&mut self) {
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
                    readonly: false,
                    field_type: FormFieldType::Text,
                },
                FormField {
                    label: "Slug".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from URL".to_string(),
                    manually_edited: false,
                    required: true,
                    readonly: false,
                    field_type: FormFieldType::Text,
                },
                FormField {
                    label: "Local Path".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from slug".to_string(),
                    manually_edited: false,
                    required: false,
                    readonly: false,
                    field_type: FormFieldType::Text,
                },
            ],
            active_field: 0,
            on_submit: FormAction::RegisterRepo,
        };
    }

    pub(super) fn submit_register_repo(&mut self, fields: Vec<FormField>) {
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

    pub(super) fn submit_add_issue_source(
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

    pub(super) fn handle_manage_issue_sources(&mut self) {
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

    pub(super) fn handle_issue_source_add(&mut self) {
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
                readonly: false,
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

    pub(super) fn handle_issue_source_delete(&mut self) {
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

    pub(super) fn show_delete_worktree_modal(
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

    pub(super) fn handle_delete(&mut self) {
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

    pub(super) fn handle_link_ticket(&mut self) {
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
}
