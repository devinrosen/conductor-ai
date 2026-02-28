use std::collections::HashMap;

use conductor_core::repo::Repo;
use conductor_core::session::Session;
use conductor_core::tickets::Ticket;
use conductor_core::worktree::Worktree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    RepoDetail,
    WorktreeDetail,
    Tickets,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardFocus {
    Repos,
    Worktrees,
    Tickets,
}

impl DashboardFocus {
    pub fn next(self) -> Self {
        match self {
            Self::Repos => Self::Worktrees,
            Self::Worktrees => Self::Tickets,
            Self::Tickets => Self::Repos,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Repos => Self::Tickets,
            Self::Worktrees => Self::Repos,
            Self::Tickets => Self::Worktrees,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFocus {
    Worktrees,
    History,
}

impl SessionFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Worktrees => Self::History,
            Self::History => Self::Worktrees,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoDetailFocus {
    Worktrees,
    Tickets,
}

impl RepoDetailFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Worktrees => Self::Tickets,
            Self::Tickets => Self::Worktrees,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    None,
    Help,
    Confirm {
        title: String,
        message: String,
        on_confirm: ConfirmAction,
    },
    Input {
        title: String,
        prompt: String,
        value: String,
        on_submit: InputAction,
    },
    Form {
        title: String,
        fields: Vec<FormField>,
        active_field: usize,
        on_submit: FormAction,
    },
    Error {
        message: String,
    },
    TicketInfo {
        ticket: Box<Ticket>,
    },
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteWorktree { repo_slug: String, wt_slug: String },
    EndSession { session_id: String },
    RemoveRepo { repo_slug: String },
}

#[derive(Debug, Clone)]
pub struct FormField {
    pub label: String,
    pub value: String,
    pub placeholder: String,
    pub manually_edited: bool,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub enum FormAction {
    AddRepo,
}

#[derive(Debug, Clone)]
pub enum InputAction {
    CreateWorktree {
        repo_slug: String,
        ticket_id: Option<String>,
    },
    LinkTicket {
        worktree_id: String,
    },
    SessionNotes {
        session_id: String,
    },
    AttachWorktree {
        session_id: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct DataCache {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
    pub tickets: Vec<Ticket>,
    pub current_session: Option<Session>,
    pub session_worktrees: Vec<Worktree>,
    /// Past sessions (ended), most recent first
    pub session_history: Vec<Session>,
    /// session_id -> worktree count for history display
    pub session_wt_counts: HashMap<String, usize>,
    /// repo_id -> slug for display
    pub repo_slug_map: HashMap<String, String>,
    /// ticket_id -> Ticket for lookups
    pub ticket_map: HashMap<String, Ticket>,
    /// repo_id -> worktree count
    pub repo_worktree_count: HashMap<String, usize>,
}

impl DataCache {
    pub fn rebuild_maps(&mut self) {
        self.repo_slug_map.clear();
        for repo in &self.repos {
            self.repo_slug_map
                .insert(repo.id.clone(), repo.slug.clone());
        }

        self.ticket_map.clear();
        for ticket in &self.tickets {
            self.ticket_map.insert(ticket.id.clone(), ticket.clone());
        }

        self.repo_worktree_count.clear();
        for wt in &self.worktrees {
            *self
                .repo_worktree_count
                .entry(wt.repo_id.clone())
                .or_insert(0) += 1;
        }
    }
}

pub struct AppState {
    pub view: View,
    pub dashboard_focus: DashboardFocus,
    pub repo_detail_focus: RepoDetailFocus,
    pub session_focus: SessionFocus,
    pub modal: Modal,
    pub data: DataCache,

    // Selection indices
    pub repo_index: usize,
    pub worktree_index: usize,
    pub ticket_index: usize,
    pub session_wt_index: usize,
    pub session_history_index: usize,

    // Detail view context
    pub selected_repo_id: Option<String>,
    pub selected_worktree_id: Option<String>,

    // Scoped lists for detail views
    pub detail_worktrees: Vec<Worktree>,
    pub detail_tickets: Vec<Ticket>,
    pub detail_wt_index: usize,
    pub detail_ticket_index: usize,

    // Filter
    pub filter_active: bool,
    pub filter_text: String,

    // Status bar message
    pub status_message: Option<String>,

    pub should_quit: bool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            view: View::Dashboard,
            dashboard_focus: DashboardFocus::Repos,
            repo_detail_focus: RepoDetailFocus::Worktrees,
            session_focus: SessionFocus::Worktrees,
            modal: Modal::None,
            data: DataCache::default(),
            repo_index: 0,
            worktree_index: 0,
            ticket_index: 0,
            session_wt_index: 0,
            session_history_index: 0,
            selected_repo_id: None,
            selected_worktree_id: None,
            detail_worktrees: Vec::new(),
            detail_tickets: Vec::new(),
            detail_wt_index: 0,
            detail_ticket_index: 0,
            filter_active: false,
            filter_text: String::new(),
            status_message: None,
            should_quit: false,
        }
    }

    /// Get the currently selected repo, if any.
    pub fn selected_repo(&self) -> Option<&Repo> {
        self.data.repos.get(self.repo_index)
    }

    /// Get the currently selected worktree from the dashboard list.
    pub fn selected_worktree(&self) -> Option<&Worktree> {
        self.data.worktrees.get(self.worktree_index)
    }

    /// Get the currently selected ticket from the dashboard list.
    #[allow(dead_code)]
    pub fn selected_ticket(&self) -> Option<&Ticket> {
        self.data.tickets.get(self.ticket_index)
    }
}
