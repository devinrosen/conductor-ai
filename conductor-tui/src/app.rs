use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::github;
use conductor_core::repo::RepoManager;
use conductor_core::session::SessionTracker;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::action::Action;
use crate::background;
use crate::event::{Event, EventLoop};
use crate::input;
use crate::state::{
    AppState, ConfirmAction, DashboardFocus, InputAction, Modal, RepoDetailFocus, View,
};
use crate::ui;

pub struct App {
    state: AppState,
    conn: Connection,
    config: Config,
}

impl App {
    pub fn new(conn: Connection, config: Config) -> Self {
        Self {
            state: AppState::new(),
            conn,
            config,
        }
    }

    /// Main run loop.
    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Initial data load
        self.refresh_data();

        let events = EventLoop::new(Duration::from_millis(200));

        // Spawn background workers
        let bg_tx = events.bg_sender();
        background::spawn_db_poller(bg_tx.clone(), Duration::from_secs(2));
        let sync_mins = self.config.general.sync_interval_minutes as u64;
        background::spawn_ticket_sync(bg_tx, Duration::from_secs(sync_mins * 60));

        loop {
            terminal.draw(|frame| ui::render(frame, &self.state))?;

            match events.next() {
                Ok(Event::Key(key)) => {
                    let action = input::map_key(key, &self.state);
                    self.update(action);
                }
                Ok(Event::Tick) => {
                    // Tick: just re-render (for timer updates etc.)
                }
                Ok(Event::Background(action)) => {
                    self.update(action);
                }
                Err(_) => break,
            }

            if self.state.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Handle an action by mutating state.
    fn update(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Quit => self.state.should_quit = true,

            // Navigation
            Action::Back => self.go_back(),
            Action::NextPanel => self.next_panel(),
            Action::PrevPanel => self.prev_panel(),
            Action::MoveUp => self.move_up(),
            Action::MoveDown => self.move_down(),
            Action::Select => self.select(),

            // View navigation
            Action::GoToDashboard => {
                self.state.view = View::Dashboard;
            }
            Action::GoToTickets => {
                self.state.view = View::Tickets;
                self.state.ticket_index = 0;
            }
            Action::GoToSession => {
                self.state.view = View::Session;
            }

            // Filter
            Action::EnterFilter => {
                self.state.filter_active = true;
                self.state.filter_text.clear();
            }
            Action::FilterChar(c) => {
                self.state.filter_text.push(c);
            }
            Action::FilterBackspace => {
                self.state.filter_text.pop();
            }
            Action::ExitFilter => {
                self.state.filter_active = false;
            }

            // Modal
            Action::ShowHelp => {
                self.state.modal = Modal::Help;
            }
            Action::DismissModal => {
                self.state.modal = Modal::None;
            }
            Action::OpenTicketUrl => self.handle_open_ticket_url(),
            Action::ConfirmYes => self.handle_confirm_yes(),
            Action::ConfirmNo => {
                self.state.modal = Modal::None;
            }
            Action::InputChar(c) => {
                if let Modal::Input { ref mut value, .. } = self.state.modal {
                    value.push(c);
                }
            }
            Action::InputBackspace => {
                if let Modal::Input { ref mut value, .. } = self.state.modal {
                    value.pop();
                }
            }
            Action::InputSubmit => self.handle_input_submit(),

            // CRUD
            Action::Create => self.handle_create(),
            Action::Delete => self.handle_delete(),
            Action::Push => self.handle_push(),
            Action::CreatePr => self.handle_create_pr(),
            Action::SyncTickets => self.handle_sync_tickets(),
            Action::LinkTicket => self.handle_link_ticket(),
            Action::StartSession => self.handle_start_session(),
            Action::EndSession => self.handle_end_session(),

            // Background results
            Action::DataRefreshed {
                repos,
                worktrees,
                tickets,
                session,
                session_worktrees,
            } => {
                self.state.data.repos = repos;
                self.state.data.worktrees = worktrees;
                self.state.data.tickets = tickets;
                self.state.data.current_session = session;
                self.state.data.session_worktrees = session_worktrees;
                self.state.data.rebuild_maps();
                self.clamp_indices();
            }
            Action::TicketSyncComplete { repo_slug, count } => {
                self.state.status_message = Some(format!("Synced {count} tickets for {repo_slug}"));
                self.refresh_data();
            }
            Action::TicketSyncFailed { repo_slug, error } => {
                self.state.status_message = Some(format!("Sync failed for {repo_slug}: {error}"));
            }
            Action::BackgroundError { message } => {
                self.state.modal = Modal::Error { message };
            }
            Action::BackgroundSuccess { message } => {
                self.state.status_message = Some(message);
                self.refresh_data();
            }
        }
    }

