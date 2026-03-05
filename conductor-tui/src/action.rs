use std::collections::HashMap;

use conductor_core::agent::{AgentRun, TicketAgentTotals};
use conductor_core::github::DiscoveredRepo;
use conductor_core::repo::Repo;
use conductor_core::tickets::Ticket;
use conductor_core::worktree::Worktree;
use crossterm::event::KeyEvent;

/// Payload for the DataRefreshed action (boxed to keep Action enum small).
#[derive(Debug)]
pub struct GithubDiscoverPayload {
    /// Owner whose repos were fetched ("" = personal account).
    pub owner: String,
    pub repos: Vec<DiscoveredRepo>,
    /// HTTPS/SSH URLs of repos already registered in Conductor
    pub registered_urls: Vec<String>,
}

/// Payload for the DataRefreshed action (boxed to keep Action enum small).
#[derive(Debug)]
pub struct DataRefreshedPayload {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
    pub tickets: Vec<Ticket>,
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
    // CRUD triggers
    AddRepo,
    Create,
    Delete,
    Push,
    CreatePr,
    SyncTickets,
    LinkTicket,
    StartWork,
    SelectWorkTarget(usize),
    ManageWorkTargets,
    WorkTargetMoveUp,
    WorkTargetMoveDown,
    WorkTargetAdd,
    WorkTargetDelete,
    ManageIssueSources,
    IssueSourceAdd,
    IssueSourceDelete,

    // GitHub repo discovery — org level
    DiscoverGithubOrgs,
    GithubOrgsLoaded {
        orgs: Vec<String>,
    },
    GithubOrgsFailed {
        error: String,
    },
    /// Drill into an owner's repos. Empty string = personal account.
    GithubDrillIntoOwner {
        owner: String,
    },
    /// Go back from the repo list to the org list.
    GithubBackToOrgs,

    // GitHub repo discovery — repo level
    GithubDiscoverLoaded(Box<GithubDiscoverPayload>),
    GithubDiscoverFailed {
        error: String,
    },
    GithubDiscoverToggle,
    GithubDiscoverSelectAll,
    GithubDiscoverImport,

    // Model configuration
    SetModel,

    // Agent issue creation toggle (repo-level)
    ToggleAgentIssues,

    // Agent triggers (tmux-based)
    LaunchAgent,
    OrchestrateAgent,
    StopAgent,
    ViewAgentLog,
    CopyLastCodeBlock,
    ExpandAgentEvent,
    AgentActivityDown,
    AgentActivityUp,
    ScrollLeft,
    ScrollRight,

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
    TextAreaInput(KeyEvent),
    TextAreaClear,
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
