use std::collections::HashMap;
use std::fmt;

use conductor_core::agent::{AgentEvent, AgentRun, TicketAgentTotals};
use conductor_core::config::WorkTarget;
use conductor_core::repo::Repo;
use conductor_core::session::Session;
use conductor_core::tickets::Ticket;
use conductor_core::worktree::Worktree;
use tui_textarea::TextArea;

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

#[derive(Clone)]
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
    AgentPrompt {
        title: String,
        prompt: String,
        textarea: Box<TextArea<'static>>,
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
    WorkTargetPicker {
        targets: Vec<WorkTarget>,
        selected: usize,
    },
    WorkTargetManager {
        targets: Vec<WorkTarget>,
        selected: usize,
    },
}

impl fmt::Debug for Modal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Modal::None => write!(f, "Modal::None"),
            Modal::Help => write!(f, "Modal::Help"),
            Modal::Confirm { title, .. } => {
                f.debug_struct("Confirm").field("title", title).finish()
            }
            Modal::Input { title, .. } => f.debug_struct("Input").field("title", title).finish(),
            Modal::AgentPrompt { title, .. } => {
                f.debug_struct("AgentPrompt").field("title", title).finish()
            }
            Modal::Form { title, .. } => f.debug_struct("Form").field("title", title).finish(),
            Modal::Error { message } => f.debug_struct("Error").field("message", message).finish(),
            Modal::TicketInfo { .. } => write!(f, "Modal::TicketInfo"),
            Modal::WorkTargetPicker { .. } => write!(f, "Modal::WorkTargetPicker"),
            Modal::WorkTargetManager { .. } => write!(f, "Modal::WorkTargetManager"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteWorktree {
        repo_slug: String,
        wt_slug: String,
    },
    EndSession {
        session_id: String,
    },
    RemoveRepo {
        repo_slug: String,
    },
    DeleteWorkTarget {
        index: usize,
    },
    StartAgentForWorktree {
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
    },
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
    AddWorkTarget,
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
    AgentPrompt {
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
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
    /// worktree_id -> latest AgentRun (populated by DB poller)
    pub latest_agent_runs: HashMap<String, AgentRun>,
    /// Parsed agent events for the currently viewed worktree
    pub agent_events: Vec<AgentEvent>,
    /// Aggregate stats across all agent runs for the currently viewed worktree
    pub agent_totals: AgentTotals,
    /// ticket_id -> aggregated agent stats across all linked worktrees
    pub ticket_agent_totals: HashMap<String, TicketAgentTotals>,
    /// ticket_id -> linked worktrees (most recently created first)
    pub ticket_worktrees: HashMap<String, Vec<Worktree>>,
}

/// Aggregated stats across all agent runs for a worktree.
#[derive(Debug, Clone, Default)]
pub struct AgentTotals {
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub run_count: usize,
    /// Live turn count from the currently running agent's log file.
    pub live_turns: i64,
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
        self.ticket_worktrees.clear();
        for wt in &self.worktrees {
            *self
                .repo_worktree_count
                .entry(wt.repo_id.clone())
                .or_insert(0) += 1;
            if let Some(ref tid) = wt.ticket_id {
                self.ticket_worktrees
                    .entry(tid.clone())
                    .or_default()
                    .push(wt.clone());
            }
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

    // Agent activity scroll
    pub agent_event_index: usize,
    /// Tracks pending `g` keypress for `gg` chord (go to top)
    pub pending_g: bool,

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
            agent_event_index: 0,
            pending_g: false,
            filter_active: false,
            filter_text: String::new(),
            status_message: None,
            should_quit: false,
        }
    }

    /// Returns (current_index, list_length) for the currently focused pane.
    pub fn focused_index_and_len(&self) -> (usize, usize) {
        match self.view {
            View::Dashboard => match self.dashboard_focus {
                DashboardFocus::Repos => (self.repo_index, self.data.repos.len()),
                DashboardFocus::Worktrees => (self.worktree_index, self.data.worktrees.len()),
                DashboardFocus::Tickets => (self.ticket_index, self.data.tickets.len()),
            },
            View::RepoDetail => match self.repo_detail_focus {
                RepoDetailFocus::Worktrees => (self.detail_wt_index, self.detail_worktrees.len()),
                RepoDetailFocus::Tickets => (self.detail_ticket_index, self.detail_tickets.len()),
            },
            View::WorktreeDetail => (self.agent_event_index, self.data.agent_events.len()),
            View::Tickets => (self.ticket_index, self.data.tickets.len()),
            View::Session => match self.session_focus {
                SessionFocus::Worktrees => {
                    (self.session_wt_index, self.data.session_worktrees.len())
                }
                SessionFocus::History => {
                    (self.session_history_index, self.data.session_history.len())
                }
            },
        }
    }

    /// Sets the index for the currently focused pane.
    pub fn set_focused_index(&mut self, index: usize) {
        match self.view {
            View::Dashboard => match self.dashboard_focus {
                DashboardFocus::Repos => self.repo_index = index,
                DashboardFocus::Worktrees => self.worktree_index = index,
                DashboardFocus::Tickets => self.ticket_index = index,
            },
            View::RepoDetail => match self.repo_detail_focus {
                RepoDetailFocus::Worktrees => self.detail_wt_index = index,
                RepoDetailFocus::Tickets => self.detail_ticket_index = index,
            },
            View::WorktreeDetail => self.agent_event_index = index,
            View::Tickets => self.ticket_index = index,
            View::Session => match self.session_focus {
                SessionFocus::Worktrees => self.session_wt_index = index,
                SessionFocus::History => self.session_history_index = index,
            },
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
