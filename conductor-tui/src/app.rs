use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use rusqlite::Connection;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::github;
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};
use conductor_core::session::SessionTracker;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::action::Action;
use crate::background;
use crate::event::{BackgroundSender, EventLoop};
use crate::input;
use crate::state::{
    AppState, ConfirmAction, DashboardFocus, FormAction, FormField, InputAction, Modal,
    RepoDetailFocus, View,
};
use crate::ui;

/// Derive a worktree slug from a ticket's source_id and title.
/// Format: `{source_id}-{slugified-title}`, e.g. `15-tui-create-worktree`.
/// Title portion is truncated to keep the total slug under ~40 chars.
fn derive_worktree_slug(source_id: &str, title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse consecutive dashes
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }
    let title_slug = collapsed.trim_matches('-');

    // Budget: 40 chars total, minus source_id and separator
    let budget = 40_usize.saturating_sub(source_id.len() + 1);
    let truncated = if title_slug.len() <= budget {
        title_slug
    } else {
        match title_slug[..budget].rfind('-') {
            Some(pos) => &title_slug[..pos],
            None => &title_slug[..budget],
        }
    };

    format!("{}-{}", source_id, truncated)
}

pub struct App {
    state: AppState,
    conn: Connection,
    config: Config,
    bg_tx: Option<BackgroundSender>,
}

impl App {
    pub fn new(conn: Connection, config: Config) -> Self {
        Self {
            state: AppState::new(),
            conn,
            config,
            bg_tx: None,
        }
    }

    /// Main run loop.
    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Initial data load
        self.refresh_data();

        let events = EventLoop::new(Duration::from_millis(200));

        // Spawn background workers
        let bg_tx = events.bg_sender();
        self.bg_tx = Some(bg_tx.clone());
        background::spawn_db_poller(bg_tx.clone(), Duration::from_secs(5));
        let sync_mins = self.config.general.sync_interval_minutes as u64;
        background::spawn_ticket_sync(bg_tx, Duration::from_secs(sync_mins * 60));

        let mut dirty = true; // tracks whether state changed since last draw

        loop {
            // Only redraw when state has actually changed.
            if dirty {
                terminal.draw(|frame| ui::render(frame, &self.state))?;
                dirty = false;
            }

            // Block until at least one event is available
            events.wait();

            // PRIORITY 1: Drain all key events first — input is never starved
            for key in events.drain_input() {
                let action = input::map_key(key, &self.state);
                dirty |= self.update(action);
            }

            // PRIORITY 2: Drain all background events
            let bg_actions = events.drain_background();
            for action in bg_actions {
                dirty |= self.update(action);
            }

            if self.state.should_quit {
                break;
            }
        }

