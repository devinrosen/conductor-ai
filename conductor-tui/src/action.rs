use std::collections::HashMap;

use conductor_core::agent::{AgentRun, AgentRunEvent, FeedbackRequest, TicketAgentTotals};
use conductor_core::feature::FeatureRow;
use conductor_core::github::DiscoveredRepo;
use conductor_core::repo::Repo;
use conductor_core::tickets::{Ticket, TicketLabel};
use conductor_core::workflow::{
    WorkflowDef, WorkflowRun, WorkflowRunStep, WorkflowStepSummary, WorkflowWarning,
};
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
    /// Pre-computed repo slug for each def in `workflow_defs` (parallel vec, same length).
    /// Empty in worktree-scoped mode (single repo implied).
    pub workflow_def_slugs: Option<Vec<String>>,
    pub workflow_runs: Vec<WorkflowRun>,
    pub workflow_steps: Vec<WorkflowRunStep>,
    /// Agent events for the selected step's child_run_id (live activity)
    pub step_agent_events: Vec<AgentRunEvent>,
    /// Agent run metadata for the selected step's child_run_id
    pub step_agent_run: Option<AgentRun>,
    /// Structured parse warnings for any `.wf` files that failed to load
    pub workflow_parse_warnings: Vec<WorkflowWarning>,
    /// Steps for every leaf run in the current scope (run_id → ordered steps).
    pub all_run_steps: HashMap<String, Vec<WorkflowRunStep>>,
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
    /// All pending agent feedback requests (for cross-process notifications).
    pub pending_feedback_requests: Vec<FeedbackRequest>,
    /// All waiting gate steps with their workflow name and optional target label (for cross-process notifications).
    pub waiting_gate_steps: Vec<(WorkflowRunStep, String, Option<String>)>,
    /// Live turn count for currently running agents, keyed by worktree_id.
    /// Computed in the background poller to avoid blocking the main thread.
    pub live_turns_by_worktree: HashMap<String, i64>,
    /// Active features per repo (repo_id → active FeatureRows).
    pub features_by_repo: HashMap<String, Vec<FeatureRow>>,
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

    FocusContentColumn,
    FocusWorkflowColumn,
    ToggleWorkflowColumn,
    // CRUD triggers
    RegisterRepo,
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

    // Base branch change (worktree detail)
    SetBaseBranch,
    BaseBranchesLoaded {
        repo_slug: String,
        wt_slug: String,
        worktree_id: String,
        items: Vec<crate::state::BranchPickerItem>,
    },
    BaseBranchesFailed {
        error: String,
    },
    SelectBaseBranch(Option<usize>),

    // Theme picker
    ShowThemePicker,
    /// Background result: theme directory scan completed; open the picker modal.
    ThemesLoaded {
        themes: Vec<(String, String)>,
        /// Pre-loaded `Theme` objects corresponding 1-to-1 with `themes`.
        /// Loaded off-thread so keypress preview is a pure in-memory lookup.
        loaded_themes: Vec<crate::theme::Theme>,
        warnings: Vec<String>,
    },
    /// Temporarily apply the theme at this index (live preview while browsing).
    ThemePreview(usize),
    /// Background result: config write after theme selection completed.
    ThemeSaveComplete {
        result: Result<String, String>,
    },

    // Agent issue creation toggle (repo-level)
    ToggleAgentIssues,

    // Toggle visibility of closed tickets in all ticket views
    ToggleClosedTickets,

    // Toggle visibility of completed/cancelled workflow runs in the workflow column
    ToggleCompletedRuns,

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
    CopyErrorMessage,
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
    FormToggle,

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
    RepoUnregisterComplete {
        repo_slug: String,
        result: Result<(), String>,
    },
    GithubImportComplete {
        imported: usize,
        errors: Vec<String>,
    },

    // Branch picker (during worktree creation)
    /// Background result: feature branches loaded for branch picker.
    FeatureBranchesLoaded {
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
        items: Vec<crate::state::BranchPickerItem>,
    },
    /// Background result: feature branch load failed.
    FeatureBranchesFailed {
        error: String,
    },
    SelectBranch(Option<usize>),

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
    /// Toggle expand/collapse for the hovered parent run row.
    ToggleWorkflowRunCollapse,
    /// Open a workflow picker for the current context (worktree, PR, etc.)
    PickWorkflow,
    RunWorkflow,
    RunPrWorkflow,
    ResumeWorkflow,
    /// Resume the latest failed/paused workflow run for the selected worktree (WorktreeDetail view).
    ResumeWorktreeWorkflow,
    CancelWorkflow,
    ApproveGate,
    RejectGate,
    /// View the selected workflow definition's YAML source in a scrollable modal.
    ViewWorkflowDef,
    /// Open the selected workflow definition's source file in $EDITOR.
    EditWorkflowDef,
    /// Enter or exit the step tree pane for the selected workflow definition.
    ToggleDefStepTree,
    GateInputChar(char),
    GateInputBackspace,
    WorkflowDataRefreshed(Box<WorkflowDataPayload>),

    // Feature actions (dashboard feature header rows)
    FeatureDetail {
        repo_idx: usize,
        feature_idx: usize,
        total: usize,
        merged: usize,
    },

    // Timer tick — also triggers workflow data refresh on workflow views
    Tick,

    // No-op (unhandled key)
    None,
}