    fn refresh_data(&mut self) {
        let repo_mgr = RepoManager::new(&self.conn, &self.config);
        let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
        let ticket_syncer = TicketSyncer::new(&self.conn);
        let session_tracker = SessionTracker::new(&self.conn);

        self.state.data.repos = repo_mgr.list().unwrap_or_default();
        self.state.data.worktrees = wt_mgr.list(None).unwrap_or_default();
        self.state.data.tickets = ticket_syncer.list(None).unwrap_or_default();
        self.state.data.current_session = session_tracker.current().unwrap_or(None);
        self.state.data.session_worktrees = if let Some(ref s) = self.state.data.current_session {
            session_tracker.get_worktrees(&s.id).unwrap_or_default()
        } else {
            Vec::new()
        };
        self.state.data.rebuild_maps();
        self.clamp_indices();

        // If in repo detail, refresh scoped data
        if let Some(ref repo_id) = self.state.selected_repo_id {
            self.state.detail_worktrees = self
                .state
                .data
                .worktrees
                .iter()
                .filter(|wt| &wt.repo_id == repo_id)
                .cloned()
                .collect();
            self.state.detail_tickets = self
                .state
                .data
                .tickets
                .iter()
                .filter(|t| &t.repo_id == repo_id)
                .cloned()
                .collect();
        }
    }

    fn clamp_indices(&mut self) {
        let repos_len = self.state.data.repos.len();
        if repos_len > 0 && self.state.repo_index >= repos_len {
            self.state.repo_index = repos_len - 1;
        }

        let wt_len = self.state.data.worktrees.len();
        if wt_len > 0 && self.state.worktree_index >= wt_len {
            self.state.worktree_index = wt_len - 1;
        }

        let t_len = self.state.data.tickets.len();
        if t_len > 0 && self.state.ticket_index >= t_len {
            self.state.ticket_index = t_len - 1;
        }
    }

    fn go_back(&mut self) {
        match self.state.view {
            View::Dashboard => self.state.should_quit = true,
            View::RepoDetail => {
                self.state.view = View::Dashboard;
                self.state.selected_repo_id = None;
            }
            View::WorktreeDetail => {
                if self.state.selected_repo_id.is_some() {
                    self.state.view = View::RepoDetail;
                } else {
                    self.state.view = View::Dashboard;
                }
                self.state.selected_worktree_id = None;
            }
            View::Tickets | View::Session => {
                self.state.view = View::Dashboard;
            }
        }
    }

    fn next_panel(&mut self) {
        match self.state.view {
            View::Dashboard => {
                self.state.dashboard_focus = self.state.dashboard_focus.next();
            }
            View::RepoDetail => {
                self.state.repo_detail_focus = self.state.repo_detail_focus.toggle();
            }
            _ => {}
        }
    }

    fn prev_panel(&mut self) {
        match self.state.view {
            View::Dashboard => {
                self.state.dashboard_focus = self.state.dashboard_focus.prev();
            }
            View::RepoDetail => {
                self.state.repo_detail_focus = self.state.repo_detail_focus.toggle();
            }
            _ => {}
        }
    }

