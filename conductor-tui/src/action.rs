use conductor_core::repo::Repo;
use conductor_core::session::Session;
use conductor_core::tickets::Ticket;
use conductor_core::worktree::Worktree;

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
    Create,
    Delete,
    Push,
    CreatePr,
    SyncTickets,
    LinkTicket,
    StartSession,
    EndSession,

    // Filter
    EnterFilter,
    FilterChar(char),
    FilterBackspace,
    ExitFilter,

    // Modal
    ShowHelp,
    DismissModal,
    ConfirmYes,
    ConfirmNo,
    InputChar(char),
    InputBackspace,
    InputSubmit,

    // Background results
    DataRefreshed {
        repos: Vec<Repo>,
        worktrees: Vec<Worktree>,
        tickets: Vec<Ticket>,
        session: Option<Session>,
        session_worktrees: Vec<Worktree>,
    },
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

    // No-op (unhandled key)
    None,
}
