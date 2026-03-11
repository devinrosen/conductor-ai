use std::collections::HashMap;

use conductor_core::agent::{AgentRun, AgentRunEvent, TicketAgentTotals};
use conductor_core::github::DiscoveredRepo;
use conductor_core::repo::Repo;
use conductor_core::tickets::{Ticket, TicketLabel};
use conductor_core::workflow::{WorkflowDef, WorkflowRun, WorkflowRunStep, WorkflowStepSummary};
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

/// Payload for workflow data refresh (workflow runs + steps for current context).
#[derive(Debug)]
pub struct WorkflowDataPayload {
    pub workflow_defs: Option<Vec<WorkflowDef>>, // None = defs not re-scanned, keep existing
    pub workflow_runs: Vec<WorkflowRun>,
    pub workflow_steps: Vec<WorkflowRunStep>,
    /// Agent events for the selected step's child_run_id (live activity)
    pub step_agent_events: Vec<AgentRunEvent>,
    /// Agent run metadata for the selected step's child_run_id
    pub step_agent_run: Option<AgentRun>,
}

/// Payload for the DataRefreshed action (boxed to keep Action enum small).
#[derive(Debug)]
pub struct DataRefreshedPayload {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
    pub tickets: Vec<Ticket>,
    pub ticket_labels: HashMap<String, Vec<TicketLabel>>,
    pub latest_agent_runs: HashMap<String, AgentRun>,
    pub ticket_agent_totals: HashMap<String, TicketAgentTotals>,
    /// Most recent workflow run per worktree (for inline indicators in the Worktrees panel).
    pub latest_workflow_runs_by_worktree: HashMap<String, WorkflowRun>,
    /// Currently-running step summary per workflow_run_id (for inline step indicators).
    pub workflow_step_summaries: HashMap<String, WorkflowStepSummary>,
    /// Active root workflow runs with no associated worktree (repo/ticket-targeted).
    pub active_non_worktree_workflow_runs: Vec<WorkflowRun>,
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
    GoToWorkflows,
    // CRUD triggers
    AddRepo,
    Create,
    Delete,
    #[allow(dead_code)]
    Push,
    #[allow(dead_code)]
    CreatePr,
    SyncTickets,
    #[allow(dead_code)]
    LinkTicket,
    ManageIssueSources,
    IssueSourceAdd,
    IssueSourceDelete,

    // GitHub repo discovery — org level
    #[allow(dead_code)]
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

    // Toggle visibility of closed tickets in all ticket views
    ToggleClosedTickets,

    // Toggle the global status bar expanded/collapsed (for 4+ active items)
    ToggleStatusBar,

    // Agent triggers (tmux-based)
    LaunchAgent,
    OrchestrateAgent,
    StopAgent,
    #[allow(dead_code)]
    CopyLastCodeBlock,
    ExpandAgentEvent,
    AgentActivityDown,
    AgentActivityUp,
    SubmitFeedback,
    DismissFeedback,
    /// Copy context-dependent value: in InfoPanel copies selected row value; in LogPanel copies last code block.
    WorktreeDetailCopy,
    /// Act on the selected info panel row: Path → open tmux window, Ticket → show ticket modal, PR → open browser.
    WorktreeDetailOpen,
    /// Act on the selected row in the RepoDetail info pane.
    RepoDetailInfoOpen,
    /// Copy the value of the selected row in the RepoDetail info pane.
    RepoDetailInfoCopy,
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
    EnterLabelFilter,
    FilterChar(char),
    FilterBackspace,
    ExitFilter,

    // Modal
    ShowHelp,
    DismissModal,
    OpenTicketUrl,
    CopyTicketUrl,
    OpenRepoUrl,
    CopyRepoUrl,
    OpenPrUrl,
    CopyPrUrl,
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
    PrsRefreshed {
        repo_id: String,
        prs: Vec<conductor_core::github::GithubPr>,
    },
    DataRefreshed(Box<DataRefreshedPayload>),
    TicketSyncComplete {
        repo_slug: String,
        count: usize,
    },
    TicketSyncFailed {
        repo_slug: String,
        error: String,
    },
    /// Sent after all repos have been processed in a manual one-shot sync.
    TicketSyncDone,
    #[allow(dead_code)]
    BackgroundError {
        message: String,
    },
    #[allow(dead_code)]
    BackgroundSuccess {
        message: String,
    },

    // Background results for worktree creation
    WorktreeCreated {
        wt_id: String,
        wt_path: String,
        wt_slug: String,
        wt_repo_id: String,
        warnings: Vec<String>,
        ticket_id: Option<String>,
    },
    WorktreeCreateFailed {
        message: String,
    },

    // Background result for worktree delete readiness check
    DeleteWorktreeReady {
        repo_slug: String,
        wt_slug: String,
        issue_closed: bool,
        pr_merged: bool,
        has_ticket: bool,
    },

    // Background results for async blocking operations
    PushComplete {
        result: Result<String, String>,
    },
    PrCreateComplete {
        result: Result<String, String>,
    },
    WorktreeDeleteComplete {
        wt_slug: String,
        result: Result<String, String>,
    },
    RepoRemoveComplete {
        repo_slug: String,
        result: Result<(), String>,
    },
    GithubImportComplete {
        imported: usize,
        errors: Vec<String>,
    },

    // Post-create picker (after worktree creation)
    SelectPostCreateChoice(usize),
    /// Background result: workflow defs loaded, ready to show post-create picker.
    PostCreatePickerReady {
        items: Vec<crate::state::PostCreateChoice>,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
        repo_path: String,
    },

    // Workflow actions
    /// Open a workflow picker for the current context (worktree, PR, etc.)
    PickWorkflow,
    RunWorkflow,
    RunPrWorkflow,
    ResumeWorkflow,
    CancelWorkflow,
    ApproveGate,
    RejectGate,
    /// View the selected workflow definition's YAML source in a scrollable modal.
    ViewWorkflowDef,
    /// Open the selected workflow definition's source file in $EDITOR.
    EditWorkflowDef,
    GateInputChar(char),
    GateInputBackspace,
    WorkflowDataRefreshed(Box<WorkflowDataPayload>),

    // Timer tick — also triggers workflow data refresh on workflow views
    Tick,

    // No-op (unhandled key)
    None,
}