        Ok(())
    }

    /// Handle an action by mutating state. Returns true if the UI needs a redraw.
    fn update(&mut self, action: Action) -> bool {
        match action {
            Action::None | Action::Tick => {
                return false;
            }
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
            Action::FormChar(c) => self.handle_form_char(c),
            Action::FormBackspace => self.handle_form_backspace(),
            Action::FormNextField => self.handle_form_next_field(),
            Action::FormPrevField => self.handle_form_prev_field(),
            Action::FormSubmit => self.handle_form_submit(),

            // CRUD
            Action::AddRepo => self.handle_add_repo(),
            Action::Create => self.handle_create(),
            Action::Delete => self.handle_delete(),
            Action::Push => self.handle_push(),
            Action::CreatePr => self.handle_create_pr(),
            Action::SyncTickets => self.handle_sync_tickets(),
            Action::LinkTicket => self.handle_link_ticket(),
            Action::StartSession => self.handle_start_session(),
            Action::EndSession => self.handle_end_session(),
            Action::StartWork => self.handle_start_work(),

            // Agent (tmux-based)
            Action::LaunchAgent => self.handle_launch_agent(),
            Action::AttachAgent => self.handle_attach_agent(),
            Action::StopAgent => self.handle_stop_agent(),

            // Background results
            Action::DataRefreshed {
                repos,
                worktrees,
                tickets,
                session,
                session_worktrees,
                latest_agent_runs,
            } => {
                self.state.data.repos = repos;
                self.state.data.worktrees = worktrees;
                self.state.data.tickets = tickets;
                self.state.data.current_session = session;
                self.state.data.session_worktrees = session_worktrees;
                self.state.data.latest_agent_runs = latest_agent_runs;
                self.state.data.rebuild_maps();
                self.clamp_indices();
                // Don't trigger a redraw — data updates silently in the
                // background; the next user-driven action will render it.
                return false;
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
        true
    }

    fn refresh_data(&mut self) {
        let repo_mgr = RepoManager::new(&self.conn, &self.config);
        let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
        let ticket_syncer = TicketSyncer::new(&self.conn);
        let session_tracker = SessionTracker::new(&self.conn);
        let agent_mgr = AgentManager::new(&self.conn);

        self.state.data.repos = repo_mgr.list().unwrap_or_default();
        self.state.data.worktrees = wt_mgr.list(None).unwrap_or_default();
        self.state.data.tickets = ticket_syncer.list(None).unwrap_or_default();
        self.state.data.current_session = session_tracker.current().unwrap_or(None);
        self.state.data.session_worktrees = if let Some(ref s) = self.state.data.current_session {
            session_tracker.get_worktrees(&s.id).unwrap_or_default()
        } else {
            Vec::new()
        };
        self.state.data.latest_agent_runs = agent_mgr.latest_runs_by_worktree().unwrap_or_default();
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
                        let wt_id = wt.id.clone();
                        self.state.selected_worktree_id = Some(wt_id);
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
                        let wt_id = wt.id.clone();
                        self.state.selected_worktree_id = Some(wt_id);
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
        // Resolve the ticket URL from either the TicketInfo modal or the WorktreeDetail view
        let url = if let Modal::TicketInfo { ref ticket } = self.state.modal {
            Some(ticket.url.clone())
        } else if self.state.view == View::WorktreeDetail {
            self.state
                .selected_worktree_id
                .as_ref()
                .and_then(|wt_id| self.state.data.worktrees.iter().find(|w| &w.id == wt_id))
                .and_then(|wt| wt.ticket_id.as_ref())
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .map(|t| t.url.clone())
        } else {
            None
        };

        let Some(url) = url else {
            if self.state.view == View::WorktreeDetail {
                self.state.status_message = Some("No ticket linked to this worktree".to_string());
            }
            return;
        };

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
                ConfirmAction::RemoveRepo { repo_slug } => {
                    let mgr = RepoManager::new(&self.conn, &self.config);
                    match mgr.remove(&repo_slug) {
                        Ok(()) => {
                            self.state.status_message = Some(format!("Removed repo: {repo_slug}"));
                            self.state.view = View::Dashboard;
                            self.state.selected_repo_id = None;
                            self.refresh_data();
                        }
                        Err(e) => {
                            self.state.modal = Modal::Error {
                                message: format!("Remove failed: {e}"),
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
                InputAction::CreateWorktree {
                    repo_slug,
                    ticket_id,
                } => {
                    let wt_mgr = WorktreeManager::new(&self.conn, &self.config);
                    match wt_mgr.create(&repo_slug, &value, None, ticket_id.as_deref()) {
                        Ok(wt) => {
                            let msg = if let Some(ref tid) = ticket_id {
                                let source_id = self
                                    .state
                                    .data
                                    .ticket_map
                                    .get(tid)
                                    .map(|t| t.source_id.as_str())
                                    .unwrap_or("?");
                                format!("Created worktree: {} (linked to #{})", wt.slug, source_id)
                            } else {
                                format!("Created worktree: {}", wt.slug)
                            };
                            self.state.status_message = Some(msg);
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
                InputAction::AgentPrompt {
                    worktree_id,
                    worktree_path,
                    worktree_slug,
                    resume_session_id,
                } => {
                    self.start_agent_tmux(
                        value,
                        worktree_id,
                        worktree_path,
                        worktree_slug,
                        resume_session_id,
                    );
                }
            }
        }
    }

    fn handle_create(&mut self) {
        // Try to detect ticket context based on current view and focus
        let ticket_context = match self.state.view {
            View::Dashboard if self.state.dashboard_focus == DashboardFocus::Tickets => self
                .state
                .data
                .tickets
                .get(self.state.ticket_index)
                .cloned(),
            View::RepoDetail if self.state.repo_detail_focus == RepoDetailFocus::Tickets => self
                .state
                .detail_tickets
                .get(self.state.detail_ticket_index)
                .cloned(),
            View::Tickets => self
                .state
                .data
                .tickets
                .get(self.state.ticket_index)
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
                } else if self.state.view == View::Dashboard
                    && self.state.dashboard_focus == DashboardFocus::Repos
                {
                    // No repo selected on repos panel — open add repo form instead
                    self.handle_add_repo();
                } else {
                    self.state.status_message = Some("Select a repo first".to_string());
                }
            }
            _ => {}
        }
    }

    fn handle_add_repo(&mut self) {
        if self.state.view != View::Dashboard {
            return;
        }
        self.state.modal = Modal::Form {
            title: "Add Repository".to_string(),
            fields: vec![
                FormField {
                    label: "Remote URL".to_string(),
                    value: String::new(),
                    placeholder: "https://github.com/org/repo.git".to_string(),
                    manually_edited: true,
                    required: true,
                },
                FormField {
                    label: "Slug".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from URL".to_string(),
                    manually_edited: false,
                    required: true,
                },
                FormField {
                    label: "Local Path".to_string(),
                    value: String::new(),
                    placeholder: "auto-derived from slug".to_string(),
                    manually_edited: false,
                    required: false,
                },
            ],
            active_field: 0,
            on_submit: FormAction::AddRepo,
        };
    }

    fn handle_form_char(&mut self, c: char) {
        let config = &self.config;
        if let Modal::Form {
            ref mut fields,
            active_field,
            ref on_submit,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                field.value.push(c);
                field.manually_edited = true;
            }
            // Auto-derive dependent fields
            match on_submit {
                FormAction::AddRepo => {
                    Self::auto_derive_add_repo_fields(fields, active_field, config)
                }
            }
        }
    }

    fn handle_form_backspace(&mut self) {
        let config = &self.config;
        if let Modal::Form {
            ref mut fields,
            active_field,
            ref on_submit,
            ..
        } = self.state.modal
        {
            if let Some(field) = fields.get_mut(active_field) {
                field.value.pop();
                // If field emptied and it's a derived field, reset to auto-derive
                if field.value.is_empty() && active_field > 0 {
                    field.manually_edited = false;
                }
            }
            match on_submit {
                FormAction::AddRepo => {
                    Self::auto_derive_add_repo_fields(fields, active_field, config)
                }
            }
        }
    }

    fn handle_form_next_field(&mut self) {
        if let Modal::Form {
            ref fields,
            ref mut active_field,
            ..
        } = self.state.modal
        {
            *active_field = (*active_field + 1) % fields.len();
        }
    }

    fn handle_form_prev_field(&mut self) {
        if let Modal::Form {
            ref fields,
            ref mut active_field,
            ..
        } = self.state.modal
        {
            if *active_field == 0 {
                *active_field = fields.len() - 1;
            } else {
                *active_field -= 1;
            }
        }
    }

    fn auto_derive_add_repo_fields(
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

    fn handle_form_submit(&mut self) {
        let modal = std::mem::replace(&mut self.state.modal, Modal::None);
        if let Modal::Form {
            fields, on_submit, ..
        } = modal
        {
            match on_submit {
                FormAction::AddRepo => self.submit_add_repo(fields),
            }
        }
    }

    fn submit_add_repo(&mut self, fields: Vec<FormField>) {
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
        match mgr.add(&slug, &local, &url, None) {
            Ok(repo) => {
                self.state.status_message = Some(format!("Added repo: {}", repo.slug));
                self.refresh_data();
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Add repo failed: {e}"),
                };
            }
        }
    }

    fn handle_delete(&mut self) {
        match self.state.view {
            View::WorktreeDetail => {
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
                        self.state.modal = Modal::Confirm {
                            title: "Remove Repository".to_string(),
                            message: format!(
                                "Remove repo '{}'?{} This unregisters it from Conductor.",
                                repo.slug, warning
                            ),
                            on_confirm: ConfirmAction::RemoveRepo {
                                repo_slug: repo.slug.clone(),
                            },
                        };
                    }
                }
            }
            _ => {}
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
                        let synced_ids: Vec<&str> =
                            tickets.iter().map(|t| t.source_id.as_str()).collect();
                        if let Ok(count) = syncer.upsert_tickets(&repo.id, &tickets) {
                            total += count;
                            let _ = syncer.close_missing_tickets(&repo.id, "github", &synced_ids);
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

    fn handle_start_work(&mut self) {
        // Resolve the selected worktree from the current view context
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned()
            .or_else(|| match self.state.view {
                View::Dashboard if self.state.dashboard_focus == DashboardFocus::Worktrees => self
                    .state
                    .data
                    .worktrees
                    .get(self.state.worktree_index)
                    .cloned(),
                View::RepoDetail => self
                    .state
                    .detail_worktrees
                    .get(self.state.detail_wt_index)
                    .cloned(),
                _ => None,
            });

        let Some(wt) = wt else {
            self.state.status_message = Some("Select a worktree first".to_string());
            return;
        };

        let editor = &self.config.general.editor;

        let result = if editor == "terminal" {
            // Open Terminal.app at the worktree path and run claude
            Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        "tell application \"Terminal\" to do script \"cd '{}' && claude\"",
                        wt.path
                    ),
                ])
                .spawn()
        } else {
            // For code, cursor, or any custom editor command
            Command::new(editor).arg(&wt.path).spawn()
        };

        match result {
            Ok(_) => {
                self.state.status_message = Some(format!("Opened {} at {}", editor, wt.slug));
            }
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to open {editor}: {e}"),
                };
            }
        }
    }

    // ── Agent handlers (tmux-based) ────────────────────────────────────

    fn handle_launch_agent(&mut self) {
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

        // Check if there's already a running agent for this worktree
        let has_running = self
            .state
            .data
            .latest_agent_runs
            .get(&wt.id)
            .is_some_and(|run| run.status == "running");

        if has_running {
            self.state.status_message =
                Some("Agent already running — press a to attach".to_string());
            return;
        }

        // Check for existing session to resume (from DB)
        let resume_session_id = self
            .state
            .data
            .latest_agent_runs
            .get(&wt.id)
            .and_then(|run| run.claude_session_id.clone());

        let (title, prefill) = if resume_session_id.is_some() {
            ("Claude Agent (Resume)".to_string(), String::new())
        } else {
            // Pre-fill prompt with ticket URL if available
            let prefill = wt
                .ticket_id
                .as_ref()
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .filter(|t| !t.url.is_empty())
                .map(|t| format!("can we work on {}", t.url))
                .unwrap_or_default();
            ("Claude Agent".to_string(), prefill)
        };

        self.state.modal = Modal::Input {
            title,
            prompt: "Enter prompt for Claude:".to_string(),
            value: prefill,
            on_submit: InputAction::AgentPrompt {
                worktree_id: wt.id.clone(),
                worktree_path: wt.path.clone(),
                worktree_slug: wt.slug.clone(),
                resume_session_id,
            },
        };
    }

    fn handle_attach_agent(&mut self) {
        let wt_id = self.state.selected_worktree_id.as_ref();
        let run = wt_id.and_then(|id| self.state.data.latest_agent_runs.get(id));

        let Some(run) = run else {
            self.state.status_message = Some("No agent to attach to".to_string());
            return;
        };

        if run.status != "running" {
            self.state.status_message = Some("Agent is not running".to_string());
            return;
        }

        let Some(ref tmux_window) = run.tmux_window else {
            self.state.status_message = Some("No tmux window for this agent".to_string());
            return;
        };

        let result = Command::new("tmux")
            .args(["select-window", "-t", &format!(":{tmux_window}")])
            .output();

        match result {
            Ok(o) if o.status.success() => {
                self.state.status_message = Some(format!("Attached to {tmux_window}"));
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                self.state.status_message = Some(format!("Failed to attach: {stderr}"));
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to attach: {e}"));
            }
        }
    }

    fn handle_stop_agent(&mut self) {
        let wt_id = self.state.selected_worktree_id.as_ref();
        let run = wt_id.and_then(|id| self.state.data.latest_agent_runs.get(id));

        let Some(run) = run else {
            return;
        };

        if run.status != "running" {
            return;
        }

        let run_id = run.id.clone();
        let tmux_window = run.tmux_window.clone();

        // Kill the tmux window
        if let Some(ref window) = tmux_window {
            let _ = Command::new("tmux")
                .args(["kill-window", "-t", &format!(":{window}")])
                .output();
        }

        // Update DB record to cancelled
        let mgr = AgentManager::new(&self.conn);
        let _ = mgr.update_run_cancelled(&run_id);

        self.state.status_message = Some("Agent cancelled".to_string());
        self.refresh_data();
    }

    fn start_agent_tmux(
        &mut self,
        prompt: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
    ) {
        // Create DB record with tmux window name
        let mgr = AgentManager::new(&self.conn);
        let run = match mgr.create_run(&worktree_id, &prompt, Some(&worktree_slug)) {
            Ok(run) => run,
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Failed to create agent run: {e}"),
                };
                return;
            }
        };

        // Build the conductor agent run command
        let mut args = vec![
            "agent".to_string(),
            "run".to_string(),
            "--run-id".to_string(),
            run.id.clone(),
            "--worktree-path".to_string(),
            worktree_path,
            "--prompt".to_string(),
            prompt,
        ];

        if let Some(ref session_id) = resume_session_id {
            args.push("--resume".to_string());
            args.push(session_id.clone());
        }

        // Spawn tmux window: tmux new-window -n <slug> -- conductor agent run ...
        let mut tmux_args = vec![
            "new-window".to_string(),
            "-n".to_string(),
            worktree_slug.clone(),
            "--".to_string(),
            "conductor".to_string(),
        ];
        tmux_args.extend(args);

        let result = Command::new("tmux").args(&tmux_args).output();

        match result {
            Ok(o) if o.status.success() => {
                self.state.status_message =
                    Some(format!("Agent launched in tmux window: {worktree_slug}"));
                self.refresh_data();
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Clean up the DB record since tmux failed
                let _ = mgr.update_run_failed(&run.id, &format!("tmux failed: {stderr}"));
                self.state.modal = Modal::Error {
                    message: format!("Failed to spawn tmux window: {stderr}"),
                };
            }
            Err(e) => {
                let _ = mgr.update_run_failed(&run.id, &format!("tmux error: {e}"));
                self.state.modal = Modal::Error {
                    message: format!("Failed to spawn tmux: {e}"),
                };
            }
        }
    }
}
