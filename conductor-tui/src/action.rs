use conductor_core::repo::Repo;
use conductor_core::session::Session;
use conductor_core::tickets::Ticket;
use conductor_core::worktree::Worktree;

use std::collections::HashMap;

use conductor_core::agent::{AgentRun, TicketAgentTotals};

/// Payload for the DataRefreshed action (boxed to keep Action enum small).
#[derive(Debug)]
pub struct DataRefreshedPayload {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
    pub tickets: Vec<Ticket>,
    pub session: Option<Session>,
    pub session_worktrees: Vec<Worktree>,
    pub latest_agent_runs: HashMap<String, AgentRun>,
    pub ticket_agent_totals: HashMap<String, TicketAgentTotals>,
}

/// Every user intent or background result flows through this enum.
#[derive(Debug)]
pub enum Action {
    // Navigation
    Quit,
    Back,
    NextPanel,
    PrevPanel,
    MoveUp,
    MoveDown,
    Select,

    // Views
    GoToDashboard,
    GoToTickets,
    GoToSession,

    // CRUD triggers
    AddRepo,
    Create,
    Delete,
    Push,
    CreatePr,
    SyncTickets,
    LinkTicket,
    StartSession,
    EndSession,
    AttachWorktree,
    StartWork,
    SelectWorkTarget(usize),
    ManageWorkTargets,
    WorkTargetMoveUp,
    WorkTargetMoveDown,
    WorkTargetAdd,
    WorkTargetDelete,

    // Agent triggers (tmux-based)
    LaunchAgent,
    StopAgent,
    ViewAgentLog,
    AgentActivityDown,
    AgentActivityUp,

    // Scroll navigation (all views)
    GoToTop,
    GoToBottom,
    HalfPageDown,
    HalfPageUp,
    PendingG,

    // Filter
    EnterFilter,
    FilterChar(char),
    FilterBackspace,
    ExitFilter,

    // Modal
    ShowHelp,
    DismissModal,
    OpenTicketUrl,
    ConfirmYes,
    ConfirmNo,
    InputChar(char),
    InputBackspace,
    InputSubmit,
    FormChar(char),
    FormBackspace,
    FormNextField,
    FormPrevField,
    FormSubmit,

    // Background results
    DataRefreshed(Box<DataRefreshedPayload>),
    TicketSyncComplete {
        repo_slug: String,
        count: usize,
    },
    TicketSyncFailed {
        repo_slug: String,
        error: String,
    },
    #[allow(dead_code)]
    BackgroundError {
        message: String,
    },
    #[allow(dead_code)]
    BackgroundSuccess {
        message: String,
    },

    // Timer tick (no-op, just wakes the main loop)
    Tick,

    // No-op (unhandled key)
    None,
}
