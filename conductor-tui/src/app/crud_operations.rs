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
                let labels: Vec<String> = serde_json::from_str(&ticket.labels).unwrap_or_default();
                let suggested = derive_worktree_slug(&ticket.source_id, &ticket.title, &labels);
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

    /// Spawn a background thread to run `check_main_health()` before creating a worktree.
    ///
    /// Shows a non-dismissable `Modal::Progress`. On completion, sends
    /// `Action::MainHealthCheckComplete` which `action_dispatch.rs` handles.
    pub(super) fn spawn_main_health_check(
        &mut self,
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
        from_pr: Option<u32>,
        from_branch: Option<String>,
    ) {
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: super::BG_TX_NOT_READY.into(),
            };
            return;
        };
        self.state.modal = Modal::Progress {
            message: "Checking main branch status\u{2026}".into(),
        };
        let config = self.config.clone();
        std::thread::spawn(move || {
            let status = (|| -> Result<_, String> {
                let db = conductor_core::config::db_path();
                let conn = conductor_core::db::open_database(&db).map_err(|e| e.to_string())?;
                conductor_core::worktree::WorktreeManager::new(&conn, &config)
                    .check_main_health(&repo_slug, from_branch.as_deref())
                    .map_err(|e| e.to_string())
            })();
            let _ = bg_tx.send(crate::action::Action::MainHealthCheckComplete {
                repo_slug,
                wt_name,
                ticket_id,
                from_pr,
                from_branch,
                status,
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FormAction, InputAction, Modal, View};

    fn make_test_app() -> App {
        let conn = conductor_core::test_helpers::create_test_conn();
        App::new(
            conn,
            conductor_core::config::Config::default(),
            crate::theme::Theme::default(),
        )
    }

    fn make_test_repo(id: &str, slug: &str) -> conductor_core::repo::Repo {
        conductor_core::repo::Repo {
            id: id.into(),
            slug: slug.into(),
            local_path: format!("/tmp/{slug}"),
            remote_url: format!("https://github.com/test/{slug}.git"),
            default_branch: "main".into(),
            workspace_dir: "/tmp".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            model: None,
            allow_agent_issue_creation: false,
        }
    }

    fn make_test_worktree(
        id: &str,
        repo_id: &str,
        slug: &str,
    ) -> conductor_core::worktree::Worktree {
        conductor_core::worktree::Worktree {
            id: id.into(),
            repo_id: repo_id.into(),
            slug: slug.into(),
            branch: format!("feat/{slug}"),
            path: format!("/tmp/ws/{slug}"),
            ticket_id: None,
            status: conductor_core::worktree::WorktreeStatus::Active,
            created_at: "2024-01-01T00:00:00Z".into(),
            completed_at: None,
            model: None,
            base_branch: None,
        }
    }

    // ── handle_create ─────────────────────────────────────────────────

    #[test]
    fn handle_create_with_selected_repo_opens_input_modal() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.selected_repo_id = Some("r1".into());
        app.state.data.repos = vec![make_test_repo("r1", "my-repo")];
        app.state
            .data
            .repo_slug_map
            .insert("r1".into(), "my-repo".into());
        app.handle_create();
        match &app.state.modal {
            Modal::Input { on_submit, .. } => match on_submit {
                InputAction::CreateWorktree {
                    repo_slug,
                    ticket_id,
                } => {
                    assert_eq!(repo_slug, "my-repo");
                    assert!(ticket_id.is_none());
                }
                other => panic!("expected CreateWorktree, got {other:?}"),
            },
            other => panic!("expected Input modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_create_no_repo_no_repos_registered_opens_register_form() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.selected_repo_id = None;
        // No repos at all
        app.handle_create();
        match &app.state.modal {
            Modal::Form { on_submit, .. } => {
                assert!(matches!(on_submit, FormAction::RegisterRepo));
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_create_no_repo_selected_with_repos_shows_status() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.state.selected_repo_id = None;
        app.state.data.repos = vec![make_test_repo("r1", "repo-a")];
        // Point dashboard_index past available rows so selected_repo() returns None
        app.state.dashboard_index = 99;
        app.handle_create();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Select a repo first")
        );
    }

    #[test]
    fn handle_create_repo_detail_with_ticket_focus_opens_ticket_aware_modal() {
        let mut app = make_test_app();
        app.state.view = View::RepoDetail;
        app.state.repo_detail_focus = RepoDetailFocus::Tickets;
        app.state.selected_repo_id = Some("r1".into());
        app.state
            .data
            .repo_slug_map
            .insert("r1".into(), "my-repo".into());

        let ticket = conductor_core::tickets::Ticket {
            id: "t1".into(),
            repo_id: "r1".into(),
            source_type: "github".into(),
            source_id: "42".into(),
            title: "Fix the bug".into(),
            body: "".into(),
            state: "open".into(),
            labels: "".into(),
            assignee: None,
            priority: None,
            url: "".into(),
            synced_at: "2024-01-01T00:00:00Z".into(),
            raw_json: "{}".into(),
            workflow: None,
            agent_map: None,
        };
        app.state.filtered_detail_tickets = vec![ticket];
        app.state.detail_ticket_index = 0;

        app.handle_create();
        match &app.state.modal {
            Modal::Input {
                on_submit, prompt, ..
            } => {
                assert!(prompt.contains("42"));
                match on_submit {
                    InputAction::CreateWorktree {
                        repo_slug,
                        ticket_id,
                    } => {
                        assert_eq!(repo_slug, "my-repo");
                        assert_eq!(ticket_id.as_deref(), Some("t1"));
                    }
                    other => panic!("expected CreateWorktree, got {other:?}"),
                }
            }
            other => panic!("expected Input modal, got {other:?}"),
        }
    }

    // ── handle_register_repo ──────────────────────────────────────────

    #[test]
    fn handle_register_repo_on_dashboard_opens_form() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.handle_register_repo();
        match &app.state.modal {
            Modal::Form {
                title,
                fields,
                on_submit,
                ..
            } => {
                assert_eq!(title, "Register Repository");
                assert_eq!(fields.len(), 3);
                assert_eq!(fields[0].label, "Remote URL");
                assert_eq!(fields[1].label, "Slug");
                assert_eq!(fields[2].label, "Local Path");
                assert!(matches!(on_submit, FormAction::RegisterRepo));
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_register_repo_on_non_dashboard_is_noop() {
        let mut app = make_test_app();
        app.state.view = View::RepoDetail;
        app.handle_register_repo();
        assert!(matches!(app.state.modal, Modal::None));
    }

    // ── submit_register_repo ──────────────────────────────────────────

    #[test]
    fn submit_register_repo_empty_fields_shows_error() {
        let mut app = make_test_app();
        let fields = vec![
            FormField {
                label: "Remote URL".into(),
                value: "".into(),
                placeholder: "".into(),
                manually_edited: false,
                required: true,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
            FormField {
                label: "Slug".into(),
                value: "".into(),
                placeholder: "".into(),
                manually_edited: false,
                required: true,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
            FormField {
                label: "Local Path".into(),
                value: "".into(),
                placeholder: "".into(),
                manually_edited: false,
                required: false,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
        ];
        app.submit_register_repo(fields);
        match &app.state.modal {
            Modal::Error { message } => {
                assert!(message.contains("required"));
            }
            other => panic!("expected Error modal, got {other:?}"),
        }
    }

    #[test]
    fn submit_register_repo_valid_fields_registers() {
        let mut app = make_test_app();
        let fields = vec![
            FormField {
                label: "Remote URL".into(),
                value: "https://github.com/test/repo.git".into(),
                placeholder: "".into(),
                manually_edited: true,
                required: true,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
            FormField {
                label: "Slug".into(),
                value: "test-repo".into(),
                placeholder: "".into(),
                manually_edited: true,
                required: true,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
            FormField {
                label: "Local Path".into(),
                value: "".into(),
                placeholder: "".into(),
                manually_edited: false,
                required: false,
                readonly: false,
                field_type: crate::state::FormFieldType::Text,
            },
        ];
        app.submit_register_repo(fields);
        let msg = app.state.status_message.as_deref().unwrap();
        assert!(msg.contains("test-repo"));
    }

    // ── handle_delete ─────────────────────────────────────────────────

    #[test]
    fn handle_delete_worktree_detail_opens_confirm() {
        let mut app = make_test_app();
        let wt = make_test_worktree("w1", "r1", "feat-test");
        app.state.data.worktrees = vec![wt];
        app.state
            .data
            .repo_slug_map
            .insert("r1".into(), "my-repo".into());
        app.state.selected_worktree_id = Some("w1".into());
        app.state.view = View::WorktreeDetail;
        app.handle_delete();
        // Should open a ConfirmByName modal (no ticket → work may be in progress)
        assert!(matches!(app.state.modal, Modal::ConfirmByName { .. }));
    }

    #[test]
    fn handle_delete_repo_detail_opens_confirm_by_name() {
        let mut app = make_test_app();
        let repo = make_test_repo("r1", "my-repo");
        app.state.data.repos = vec![repo];
        app.state.selected_repo_id = Some("r1".into());
        app.state.view = View::RepoDetail;
        app.handle_delete();
        match &app.state.modal {
            Modal::ConfirmByName {
                expected, title, ..
            } => {
                assert_eq!(expected, "my-repo");
                assert!(title.contains("Unregister"));
            }
            other => panic!("expected ConfirmByName modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_delete_on_other_views_is_noop() {
        let mut app = make_test_app();
        app.state.view = View::Dashboard;
        app.handle_delete();
        assert!(matches!(app.state.modal, Modal::None));
    }

    // ── spawn_main_health_check ───────────────────────────────────────

    #[test]
    fn spawn_main_health_check_no_bg_tx_shows_error_modal() {
        let mut app = make_test_app(); // bg_tx is None by default
        app.state.data.repos = vec![make_test_repo("r1", "my-repo")];
        app.spawn_main_health_check("my-repo".into(), "my-wt".into(), None, None, None);
        assert!(matches!(app.state.modal, Modal::Error { .. }));
    }

    #[test]
    fn handle_delete_archived_worktree_shows_status() {
        let mut app = make_test_app();
        let mut wt = make_test_worktree("w1", "r1", "feat-done");
        wt.status = conductor_core::worktree::WorktreeStatus::Merged;
        app.state.data.worktrees = vec![wt];
        app.state.selected_worktree_id = Some("w1".into());
        app.state.view = View::WorktreeDetail;
        app.handle_delete();
        assert_eq!(
            app.state.status_message.as_deref(),
            Some("Cannot modify archived worktree")
        );
    }
}