    fn move_up(&mut self) {
        match self.state.view {
            View::Dashboard => match self.state.dashboard_focus {
                DashboardFocus::Repos => {
                    self.state.repo_index = self.state.repo_index.saturating_sub(1);
                }
                DashboardFocus::Worktrees => {
                    self.state.worktree_index = self.state.worktree_index.saturating_sub(1);
                }
                DashboardFocus::Tickets => {
                    self.state.ticket_index = self.state.ticket_index.saturating_sub(1);
                }
            },
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Worktrees => {
                    self.state.detail_wt_index = self.state.detail_wt_index.saturating_sub(1);
                }
                RepoDetailFocus::Tickets => {
                    self.state.detail_ticket_index =
                        self.state.detail_ticket_index.saturating_sub(1);
                }
            },
            View::Tickets => {
                self.state.ticket_index = self.state.ticket_index.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn move_down(&mut self) {
        match self.state.view {
            View::Dashboard => match self.state.dashboard_focus {
                DashboardFocus::Repos => {
                    let max = self.state.data.repos.len().saturating_sub(1);
                    if self.state.repo_index < max {
                        self.state.repo_index += 1;
                    }
                }
                DashboardFocus::Worktrees => {
                    let max = self.state.data.worktrees.len().saturating_sub(1);
                    if self.state.worktree_index < max {
                        self.state.worktree_index += 1;
                    }
                }
                DashboardFocus::Tickets => {
                    let max = self.state.data.tickets.len().saturating_sub(1);
                    if self.state.ticket_index < max {
                        self.state.ticket_index += 1;
                    }
                }
            },
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Worktrees => {
                    let max = self.state.detail_worktrees.len().saturating_sub(1);
                    if self.state.detail_wt_index < max {
                        self.state.detail_wt_index += 1;
                    }
                }
                RepoDetailFocus::Tickets => {
                    let max = self.state.detail_tickets.len().saturating_sub(1);
                    if self.state.detail_ticket_index < max {
                        self.state.detail_ticket_index += 1;
                    }
                }
            },
            View::Tickets => {
                let max = self.state.data.tickets.len().saturating_sub(1);
                if self.state.ticket_index < max {
                    self.state.ticket_index += 1;
                }
            }
            _ => {}
        }
    }

    fn select(&mut self) {
        match self.state.view {
            View::Dashboard => match self.state.dashboard_focus {
                DashboardFocus::Repos => {
                    if let Some(repo) = self.state.selected_repo() {
                        let repo_id = repo.id.clone();
                        self.state.selected_repo_id = Some(repo_id.clone());
                        self.state.detail_worktrees = self
                            .state
                            .data
                            .worktrees
                            .iter()
                            .filter(|wt| wt.repo_id == repo_id)
                            .cloned()
                            .collect();
                        self.state.detail_tickets = self
                            .state
                            .data
                            .tickets
                            .iter()
                            .filter(|t| t.repo_id == repo_id)
                            .cloned()
                            .collect();
                        self.state.detail_wt_index = 0;
                        self.state.detail_ticket_index = 0;
                        self.state.repo_detail_focus = RepoDetailFocus::Worktrees;
                        self.state.view = View::RepoDetail;
                    }
                }
                DashboardFocus::Worktrees => {
                    if let Some(wt) = self.state.selected_worktree() {
                        self.state.selected_worktree_id = Some(wt.id.clone());
                        self.state.selected_repo_id = None;
                        self.state.view = View::WorktreeDetail;
                    }
                }
                DashboardFocus::Tickets => {
                    if let Some(ticket) = self.state.data.tickets.get(self.state.ticket_index) {
                        self.state.modal = Modal::TicketInfo {
                            ticket: Box::new(ticket.clone()),
                        };
                    }
                }
            },
            View::RepoDetail => match self.state.repo_detail_focus {
                RepoDetailFocus::Worktrees => {
                    if let Some(wt) = self.state.detail_worktrees.get(self.state.detail_wt_index) {
                        self.state.selected_worktree_id = Some(wt.id.clone());
                        self.state.view = View::WorktreeDetail;
                    }
                }
                RepoDetailFocus::Tickets => {
                    if let Some(ticket) = self
                        .state
                        .detail_tickets
                        .get(self.state.detail_ticket_index)
                    {
                        self.state.modal = Modal::TicketInfo {
                            ticket: Box::new(ticket.clone()),
                        };
                    }
                }
            },
            View::Tickets => {
                if let Some(ticket) = self.state.data.tickets.get(self.state.ticket_index) {
                    self.state.modal = Modal::TicketInfo {
                        ticket: Box::new(ticket.clone()),
                    };
                }
            }
            _ => {}
        }
    }

    fn handle_open_ticket_url(&mut self) {
        if let Modal::TicketInfo { ref ticket } = self.state.modal {
            let url = ticket.url.clone();
            if url.is_empty() {
                self.state.status_message = Some("No URL available".to_string());
                return;
            }
            let result = Command::new("open").arg(&url).output();
            match result {
                Ok(o) if o.status.success() => {
                    self.state.status_message = Some(format!("Opened {url}"));
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    self.state.status_message = Some(format!("Failed to open URL: {stderr}"));
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Failed to open URL: {e}"));
                }
            }
        }
    }

    fn handle_confirm_yes(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::Confirm { on_confirm, .. } = modal {
            match on_confirm {
                ConfirmAction::DeleteWorktree { repo_slug, wt_slug } => {
                    let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
                    match wt_mgr.delete(&repo_slug, &wt_slug) {
                        Ok(()) => {
                            self.state.status_message =
                                Some(format!("Deleted worktree: {wt_slug}"));
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
                ConfirmAction::EndSession { session_id } => {
                    let tracker = SessionTracker::new(&self.conn);
                    match tracker.end(&session_id, None) {
                        Ok(()) => {
                            self.state.status_message = Some("Session ended".to_string());
                            self.refresh_data();
                        }
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("End session failed: {e}"),
                            };
                        }
                    }
                }
            }
        }
    }

    fn handle_input_submit(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::Input {
            value, on_submit, ..
        } = modal
        {
            if value.is_empty() {
                return;
            }
            match on_submit {
                InputAction::CreateWorktree { repo_slug } => {
                    let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
                    match wt_mgr.create(&repo_slug, &value, None, None) {
                        Ok(wt) => {
                            self.state.status_message =
                                Some(format!("Created worktree: {}", wt.slug));
                            self.refresh_data();
                        }
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("Create failed: {e}"),
                            };
                        }
                    }
                }
                InputAction::LinkTicket { worktree_id } => {
                    let syncer = TicketSyncer::new(&self.conn);
                    // Find ticket by source_id, scoped to the worktree's repo
                    let wt_repo_id = self
                        .state
                        .data
                        .worktrees
                        .iter()
                        .find(|w| w.id == worktree_id)
                        .map(|w| w.repo_id.as_str());
                    let ticket =
                        self.state.data.tickets.iter().find(|t| {
                            t.source_id == value && Some(t.repo_id.as_str()) == wt_repo_id
                        });
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
                InputAction::SessionNotes { session_id } => {
                    let tracker = SessionTracker::new(&self.conn);
                    match tracker.end(&session_id, Some(&value)) {
                        Ok(()) => {
                            self.state.status_message =
                                Some("Session ended with notes".to_string());
                            self.refresh_data();
                        }
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("End session failed: {e}"),
                            };
                        }
                    }
                }
            }
        }
    }

    fn handle_create(&mut self) {
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
                        on_submit: InputAction::CreateWorktree { repo_slug: slug },
                    };
                } else {
                    self.state.status_message = Some("Select a repo first".to_string());
                }
            }
            _ => {}
        }
    }

    fn handle_delete(&mut self) {
        if self.state.view == View::WorktreeDetail {
            if let Some(ref wt_id) = self.state.selected_worktree_id {
                if let Some(wt) = self.state.data.worktrees.iter().find(|w| &w.id == wt_id) {
                    let repo_slug = self
                        .state
                        .data
                        .repo_slug_map
                        .get(&wt.repo_id)
                        .cloned()
                        .unwrap_or_else(|| "?".to_string());
                    self.state.modal = Modal::Confirm {
                        title: "Delete Worktree".to_string(),
                        message: format!(
                            "Delete worktree {}/{}? This removes the git worktree and branch.",
                            repo_slug, wt.slug
                        ),
                        on_confirm: ConfirmAction::DeleteWorktree {
                            repo_slug,
                            wt_slug: wt.slug.clone(),
                        },
                    };
                }
            }
        }
    }

    fn handle_push(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        if let Some(wt) = wt {
            self.state.status_message = Some(format!("Pushing {}...", wt.slug));
            let output = Command::new("git")
                .args(["push", "-u", "origin", &wt.branch])
                .current_dir(&wt.path)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    self.state.status_message = Some(format!("Pushed {}", wt.slug));
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    self.state.modal = Modal::Error {
                        message: format!("Push failed: {stderr}"),
                    };
                }
                Err(e) => {
                    self.state.modal = Modal::Error {
                        message: format!("Push failed: {e}"),
                    };
                }
            }
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    fn handle_create_pr(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        if let Some(wt) = wt {
            self.state.status_message = Some(format!("Creating PR for {}...", wt.slug));
            let output = Command::new("gh")
                .args(["pr", "create", "--fill", "--head", &wt.branch])
                .current_dir(&wt.path)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    let url = String::from_utf8_lossy(&o.stdout);
                    self.state.status_message = Some(format!("PR created: {}", url.trim()));
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    self.state.modal = Modal::Error {
                        message: format!("PR creation failed: {stderr}"),
                    };
                }
                Err(e) => {
                    self.state.modal = Modal::Error {
                        message: format!("PR creation failed: {e}"),
                    };
                }
            }
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    fn handle_sync_tickets(&mut self) {
        self.state.status_message = Some("Syncing tickets...".to_string());

        let repo_mgr = RepoManager::new(&self.conn, &self.config);
        let repos = repo_mgr.list().unwrap_or_default();
        let syncer = TicketSyncer::new(&self.conn);

        let mut total = 0;
        for repo in &repos {
            if let Some((owner, name)) = github::parse_github_remote(&repo.remote_url) {
                match github::sync_github_issues(&owner, &name) {
                    Ok(tickets) => {
                        if let Ok(count) = syncer.upsert_tickets(&repo.id, &tickets) {
                            total += count;
                        }
                    }
                    Err(e) => {
                        self.state.status_message =
                            Some(format!("Sync error for {}: {e}", repo.slug));
                    }
                }
            }
        }

        self.state.status_message = Some(format!("Synced {total} tickets"));
        self.refresh_data();
    }

    fn handle_link_ticket(&mut self) {
        if let Some(ref wt_id) = self.state.selected_worktree_id.clone() {
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

    fn handle_start_session(&mut self) {
        if self.state.data.current_session.is_some() {
            self.state.status_message = Some("Session already active".to_string());
            return;
        }

        let tracker = SessionTracker::new(&self.conn);
        match tracker.start() {
            Ok(session) => {
                self.state.status_message = Some(format!(
                    "Session started: {}",
                    &session.id[..13.min(session.id.len())]
                ));
                self.refresh_data();
                self.state.view = View::Session;
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Start session failed: {e}"),
                };
            }
        }
    }

    fn handle_end_session(&mut self) {
        if let Some(ref session) = self.state.data.current_session {
            self.state.modal = Modal::Confirm {
                title: "End Session".to_string(),
                message: "End the current session?".to_string(),
                on_confirm: ConfirmAction::EndSession {
                    session_id: session.id.clone(),
                },
            };
        } else {
            self.state.status_message = Some("No active session".to_string());
        }
    }
}
