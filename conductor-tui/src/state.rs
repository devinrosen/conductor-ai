use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone)]
pub struct FilterState {
    pub active: bool,
    pub text: String,
}

impl FilterState {
    pub fn enter(&mut self) {
        self.active = true;
        self.text.clear();
    }
    pub fn exit(&mut self) {
        self.active = false;
    }
    pub fn push(&mut self, c: char) {
        self.text.push(c);
    }
    pub fn backspace(&mut self) {
        self.text.pop();
    }
    pub fn as_query(&self) -> Option<String> {
        if self.active || !self.text.is_empty() {
            Some(self.text.to_lowercase())
        } else {
            None
        }
    }
}

use conductor_core::agent::{
    AgentCreatedIssue, AgentRun, AgentRunEvent, AgentRunStatus, FeedbackRequest, TicketAgentTotals,
};
use conductor_core::github::{DiscoveredRepo, GithubPr};
use conductor_core::issue_source::IssueSource;
use conductor_core::repo::Repo;
use conductor_core::tickets::{Ticket, TicketLabel};
use conductor_core::workflow::{
    WorkflowDef, WorkflowRun, WorkflowRunStatus, WorkflowRunStep, WorkflowStepSummary,
};

use crate::theme::Theme;

/// A single active item for the global status bar detail line.
#[derive(Debug, Clone)]
pub enum GlobalStatusItem {
    Agent {
        worktree_slug: String,
        status: AgentRunStatus,
        /// Elapsed seconds since the run started (0 if unknown).
        elapsed_secs: u64,
    },
    Workflow {
        /// Display label for the context: worktree slug, repo slug, or `#<source_id>` for tickets.
        context_label: String,
        status: WorkflowRunStatus,
        /// Elapsed seconds since the agent run for the current step started (0 if unknown).
        elapsed_secs: u64,
        /// Current step name, if known.
        current_step: Option<String>,
        /// Ordered list of workflow names from root down to the workflow containing the
        /// currently-running step. Empty for single-level (non-nested) workflows.
        workflow_chain: Vec<String>,
    },
}

/// Aggregated global status for the persistent top status bar.
#[derive(Debug, Clone, Default)]
pub struct GlobalStatus {
    pub running_agents: usize,
    pub waiting_agents: usize,
    pub running_workflows: usize,
    pub waiting_workflows: usize,
    /// Active items sorted by priority (waiting first, then running).
    pub active_items: Vec<GlobalStatusItem>,
}

impl GlobalStatus {
    pub fn total_active(&self) -> usize {
        self.active_items.len()
    }
}
use conductor_core::worktree::Worktree;
use ratatui::widgets::ListState;
use tui_textarea::TextArea;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    RepoDetail,
    WorktreeDetail,
    Tickets,
    Workflows,
    WorkflowRunDetail,
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
pub enum RepoDetailFocus {
    Info,
    Worktrees,
    Tickets,
    Prs,
}

impl RepoDetailFocus {
    pub fn next(self) -> Self {
        match self {
            Self::Info => Self::Worktrees,
            Self::Worktrees => Self::Tickets,
            Self::Tickets => Self::Prs,
            Self::Prs => Self::Info,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Info => Self::Prs,
            Self::Worktrees => Self::Info,
            Self::Tickets => Self::Worktrees,
            Self::Prs => Self::Tickets,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowsFocus {
    Defs,
    Runs,
}

/// A row in the visible workflow runs list.
/// Either a root/parent run or an indented child run.
#[derive(Debug, Clone)]
pub enum WorkflowRunRow {
    Parent {
        run_id: String,
        collapsed: bool,
        child_count: usize,
    },
    Child {
        run_id: String,
        #[allow(dead_code)]
        parent_id: String,
    },
}

impl WorkflowRunRow {
    pub fn run_id(&self) -> &str {
        match self {
            WorkflowRunRow::Parent { run_id, .. } => run_id,
            WorkflowRunRow::Child { run_id, .. } => run_id,
        }
    }
}

impl WorkflowsFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Defs => Self::Runs,
            Self::Runs => Self::Defs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRunDetailFocus {
    Steps,
    AgentActivity,
}

impl WorkflowRunDetailFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Steps => Self::AgentActivity,
            Self::AgentActivity => Self::Steps,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorktreeDetailFocus {
    #[default]
    InfoPanel,
    LogPanel,
}

impl WorktreeDetailFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::InfoPanel => Self::LogPanel,
            Self::LogPanel => Self::InfoPanel,
        }
    }
}

/// Named row indices for the WorktreeDetail info panel.
/// These constants must stay in sync with the row order rendered in
/// `ui/worktree_detail.rs`. Centralising them here eliminates magic numbers
/// in `app.rs` and makes row-order changes an explicit, single-site update.
pub mod info_row {
    pub const SLUG: usize = 0;
    pub const REPO: usize = 1;
    pub const BRANCH: usize = 2;
    pub const BASE: usize = 3;
    pub const PATH: usize = 4;
    pub const STATUS: usize = 5;
    pub const MODEL: usize = 6;
    pub const CREATED: usize = 7;
    pub const TICKET: usize = 8;
    /// Total number of navigable rows (used for bounds clamping).
    pub const COUNT: usize = 9;
}

/// Named row indices for the RepoDetail info panel.
/// These constants must stay in sync with the row order rendered in
/// `ui/repo_detail.rs`.
pub mod repo_info_row {
    pub const SLUG: usize = 0;
    pub const REMOTE: usize = 1;
    #[allow(dead_code)]
    pub const BRANCH: usize = 2;
    pub const PATH: usize = 3;
    pub const WORKTREES_DIR: usize = 4;
    pub const MODEL: usize = 5;
    pub const AGENT_ISSUES: usize = 6;
    /// Total number of navigable rows (used for bounds clamping).
    pub const COUNT: usize = 7;
}

/// Choice offered in the post-worktree-creation picker.
#[derive(Clone, Debug)]
pub enum PostCreateChoice {
    StartAgent,
    RunWorkflow { name: String, def: WorkflowDef },
    Skip,
}

impl std::fmt::Display for PostCreateChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PostCreateChoice::StartAgent => write!(f, "Start agent"),
            PostCreateChoice::RunWorkflow { name, .. } => write!(f, "Run: {name}"),
            PostCreateChoice::Skip => write!(f, "Skip"),
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
    IssueSourceManager {
        repo_id: String,
        repo_slug: String,
        remote_url: String,
        sources: Vec<IssueSource>,
        selected: usize,
    },
    /// Full-screen detail view for a single agent event.
    EventDetail {
        title: String,
        body: String,
        line_count: usize,
        scroll_offset: u16,
        horizontal_offset: u16,
    },
    /// First level: pick a GitHub org (or personal account) to browse repos from.
    GithubDiscoverOrgs {
        /// Org login names; "Personal" (displayed) maps to empty owner string internally.
        orgs: Vec<String>,
        cursor: usize,
        loading: bool,
        error: Option<String>,
    },
    /// Model picker with curated list and effective default display.
    ModelPicker {
        /// Label for the context (e.g. "worktree: my-wt" or "repo: my-repo")
        context_label: String,
        /// The currently effective model from the resolution chain, if any.
        effective_default: Option<String>,
        /// Where the effective default came from (e.g. "global config", "repo", "worktree")
        effective_source: String,
        /// Index of the currently selected item in the list (0..=KNOWN_MODELS.len() for custom)
        selected: usize,
        /// Custom model input text (when user selects "custom…")
        custom_input: String,
        /// Whether the custom input line is active
        custom_active: bool,
        /// Suggested model alias based on prompt keywords (e.g. "haiku", "opus"), if any
        suggested: Option<String>,
        /// What to do when a model is selected
        on_submit: InputAction,
    },
    /// Gate action modal for approving/rejecting a workflow gate step.
    GateAction {
        #[allow(dead_code)]
        run_id: String,
        step_id: String,
        gate_prompt: String,
        feedback: String,
    },
    /// Confirm-by-name modal: user must type the expected slug to confirm.
    ConfirmByName {
        title: String,
        message: String,
        /// The slug the user must type to confirm.
        expected: String,
        /// Current user input.
        value: String,
        on_confirm: ConfirmAction,
    },
    /// Second level: browse and import repos for a specific owner.
    GithubDiscover {
        /// Owner whose repos are shown ("" = personal account).
        owner: String,
        repos: Vec<DiscoveredRepo>,
        /// HTTPS/SSH URLs of already-registered repos, for duplicate detection
        registered_urls: Vec<String>,
        /// Per-repo selection state (parallel to `repos`)
        selected: Vec<bool>,
        cursor: usize,
        loading: bool,
        error: Option<String>,
    },
    /// Post-worktree-creation picker: start agent, run a workflow, or skip.
    PostCreatePicker {
        items: Vec<PostCreateChoice>,
        selected: usize,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
        repo_path: String,
    },
    /// Workflow picker for running a PR-targeted workflow against a selected PR.
    PrWorkflowPicker {
        pr_number: i64,
        pr_title: String,
        workflow_defs: Vec<WorkflowDef>,
        selected: usize,
    },
    /// Generic workflow picker — opened by `w` key in any context.
    WorkflowPicker {
        target: WorkflowPickerTarget,
        workflow_defs: Vec<WorkflowDef>,
        selected: usize,
    },
    /// Non-dismissable progress indicator shown while a background operation runs.
    Progress {
        message: String,
    },
    /// In-TUI theme picker: browse named themes with live preview.
    ThemePicker {
        /// Index into `KNOWN_THEMES`.
        selected: usize,
        /// Theme active when the picker was opened; restored on Esc.
        original_theme: crate::theme::Theme,
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
            Modal::IssueSourceManager { .. } => write!(f, "Modal::IssueSourceManager"),
            Modal::ModelPicker {
                ref context_label, ..
            } => {
                write!(f, "Modal::ModelPicker(ctx={context_label:?})")
            }
            Modal::ConfirmByName { title, .. } => f
                .debug_struct("ConfirmByName")
                .field("title", title)
                .finish(),
            Modal::GateAction { .. } => write!(f, "Modal::GateAction"),
            Modal::EventDetail { .. } => write!(f, "Modal::EventDetail"),
            Modal::GithubDiscoverOrgs { loading, .. } => {
                write!(f, "Modal::GithubDiscoverOrgs(loading={loading})")
            }
            Modal::GithubDiscover { owner, loading, .. } => {
                write!(
                    f,
                    "Modal::GithubDiscover(owner={owner:?}, loading={loading})"
                )
            }
            Modal::PostCreatePicker { .. } => write!(f, "Modal::PostCreatePicker"),
            Modal::PrWorkflowPicker {
                pr_number,
                pr_title,
                ..
            } => write!(f, "Modal::PrWorkflowPicker(#{pr_number} {pr_title:?})"),
            Modal::WorkflowPicker { ref target, .. } => {
                write!(f, "Modal::WorkflowPicker(target={target:?})")
            }
            Modal::Progress { message } => {
                write!(f, "Modal::Progress({message:?})")
            }
            Modal::ThemePicker { selected, .. } => {
                write!(f, "Modal::ThemePicker(selected={selected})")
            }
        }
    }
}

/// Target context for the generic workflow picker.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum WorkflowPickerTarget {
    Worktree {
        worktree_id: String,
        worktree_path: String,
        repo_path: String,
    },
    Pr {
        pr_number: i64,
        pr_title: String,
    },
    Ticket {
        ticket_id: String,
        ticket_title: String,
        ticket_url: String,
        repo_id: String,
        repo_path: String,
    },
    Repo {
        repo_id: String,
        repo_path: String,
        repo_name: String,
    },
    WorkflowRun {
        workflow_run_id: String,
        workflow_name: String,
        worktree_id: Option<String>,
        worktree_path: Option<String>,
        repo_path: String,
    },
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Carry creation params through the clone-warning confirm flow.
    CreateWorktree {
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
        from_pr: Option<u32>,
    },
    DeleteWorktree {
        repo_slug: String,
        wt_slug: String,
    },
    RemoveRepo {
        repo_slug: String,
    },
    DeleteIssueSource {
        source_id: String,
        repo_id: String,
        repo_slug: String,
        remote_url: String,
    },
    CancelWorkflow {
        workflow_run_id: String,
    },
    ResumeWorkflow {
        workflow_run_id: String,
    },
    Quit,
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
#[allow(clippy::enum_variant_names)]
pub enum FormAction {
    AddRepo,
    AddIssueSource {
        repo_id: String,
        repo_slug: String,
        remote_url: String,
    },
}

#[derive(Debug, Clone)]
pub enum InputAction {
    CreateWorktree {
        repo_slug: String,
        ticket_id: Option<String>,
    },
    /// Second step of worktree creation: user optionally enters a PR number.
    CreateWorktreePrStep {
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
    },
    LinkTicket {
        worktree_id: String,
    },
    AgentPrompt {
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
    },
    OrchestratePrompt {
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
    },
    /// Second step: optionally override the model for this run.
    /// `resolved_default` is the already-resolved model (worktree → repo → global config).
    AgentModelOverride {
        prompt: String,
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        resume_session_id: Option<String>,
        resolved_default: Option<String>,
    },
    /// Set (or clear) the default model for a worktree.
    SetWorktreeModel {
        worktree_id: String,
        repo_slug: String,
        slug: String,
    },
    /// Set (or clear) the default model for a repo.
    SetRepoModel {
        repo_id: String,
        slug: String,
    },
    /// Submit a response to a pending feedback request.
    FeedbackResponse {
        feedback_id: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct DataCache {
    pub repos: Vec<Repo>,
    pub worktrees: Vec<Worktree>,
    pub tickets: Vec<Ticket>,
    /// ticket_id -> labels with colors (populated by DB poller)
    pub ticket_labels: HashMap<String, Vec<TicketLabel>>,
    /// repo_id -> slug for display
    pub repo_slug_map: HashMap<String, String>,
    /// ticket_id -> Ticket for lookups
    pub ticket_map: HashMap<String, Ticket>,
    /// repo_id -> worktree count
    pub repo_worktree_count: HashMap<String, usize>,
    /// worktree_id -> latest AgentRun (populated by DB poller)
    pub latest_agent_runs: HashMap<String, AgentRun>,
    /// Persisted agent events for the currently viewed worktree (from DB)
    pub agent_events: Vec<AgentRunEvent>,
    /// run_id -> (run_number, model, started_at) for per-run boundary headers
    pub agent_run_info: HashMap<String, (usize, Option<String>, String)>,
    /// Aggregate stats across all agent runs for the currently viewed worktree
    pub agent_totals: AgentTotals,
    /// Child runs of the latest root run (for run tree display)
    pub child_runs: Vec<AgentRun>,
    /// ticket_id -> aggregated agent stats across all linked worktrees
    pub ticket_agent_totals: HashMap<String, TicketAgentTotals>,
    /// ticket_id -> linked worktrees (most recently created first)
    pub ticket_worktrees: HashMap<String, Vec<Worktree>>,
    /// Issues created by agents for the currently viewed worktree
    pub agent_created_issues: Vec<AgentCreatedIssue>,
    /// Pending feedback request for the currently viewed worktree (if any)
    pub pending_feedback: Option<FeedbackRequest>,
    /// Most recent workflow run per worktree (worktree_id → run), for inline indicators.
    pub latest_workflow_runs_by_worktree: HashMap<String, WorkflowRun>,
    /// Currently-running step summary per workflow_run_id, for inline step indicators.
    pub workflow_step_summaries: HashMap<String, WorkflowStepSummary>,
    /// Active root workflow runs with no associated worktree (repo/ticket-targeted).
    pub active_non_worktree_workflow_runs: Vec<WorkflowRun>,
    /// Workflow definitions for the currently viewed worktree
    pub workflow_defs: Vec<WorkflowDef>,
    /// Pre-computed repo slug per def (parallel to `workflow_defs`).
    /// Populated by the background thread in global mode; empty in worktree-scoped mode.
    pub workflow_def_slugs: Vec<String>,
    /// Workflow runs for the currently viewed worktree (or all worktrees in global mode)
    pub workflow_runs: Vec<WorkflowRun>,
    /// Steps for the currently viewed workflow run
    pub workflow_steps: Vec<WorkflowRunStep>,
    /// Agent events for the currently selected workflow step's child_run_id
    pub step_agent_events: Vec<AgentRunEvent>,
    /// Agent run metadata for the currently selected step's child_run_id
    pub step_agent_run: Option<AgentRun>,
}

/// Aggregated stats across all agent runs for a worktree.
#[derive(Debug, Clone, Default)]
pub struct AgentTotals {
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub run_count: usize,
    /// Live turn count from the currently running agent's log file.
    pub live_turns: i64,
}

/// A row in the agent activity list: either a run-group separator or an event.
pub enum VisualRow<'a> {
    /// Separator row for a run group: (run_number, model, started_at).
    RunSeparator(usize, Option<&'a str>, &'a str),
    /// An actual agent event.
    Event(&'a AgentRunEvent),
}

impl DataCache {
    /// Count the number of run-boundary separator rows that would be inserted
    /// when there are multiple runs. Shared by `visual_rows` and
    /// `agent_activity_len` so the logic lives in one place.
    fn count_separators(&self) -> usize {
        if self.agent_run_info.len() <= 1 {
            return 0;
        }
        let mut count = 0;
        let mut prev_run_id: Option<&str> = None;
        for ev in &self.agent_events {
            if prev_run_id.is_none_or(|p| p != ev.run_id)
                && self.agent_run_info.contains_key(&ev.run_id)
            {
                count += 1;
            }
            prev_run_id = Some(&ev.run_id);
        }
        count
    }

    /// Iterate the agent activity list as visual rows, interleaving run-group
    /// separators when there are multiple runs. This is the single source of
    /// truth for the visual-index ↔ event mapping used by both the renderer
    /// and the action handler.
    pub fn visual_rows(&self) -> Vec<VisualRow<'_>> {
        let has_multiple_runs = self.agent_run_info.len() > 1;
        let mut rows = Vec::with_capacity(self.agent_events.len() + self.count_separators());
        let mut prev_run_id: Option<&str> = None;

        for ev in &self.agent_events {
            if has_multiple_runs && prev_run_id.is_none_or(|p| p != ev.run_id) {
                if let Some((run_num, model, started_at)) = self.agent_run_info.get(&ev.run_id) {
                    rows.push(VisualRow::RunSeparator(
                        *run_num,
                        model.as_deref(),
                        started_at.as_str(),
                    ));
                }
            }
            prev_run_id = Some(&ev.run_id);
            rows.push(VisualRow::Event(ev));
        }
        rows
    }

    /// Total number of items in the agent activity list, including run boundary
    /// separators. Must match the item count built in `render_agent_activity`.
    pub fn agent_activity_len(&self) -> usize {
        self.agent_events.len() + self.count_separators()
    }

    /// Map a visual index (which may include run-separator rows) back to the
    /// underlying `AgentRunEvent`. Returns `None` if the index points at a
    /// separator row or is out of range.
    pub fn event_at_visual_index(&self, visual_target: usize) -> Option<&AgentRunEvent> {
        match self.visual_rows().into_iter().nth(visual_target)? {
            VisualRow::Event(ev) => Some(ev),
            VisualRow::RunSeparator(..) => None,
        }
    }

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

        // Sort worktrees by (repo_slug, wt_slug) so that state.worktree_index
        // indexes into the same order that the dashboard renders them.
        self.worktrees.sort_by(|a, b| {
            let sa = self
                .repo_slug_map
                .get(&a.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            let sb = self
                .repo_slug_map
                .get(&b.repo_id)
                .map(|s| s.as_str())
                .unwrap_or("");
            sa.cmp(sb).then_with(|| a.slug.cmp(&b.slug))
        });

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
    pub modal: Modal,
    pub data: DataCache,

    // Selection indices
    pub repo_index: usize,
    pub worktree_index: usize,
    pub ticket_index: usize,
    // Detail view context
    pub selected_repo_id: Option<String>,
    pub selected_worktree_id: Option<String>,

    // Scoped lists for detail views
    pub detail_worktrees: Vec<Worktree>,
    pub detail_tickets: Vec<Ticket>,
    pub detail_prs: Vec<GithubPr>,
    pub detail_wt_index: usize,
    pub detail_ticket_index: usize,
    pub detail_pr_index: usize,
    /// When the PR list was last successfully fetched (None = never).
    pub pr_last_fetched_at: Option<std::time::Instant>,

    // Pre-filtered ticket lists (closed + text filter applied); index into these for nav/actions
    pub filtered_tickets: Vec<Ticket>,
    pub filtered_detail_tickets: Vec<Ticket>,

    // Agent activity list navigation (replaces the old Paragraph scroll offset)
    pub agent_list_state: RefCell<ListState>,
    /// Tracks pending `g` keypress for `gg` chord (go to top)
    pub pending_g: bool,

    // WorktreeDetail two-panel focus model
    pub worktree_detail_focus: WorktreeDetailFocus,
    /// Selected row index in the WorktreeDetail info panel (for j/k navigation and y/o actions).
    pub worktree_detail_selected_row: usize,

    /// Selected row index in the RepoDetail info panel (for j/k navigation and o actions).
    pub repo_detail_info_row: usize,

    // Filters
    pub filter: FilterState,
    pub detail_ticket_filter: FilterState,
    pub label_filter: FilterState,

    // Status bar message
    pub status_message: Option<String>,
    /// When `status_message` was last set; used to auto-clear after a timeout.
    pub status_message_at: Option<std::time::Instant>,

    /// Cached org list so navigating back from repo modal doesn't re-fetch.
    pub github_orgs_cache: Vec<String>,

    // Workflow state
    pub workflows_focus: WorkflowsFocus,
    pub workflow_def_index: usize,
    pub workflow_run_index: usize,
    pub workflow_step_index: usize,
    pub workflow_run_detail_focus: WorkflowRunDetailFocus,
    pub step_agent_event_index: usize,
    /// Currently selected workflow run ID (for detail view)
    pub selected_workflow_run_id: Option<String>,
    /// Set of parent workflow run IDs that are currently collapsed in the runs pane.
    pub collapsed_workflow_run_ids: HashSet<String>,
    /// Tracks which run IDs have had their default collapse state initialized.
    collapse_initialized: HashSet<String>,

    pub should_quit: bool,

    /// When false (default), closed tickets are hidden in all ticket views.
    pub show_closed_tickets: bool,

    /// Semantic colour theme — centralises all Color constants used by the UI.
    pub theme: Theme,

    /// True while a manual ticket sync is running in the background.
    pub ticket_sync_in_progress: bool,

    /// When true, force the global status bar detail line to expand even when
    /// there are 4+ active items (which default to the collapsed 1-line view).
    pub status_bar_expanded: bool,

    /// Cached home directory path for `~` substitution in path display. Never changes.
    pub home_dir: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            view: View::Dashboard,
            dashboard_focus: DashboardFocus::Repos,
            repo_detail_focus: RepoDetailFocus::Worktrees,
            modal: Modal::None,
            data: DataCache::default(),
            repo_index: 0,
            worktree_index: 0,
            ticket_index: 0,
            selected_repo_id: None,
            selected_worktree_id: None,
            detail_worktrees: Vec::new(),
            detail_tickets: Vec::new(),
            detail_prs: Vec::new(),
            detail_wt_index: 0,
            detail_ticket_index: 0,
            detail_pr_index: 0,
            pr_last_fetched_at: None,
            filtered_tickets: Vec::new(),
            filtered_detail_tickets: Vec::new(),
            agent_list_state: RefCell::new(ListState::default()),
            pending_g: false,
            worktree_detail_focus: WorktreeDetailFocus::InfoPanel,
            worktree_detail_selected_row: 0,
            repo_detail_info_row: 0,
            filter: FilterState::default(),
            detail_ticket_filter: FilterState::default(),
            label_filter: FilterState::default(),
            status_message: None,
            status_message_at: None,
            github_orgs_cache: Vec::new(),
            workflows_focus: WorkflowsFocus::Defs,
            workflow_def_index: 0,
            workflow_run_index: 0,
            workflow_step_index: 0,
            workflow_run_detail_focus: WorkflowRunDetailFocus::Steps,
            step_agent_event_index: 0,
            selected_workflow_run_id: None,
            collapsed_workflow_run_ids: HashSet::new(),
            collapse_initialized: HashSet::new(),
            should_quit: false,
            show_closed_tickets: false,
            ticket_sync_in_progress: false,
            status_bar_expanded: false,
            home_dir: dirs::home_dir().map(|p| p.to_string_lossy().into_owned()),
            theme: Theme::default(),
        }
    }

    /// Returns the filter that should receive input based on current view/focus.
    pub fn active_filter_mut(&mut self) -> &mut FilterState {
        if self.label_filter.active {
            &mut self.label_filter
        } else if self.view == View::RepoDetail
            && self.repo_detail_focus == RepoDetailFocus::Tickets
        {
            &mut self.detail_ticket_filter
        } else {
            &mut self.filter
        }
    }

    /// Returns the currently active filter (immutable), or None if no filter is active.
    pub fn active_filter(&self) -> Option<&FilterState> {
        if self.label_filter.active {
            Some(&self.label_filter)
        } else if self.filter.active {
            Some(&self.filter)
        } else if self.detail_ticket_filter.active {
            Some(&self.detail_ticket_filter)
        } else {
            None
        }
    }

    /// Returns whether any filter is currently active.
    pub fn any_filter_active(&self) -> bool {
        self.filter.active || self.detail_ticket_filter.active || self.label_filter.active
    }

    /// Compute the current global status from cached data.
    pub fn global_status(&self) -> GlobalStatus {
        let slug_map: HashMap<&str, &str> = self
            .data
            .worktrees
            .iter()
            .map(|wt| (wt.id.as_str(), wt.slug.as_str()))
            .collect();

        let resolve_slug = |id: &str| {
            slug_map
                .get(id)
                .map(|s| s.to_string())
                .unwrap_or_else(|| id.to_string())
        };

        let mut gs = GlobalStatus::default();

        // Collect worktree IDs that have an active workflow run.  These take
        // precedence over agent entries for the same worktree so we can avoid
        // double-counting in the header bar.
        let mut workflow_worktree_ids: std::collections::HashSet<&str> =
            std::collections::HashSet::new();

        // First pass: build Workflow items and collect their worktree IDs.
        for (wt_key, run) in &self.data.latest_workflow_runs_by_worktree {
            match run.status {
                WorkflowRunStatus::Running => gs.running_workflows += 1,
                WorkflowRunStatus::Waiting => gs.waiting_workflows += 1,
                _ => {}
            }
            if matches!(
                run.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Waiting
            ) {
                if let Some(wt_id) = run.worktree_id.as_deref() {
                    workflow_worktree_ids.insert(wt_id);
                } else {
                    // No worktree_id — use the map key as the dedup key.
                    workflow_worktree_ids.insert(wt_key.as_str());
                }

                let context_label = run
                    .worktree_id
                    .as_deref()
                    .map(&resolve_slug)
                    .unwrap_or_else(|| "(ephemeral)".to_string());

                // Borrow the agent run for this worktree (if any) to get elapsed.
                let elapsed_secs = run
                    .worktree_id
                    .as_deref()
                    .and_then(|id| self.data.latest_agent_runs.get(id))
                    .filter(|ar| ar.is_active())
                    .and_then(|ar| {
                        chrono::DateTime::parse_from_rfc3339(&ar.started_at)
                            .ok()
                            .map(|dt| {
                                let now = chrono::Utc::now();
                                now.signed_duration_since(dt).num_seconds().max(0) as u64
                            })
                    })
                    .unwrap_or(0);

                let (current_step, workflow_chain) = self
                    .data
                    .workflow_step_summaries
                    .get(&run.id)
                    .map(|s| (Some(s.step_name.clone()), s.workflow_chain.clone()))
                    .unwrap_or((None, Vec::new()));

                gs.active_items.push(GlobalStatusItem::Workflow {
                    context_label,
                    status: run.status.clone(),
                    elapsed_secs,
                    current_step,
                    workflow_chain,
                });
            }
        }

        // Third pass: build Workflow items for active non-worktree runs (repo/ticket-targeted).
        for run in &self.data.active_non_worktree_workflow_runs {
            match run.status {
                WorkflowRunStatus::Running => gs.running_workflows += 1,
                WorkflowRunStatus::Waiting => gs.waiting_workflows += 1,
                _ => {}
            }
            if matches!(
                run.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Waiting
            ) {
                // Derive a context label from repo or ticket, fallback to "(ephemeral)".
                // Use the pre-built maps from DataCache instead of allocating new ones.
                let context_label = if let Some(ref rid) = run.repo_id {
                    self.data
                        .repo_slug_map
                        .get(rid.as_str())
                        .map(|s| s.as_str())
                        .unwrap_or("(ephemeral)")
                        .to_string()
                } else if let Some(ref tid) = run.ticket_id {
                    self.data
                        .ticket_map
                        .get(tid.as_str())
                        .map(|t| format!("#{}", t.source_id))
                        .unwrap_or_else(|| "(ephemeral)".to_string())
                } else {
                    "(ephemeral)".to_string()
                };

                let (current_step, workflow_chain) = self
                    .data
                    .workflow_step_summaries
                    .get(&run.id)
                    .map(|s| (Some(s.step_name.clone()), s.workflow_chain.clone()))
                    .unwrap_or((None, Vec::new()));

                gs.active_items.push(GlobalStatusItem::Workflow {
                    context_label,
                    status: run.status.clone(),
                    elapsed_secs: 0,
                    current_step,
                    workflow_chain,
                });
            }
        }

        // Second pass: build Agent items, skipping worktrees covered by a workflow.
        for run in self.data.latest_agent_runs.values() {
            // Determine the dedup key for this agent run.
            let wt_id = run.worktree_id.as_deref().unwrap_or("");
            if !wt_id.is_empty() && workflow_worktree_ids.contains(wt_id) {
                // This worktree already has a Workflow item — skip the agent entry.
                continue;
            }

            match run.status {
                AgentRunStatus::Running => gs.running_agents += 1,
                AgentRunStatus::WaitingForFeedback => gs.waiting_agents += 1,
                _ => {}
            }
            if run.is_active() {
                let worktree_slug = run
                    .worktree_id
                    .as_deref()
                    .map(&resolve_slug)
                    .unwrap_or_else(|| "(ephemeral)".to_string());
                let elapsed_secs = chrono::DateTime::parse_from_rfc3339(&run.started_at)
                    .ok()
                    .map(|dt| {
                        let now = chrono::Utc::now();
                        now.signed_duration_since(dt).num_seconds().max(0) as u64
                    })
                    .unwrap_or(0);
                gs.active_items.push(GlobalStatusItem::Agent {
                    worktree_slug,
                    status: run.status.clone(),
                    elapsed_secs,
                });
            }
        }

        // Sort: waiting items first (they block progress), then running.
        gs.active_items.sort_by_key(|item| match item {
            GlobalStatusItem::Agent {
                status: AgentRunStatus::WaitingForFeedback,
                ..
            }
            | GlobalStatusItem::Workflow {
                status: WorkflowRunStatus::Waiting,
                ..
            } => 0u8,
            _ => 1,
        });

        gs
    }

    /// Number of lines the persistent header bar occupies.
    ///
    /// - 0 active items  → 1 line (just the Conductor title)
    /// - 1–3 active items → 2 lines (auto-expanded detail view)
    /// - 4+ active items  → 1 line by default; 2 lines when `status_bar_expanded`
    ///
    /// Accepts a pre-computed `GlobalStatus` so callers that already hold one
    /// (e.g. the render loop) don't trigger a second full recomputation.
    pub fn header_height(&self, gs: &GlobalStatus) -> u16 {
        let total = gs.total_active();
        if total == 0 {
            1
        } else if total <= 3 || self.status_bar_expanded {
            2
        } else {
            1
        }
    }

    /// Rebuild the pre-filtered ticket vecs from the current source data,
    /// `show_closed_tickets`, and the active text/label filters.  Must be called
    /// whenever any of those inputs change.
    pub fn rebuild_filtered_tickets(&mut self) {
        let filter_query = self.filter.as_query();
        let label_query = self.label_filter.as_query();
        self.filtered_tickets = self
            .data
            .tickets
            .iter()
            .filter(|t| self.show_closed_tickets || t.state != "closed")
            .filter(|t| match filter_query.as_deref() {
                Some(f) if !f.is_empty() => t.matches_filter(f),
                _ => true,
            })
            .filter(|t| match label_query.as_deref() {
                Some(f) if !f.is_empty() => self
                    .data
                    .ticket_labels
                    .get(&t.id)
                    .map(|labels| labels.iter().any(|l| l.label.to_lowercase().contains(f)))
                    .unwrap_or(false),
                _ => true,
            })
            .cloned()
            .collect();

        let slug_map = &self.data.repo_slug_map;
        self.filtered_tickets.sort_by(|a, b| {
            let sa = slug_map.get(&a.repo_id).map(|s| s.as_str()).unwrap_or("");
            let sb = slug_map.get(&b.repo_id).map(|s| s.as_str()).unwrap_or("");
            sa.cmp(sb).then_with(|| a.source_id.cmp(&b.source_id))
        });

        let detail_filter_query = self.detail_ticket_filter.as_query();
        self.filtered_detail_tickets = self
            .detail_tickets
            .iter()
            .filter(|t| self.show_closed_tickets || t.state != "closed")
            .filter(|t| match detail_filter_query.as_deref() {
                Some(f) if !f.is_empty() => t.matches_filter(f),
                _ => true,
            })
            .cloned()
            .collect();
    }

    /// Returns (current_index, list_length) for the currently focused pane.
    pub fn focused_index_and_len(&self) -> (usize, usize) {
        match self.view {
            View::Dashboard => match self.dashboard_focus {
                DashboardFocus::Repos => (self.repo_index, self.data.repos.len()),
                DashboardFocus::Worktrees => (self.worktree_index, self.data.worktrees.len()),
                DashboardFocus::Tickets => (self.ticket_index, self.filtered_tickets.len()),
            },
            View::RepoDetail => match self.repo_detail_focus {
                RepoDetailFocus::Info => (self.repo_detail_info_row, repo_info_row::COUNT),
                RepoDetailFocus::Worktrees => (self.detail_wt_index, self.detail_worktrees.len()),
                RepoDetailFocus::Tickets => {
                    (self.detail_ticket_index, self.filtered_detail_tickets.len())
                }
                RepoDetailFocus::Prs => (self.detail_pr_index, self.detail_prs.len()),
            },
            View::WorktreeDetail => {
                let idx = self.agent_list_state.borrow().selected().unwrap_or(0);
                (idx, self.data.agent_activity_len())
            }
            View::Tickets => (self.ticket_index, self.filtered_tickets.len()),
            View::Workflows => match self.workflows_focus {
                WorkflowsFocus::Defs => (self.workflow_def_index, self.data.workflow_defs.len()),
                WorkflowsFocus::Runs => (
                    self.workflow_run_index,
                    self.visible_workflow_run_rows().len(),
                ),
            },
            View::WorkflowRunDetail => match self.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Steps => {
                    (self.workflow_step_index, self.data.workflow_steps.len())
                }
                WorkflowRunDetailFocus::AgentActivity => (
                    self.step_agent_event_index,
                    self.data.step_agent_events.len(),
                ),
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
                RepoDetailFocus::Info => self.repo_detail_info_row = index,
                RepoDetailFocus::Worktrees => self.detail_wt_index = index,
                RepoDetailFocus::Tickets => self.detail_ticket_index = index,
                RepoDetailFocus::Prs => self.detail_pr_index = index,
            },
            View::WorktreeDetail => {
                self.agent_list_state.borrow_mut().select(Some(index));
            }
            View::Tickets => self.ticket_index = index,
            View::Workflows => match self.workflows_focus {
                WorkflowsFocus::Defs => self.workflow_def_index = index,
                WorkflowsFocus::Runs => self.workflow_run_index = index,
            },
            View::WorkflowRunDetail => match self.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Steps => self.workflow_step_index = index,
                WorkflowRunDetailFocus::AgentActivity => self.step_agent_event_index = index,
            },
        }
    }

    /// Returns the flat, ordered list of visible workflow run rows.
    /// Roots appear first; their expanded children follow immediately after.
    /// Runs returned DESC by the DB (newest first); children are sorted ASC (oldest first).
    pub fn visible_workflow_run_rows(&self) -> Vec<WorkflowRunRow> {
        let runs = &self.data.workflow_runs;
        let known_ids: HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        // Build parent_id → sorted-children map.
        let mut children_map: HashMap<&str, Vec<&WorkflowRun>> = HashMap::new();
        for run in runs {
            if let Some(ref parent_id) = run.parent_workflow_run_id {
                if known_ids.contains(parent_id.as_str()) {
                    children_map
                        .entry(parent_id.as_str())
                        .or_default()
                        .push(run);
                }
            }
        }
        // Sort children oldest-first (ascending by started_at).
        for children in children_map.values_mut() {
            children.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        }

        // IDs that are children of a known parent — skip when iterating roots.
        let child_ids: HashSet<&str> = children_map
            .values()
            .flat_map(|v| v.iter().map(|r| r.id.as_str()))
            .collect();

        let mut result = Vec::new();
        for run in runs {
            if child_ids.contains(run.id.as_str()) {
                continue;
            }
            let my_children = children_map
                .get(run.id.as_str())
                .cloned()
                .unwrap_or_default();
            let child_count = my_children.len();
            let collapsed = self.collapsed_workflow_run_ids.contains(&run.id);
            result.push(WorkflowRunRow::Parent {
                run_id: run.id.clone(),
                collapsed,
                child_count,
            });
            if !collapsed {
                for child in my_children {
                    result.push(WorkflowRunRow::Child {
                        run_id: child.id.clone(),
                        parent_id: run.id.clone(),
                    });
                }
            }
        }
        result
    }

    /// Auto-initialize collapse state for newly-seen terminal-status parent runs.
    /// Call this after updating `self.data.workflow_runs`.
    /// Terminal runs (completed/failed/cancelled) are collapsed on first appearance.
    pub fn init_collapse_state(&mut self) {
        for run in &self.data.workflow_runs {
            if self.collapse_initialized.contains(&run.id) {
                continue;
            }
            if run.parent_workflow_run_id.is_none() {
                let is_terminal = matches!(
                    run.status,
                    WorkflowRunStatus::Completed
                        | WorkflowRunStatus::Failed
                        | WorkflowRunStatus::Cancelled
                );
                if is_terminal {
                    self.collapsed_workflow_run_ids.insert(run.id.clone());
                }
            }
            self.collapse_initialized.insert(run.id.clone());
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

    /// Returns true if the currently selected workflow step has a child agent run.
    pub fn selected_step_has_agent(&self) -> bool {
        self.data
            .workflow_steps
            .get(self.workflow_step_index)
            .map(|s| s.child_run_id.is_some())
            .unwrap_or(false)
    }

    /// Called on each tick: clears `status_message` (and `status_message_at`) if
    /// the message has been visible for longer than `timeout`.
    pub(crate) fn tick_status_message(&mut self, timeout: Duration) {
        if let Some(at) = self.status_message_at {
            if at.elapsed() > timeout {
                self.status_message = None;
                self.status_message_at = None;
            }
        }
    }

    /// Updates `status_message_at` to reflect a change in `status_message` presence.
    /// `had_message` is whether a message was present before the action ran.
    pub(crate) fn track_status_message_change(&mut self, had_message: bool) {
        match (had_message, self.status_message.is_some()) {
            (false, true) => self.status_message_at = Some(Instant::now()),
            (_, false) => self.status_message_at = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::agent::AgentRunEvent;

    fn make_event(id: &str, run_id: &str) -> AgentRunEvent {
        AgentRunEvent {
            id: id.to_string(),
            run_id: run_id.to_string(),
            kind: "tool_use".to_string(),
            summary: "test".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            metadata: None,
        }
    }

    #[test]
    fn repo_detail_focus_next_cycles_forward() {
        assert_eq!(RepoDetailFocus::Info.next(), RepoDetailFocus::Worktrees);
        assert_eq!(RepoDetailFocus::Worktrees.next(), RepoDetailFocus::Tickets);
        assert_eq!(RepoDetailFocus::Tickets.next(), RepoDetailFocus::Prs);
        assert_eq!(RepoDetailFocus::Prs.next(), RepoDetailFocus::Info);
    }

    #[test]
    fn repo_detail_focus_prev_cycles_backward() {
        assert_eq!(RepoDetailFocus::Info.prev(), RepoDetailFocus::Prs);
        assert_eq!(RepoDetailFocus::Worktrees.prev(), RepoDetailFocus::Info);
        assert_eq!(RepoDetailFocus::Prs.prev(), RepoDetailFocus::Tickets);
        assert_eq!(RepoDetailFocus::Tickets.prev(), RepoDetailFocus::Worktrees);
    }

    #[test]
    fn repo_detail_focus_next_prev_are_inverses() {
        for focus in [
            RepoDetailFocus::Info,
            RepoDetailFocus::Worktrees,
            RepoDetailFocus::Tickets,
            RepoDetailFocus::Prs,
        ] {
            assert_eq!(focus.next().prev(), focus);
            assert_eq!(focus.prev().next(), focus);
        }
    }

    #[test]
    fn agent_activity_len_empty() {
        let cache = DataCache::default();
        assert_eq!(cache.agent_activity_len(), 0);
        assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
    }

    #[test]
    fn agent_activity_len_single_run() {
        let mut cache = DataCache {
            agent_events: vec![make_event("e1", "r1"), make_event("e2", "r1")],
            ..Default::default()
        };
        cache
            .agent_run_info
            .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
        // Single run -> no separators
        assert_eq!(cache.agent_activity_len(), 2);
        assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
    }

    #[test]
    fn agent_activity_len_multiple_runs() {
        let mut cache = DataCache {
            agent_events: vec![
                make_event("e1", "r1"),
                make_event("e2", "r1"),
                make_event("e3", "r2"),
            ],
            ..Default::default()
        };
        cache
            .agent_run_info
            .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
        cache
            .agent_run_info
            .insert("r2".into(), (2, None, "2026-01-01T00:01:00Z".into()));
        // 3 events + 2 separators = 5
        assert_eq!(cache.agent_activity_len(), 5);
        assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
    }

    #[test]
    fn agent_activity_len_interleaved_runs() {
        let mut cache = DataCache {
            agent_events: vec![
                make_event("e1", "r1"),
                make_event("e2", "r2"),
                make_event("e3", "r1"),
            ],
            ..Default::default()
        };
        cache
            .agent_run_info
            .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
        cache
            .agent_run_info
            .insert("r2".into(), (2, None, "2026-01-01T00:01:00Z".into()));
        // 3 events + 3 separators (r1, r2, r1 transitions) = 6
        assert_eq!(cache.agent_activity_len(), 6);
        assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
    }

    #[test]
    fn show_closed_tickets_defaults_to_false() {
        let state = AppState::new();
        assert!(!state.show_closed_tickets);
    }

    #[test]
    fn show_closed_tickets_toggle() {
        let mut state = AppState::new();
        assert!(!state.show_closed_tickets);
        state.show_closed_tickets = true;
        assert!(state.show_closed_tickets);
        state.show_closed_tickets = false;
        assert!(!state.show_closed_tickets);
    }

    fn make_workflow_run(
        worktree_id: &str,
        status: WorkflowRunStatus,
    ) -> conductor_core::workflow::WorkflowRun {
        conductor_core::workflow::WorkflowRun {
            id: "wfrun-1".into(),
            workflow_name: "test-workflow".into(),
            worktree_id: Some(worktree_id.into()),
            parent_run_id: "run-1".into(),
            status,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
        }
    }

    fn make_agent_run(
        worktree_id: &str,
        status: AgentRunStatus,
    ) -> conductor_core::agent::AgentRun {
        conductor_core::agent::AgentRun {
            id: "run-1".into(),
            worktree_id: Some(worktree_id.to_string()),
            claude_session_id: None,
            prompt: "do stuff".into(),
            status,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
        }
    }

    #[test]
    fn global_status_empty() {
        let state = AppState::new();
        let gs = state.global_status();
        assert_eq!(gs.total_active(), 0);
        assert!(gs.active_items.is_empty());
    }

    #[test]
    fn global_status_running_agent() {
        let mut state = AppState::new();
        state
            .data
            .latest_agent_runs
            .insert("wt1".into(), make_agent_run("wt1", AgentRunStatus::Running));
        let gs = state.global_status();
        assert_eq!(gs.running_agents, 1);
        assert_eq!(gs.waiting_agents, 0);
        assert_eq!(gs.total_active(), 1);
        assert_eq!(gs.active_items.len(), 1);
    }

    #[test]
    fn global_status_waiting_sorted_first() {
        let mut state = AppState::new();
        state
            .data
            .latest_agent_runs
            .insert("wt1".into(), make_agent_run("wt1", AgentRunStatus::Running));
        state.data.latest_agent_runs.insert(
            "wt2".into(),
            make_agent_run("wt2", AgentRunStatus::WaitingForFeedback),
        );
        let gs = state.global_status();
        assert_eq!(gs.running_agents, 1);
        assert_eq!(gs.waiting_agents, 1);
        assert_eq!(gs.total_active(), 2);
        // Waiting item should be sorted first
        assert!(matches!(
            &gs.active_items[0],
            GlobalStatusItem::Agent {
                status: AgentRunStatus::WaitingForFeedback,
                ..
            }
        ));
    }

    #[test]
    fn header_height_no_active() {
        let state = AppState::new();
        assert_eq!(state.header_height(&state.global_status()), 1);
    }

    #[test]
    fn header_height_few_active_auto_expands() {
        let mut state = AppState::new();
        state
            .data
            .latest_agent_runs
            .insert("wt1".into(), make_agent_run("wt1", AgentRunStatus::Running));
        let gs = state.global_status();
        assert_eq!(state.header_height(&gs), 2);
    }

    #[test]
    fn header_height_many_active_collapsed_by_default() {
        let mut state = AppState::new();
        for i in 0..5 {
            state.data.latest_agent_runs.insert(
                format!("wt{i}"),
                make_agent_run(&format!("wt{i}"), AgentRunStatus::Running),
            );
        }
        let gs = state.global_status();
        assert_eq!(state.header_height(&gs), 1);
    }

    #[test]
    fn header_height_many_active_expanded_when_toggled() {
        let mut state = AppState::new();
        for i in 0..5 {
            state.data.latest_agent_runs.insert(
                format!("wt{i}"),
                make_agent_run(&format!("wt{i}"), AgentRunStatus::Running),
            );
        }
        state.status_bar_expanded = true;
        let gs = state.global_status();
        assert_eq!(state.header_height(&gs), 2);
    }

    #[test]
    fn global_status_running_workflow() {
        let mut state = AppState::new();
        state.data.latest_workflow_runs_by_worktree.insert(
            "wt1".into(),
            make_workflow_run("wt1", WorkflowRunStatus::Running),
        );
        let gs = state.global_status();
        assert_eq!(gs.running_workflows, 1);
        assert_eq!(gs.waiting_workflows, 0);
        assert_eq!(gs.total_active(), 1);
        assert_eq!(gs.active_items.len(), 1);
        assert!(matches!(
            &gs.active_items[0],
            GlobalStatusItem::Workflow {
                status: WorkflowRunStatus::Running,
                ..
            }
        ));
    }

    #[test]
    fn global_status_waiting_workflow_sorted_first() {
        let mut state = AppState::new();
        state
            .data
            .latest_agent_runs
            .insert("wt1".into(), make_agent_run("wt1", AgentRunStatus::Running));
        state.data.latest_workflow_runs_by_worktree.insert(
            "wt2".into(),
            make_workflow_run("wt2", WorkflowRunStatus::Waiting),
        );
        let gs = state.global_status();
        assert_eq!(gs.running_agents, 1);
        assert_eq!(gs.waiting_workflows, 1);
        assert_eq!(gs.total_active(), 2);
        // Waiting workflow should be sorted before running agent
        assert!(matches!(
            &gs.active_items[0],
            GlobalStatusItem::Workflow {
                status: WorkflowRunStatus::Waiting,
                ..
            }
        ));
    }

    #[test]
    fn global_status_completed_and_failed_agents_excluded() {
        let mut state = AppState::new();
        state.data.latest_agent_runs.insert(
            "wt1".into(),
            make_agent_run("wt1", AgentRunStatus::Completed),
        );
        state
            .data
            .latest_agent_runs
            .insert("wt2".into(), make_agent_run("wt2", AgentRunStatus::Failed));
        let gs = state.global_status();
        assert_eq!(gs.total_active(), 0);
        assert!(gs.active_items.is_empty());
    }

    #[test]
    fn global_status_slug_resolved_from_worktree() {
        let mut state = AppState::new();
        state
            .data
            .worktrees
            .push(conductor_core::worktree::Worktree {
                id: "wt-id-1".into(),
                repo_id: "repo-1".into(),
                slug: "feat-my-feature".into(),
                branch: "feat/my-feature".into(),
                path: "/tmp/wt".into(),
                ticket_id: None,
                status: conductor_core::worktree::WorktreeStatus::Active,
                created_at: "2026-01-01T00:00:00Z".into(),
                completed_at: None,
                model: None,
                base_branch: None,
            });
        state.data.latest_agent_runs.insert(
            "wt-id-1".into(),
            make_agent_run("wt-id-1", AgentRunStatus::Running),
        );
        let gs = state.global_status();
        assert_eq!(gs.total_active(), 1);
        match &gs.active_items[0] {
            GlobalStatusItem::Agent { worktree_slug, .. } => {
                assert_eq!(worktree_slug, "feat-my-feature");
            }
            _ => panic!("expected Agent item"),
        }
    }

    #[test]
    fn global_status_slug_fallback_to_id_when_not_found() {
        let mut state = AppState::new();
        // No worktrees registered — slug_map is empty
        state.data.latest_agent_runs.insert(
            "unknown-wt-id".into(),
            make_agent_run("unknown-wt-id", AgentRunStatus::Running),
        );
        let gs = state.global_status();
        assert_eq!(gs.total_active(), 1);
        match &gs.active_items[0] {
            GlobalStatusItem::Agent { worktree_slug, .. } => {
                assert_eq!(worktree_slug, "unknown-wt-id");
            }
            _ => panic!("expected Agent item"),
        }
    }

    fn make_ticket(id: &str, state: &str) -> conductor_core::tickets::Ticket {
        conductor_core::tickets::Ticket {
            id: id.to_string(),
            repo_id: "repo-1".to_string(),
            source_type: "github".to_string(),
            source_id: id.to_string(),
            title: format!("Ticket {id}"),
            body: String::new(),
            state: state.to_string(),
            labels: String::new(),
            assignee: None,
            priority: None,
            url: String::new(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            raw_json: String::new(),
        }
    }

    #[test]
    fn rebuild_filtered_tickets_hides_closed() {
        let mut state = AppState::new();
        state.data.tickets = vec![
            make_ticket("1", "open"),
            make_ticket("2", "closed"),
            make_ticket("3", "open"),
        ];
        state.show_closed_tickets = false;
        state.rebuild_filtered_tickets();
        assert_eq!(state.filtered_tickets.len(), 2);
        assert!(state.filtered_tickets.iter().all(|t| t.state != "closed"));
    }

    #[test]
    fn rebuild_filtered_tickets_shows_closed_when_toggled() {
        let mut state = AppState::new();
        state.data.tickets = vec![
            make_ticket("1", "open"),
            make_ticket("2", "closed"),
            make_ticket("3", "open"),
        ];
        state.show_closed_tickets = true;
        state.rebuild_filtered_tickets();
        assert_eq!(state.filtered_tickets.len(), 3);
    }

    #[test]
    fn rebuild_filtered_tickets_applies_text_filter() {
        let mut state = AppState::new();
        state.data.tickets = vec![
            make_ticket("1", "open"),
            make_ticket("2", "open"),
            make_ticket("3", "open"),
        ];
        state.show_closed_tickets = true;
        state.filter.active = true;
        state.filter.text = "Ticket 2".to_lowercase();
        state.rebuild_filtered_tickets();
        // Only ticket whose title contains "ticket 2"
        assert_eq!(state.filtered_tickets.len(), 1);
        assert_eq!(state.filtered_tickets[0].id, "2");
    }

    #[test]
    fn rebuild_filtered_detail_tickets_independent_of_global() {
        let mut state = AppState::new();
        state.data.tickets = vec![make_ticket("1", "open"), make_ticket("2", "closed")];
        // detail_tickets has different content
        state.detail_tickets = vec![make_ticket("3", "open"), make_ticket("4", "closed")];
        state.show_closed_tickets = false;
        state.rebuild_filtered_tickets();
        assert_eq!(state.filtered_tickets.len(), 1);
        assert_eq!(state.filtered_detail_tickets.len(), 1);
        assert_eq!(state.filtered_tickets[0].id, "1");
        assert_eq!(state.filtered_detail_tickets[0].id, "3");
    }

    /// Regression: index into filtered list must match what's rendered.
    /// Given [#1 open, #2 closed, #3 open] with closed hidden, ticket_index=1
    /// should point to #3 (the 2nd visible item), not #2 (the 2nd raw item).
    #[test]
    fn filtered_tickets_index_matches_rendered_order() {
        let mut state = AppState::new();
        state.data.tickets = vec![
            make_ticket("1", "open"),
            make_ticket("2", "closed"),
            make_ticket("3", "open"),
            make_ticket("4", "open"),
        ];
        state.show_closed_tickets = false;
        state.rebuild_filtered_tickets();
        // filtered: [#1, #3, #4]
        assert_eq!(state.filtered_tickets.len(), 3);
        assert_eq!(state.filtered_tickets[0].id, "1");
        assert_eq!(state.filtered_tickets[1].id, "3");
        assert_eq!(state.filtered_tickets[2].id, "4");
        // ticket_index=2 now correctly resolves to #4
        assert_eq!(state.filtered_tickets[2].id, "4");
    }

    // --- status message auto-clear tests ---

    #[test]
    fn tick_status_message_clears_after_timeout() {
        let mut state = AppState::new();
        state.status_message = Some("hello".into());
        // Backdate the timestamp so it appears to have been set 5 seconds ago.
        state.status_message_at = Some(Instant::now() - Duration::from_secs(5));

        state.tick_status_message(Duration::from_secs(4));

        assert!(state.status_message.is_none());
        assert!(state.status_message_at.is_none());
    }

    #[test]
    fn tick_status_message_keeps_message_within_timeout() {
        let mut state = AppState::new();
        state.status_message = Some("hello".into());
        state.status_message_at = Some(Instant::now());

        state.tick_status_message(Duration::from_secs(4));

        assert!(state.status_message.is_some());
        assert!(state.status_message_at.is_some());
    }

    #[test]
    fn tick_status_message_no_op_when_none() {
        let mut state = AppState::new();
        // No message, no timestamp — should be a no-op.
        state.tick_status_message(Duration::from_secs(4));
        assert!(state.status_message.is_none());
        assert!(state.status_message_at.is_none());
    }

    /// Verifies that an `AgentRun` with `worktree_id = None` (ephemeral PR run)
    /// in the active runs map does not panic and produces a `GlobalStatusItem::Agent`
    /// with an "(ephemeral)" worktree slug.
    #[test]
    fn global_status_agent_with_none_worktree_id_uses_ephemeral_slug() {
        let mut state = AppState::new();
        let run = conductor_core::agent::AgentRun {
            id: "run-eph".into(),
            worktree_id: None,
            claude_session_id: None,
            prompt: "ephemeral".into(),
            status: AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
        };
        // Insert under an arbitrary key to exercise the global_status iteration path.
        state.data.latest_agent_runs.insert("run-eph".into(), run);
        let gs = state.global_status();
        assert_eq!(gs.running_agents, 1);
        assert_eq!(gs.active_items.len(), 1);
        match &gs.active_items[0] {
            GlobalStatusItem::Agent { worktree_slug, .. } => {
                assert_eq!(worktree_slug, "(ephemeral)");
            }
            _ => panic!("expected Agent item"),
        }
    }

    /// When a workflow and its spawned agent both exist for the same worktree,
    /// `global_status()` should produce exactly one item (Workflow) and
    /// `total_active()` should return 1.
    #[test]
    fn global_status_workflow_suppresses_agent_for_same_worktree() {
        let mut state = AppState::new();
        state
            .data
            .latest_agent_runs
            .insert("wt1".into(), make_agent_run("wt1", AgentRunStatus::Running));
        state.data.latest_workflow_runs_by_worktree.insert(
            "wt1".into(),
            make_workflow_run("wt1", WorkflowRunStatus::Running),
        );
        let gs = state.global_status();
        assert_eq!(
            gs.total_active(),
            1,
            "should count only one item after dedup"
        );
        assert_eq!(gs.active_items.len(), 1);
        assert!(
            matches!(
                &gs.active_items[0],
                GlobalStatusItem::Workflow {
                    status: WorkflowRunStatus::Running,
                    ..
                }
            ),
            "surviving item should be a Workflow variant"
        );
        // The agent counter should NOT have been incremented for the suppressed entry.
        assert_eq!(gs.running_agents, 0);
        assert_eq!(gs.running_workflows, 1);
    }

    #[test]
    fn global_status_non_worktree_workflow_repo_targeted() {
        let mut state = AppState::new();
        // Register a repo so repo_slug_map is populated.
        let repo = conductor_core::repo::Repo {
            id: "repo-1".into(),
            slug: "my-repo".into(),
            local_path: "/tmp/repo".into(),
            remote_url: String::new(),
            default_branch: "main".into(),
            workspace_dir: String::new(),
            created_at: "2026-01-01T00:00:00Z".into(),
            model: None,
            allow_agent_issue_creation: false,
        };
        state.data.repos.push(repo);
        state.data.rebuild_maps();

        let mut run = conductor_core::workflow::WorkflowRun {
            id: "wfrun-repo".into(),
            workflow_name: "test-workflow".into(),
            worktree_id: None,
            parent_run_id: "root".into(),
            status: WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: Some("repo-1".into()),
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
        };
        state
            .data
            .active_non_worktree_workflow_runs
            .push(run.clone());

        let gs = state.global_status();
        assert_eq!(gs.running_workflows, 1);
        assert_eq!(gs.total_active(), 1);
        match &gs.active_items[0] {
            GlobalStatusItem::Workflow { context_label, .. } => {
                assert_eq!(context_label, "my-repo");
            }
            _ => panic!("expected Workflow item"),
        }

        // Unknown repo_id falls back to "(ephemeral)".
        run.repo_id = Some("unknown-repo".into());
        state.data.active_non_worktree_workflow_runs[0] = run;
        let gs2 = state.global_status();
        match &gs2.active_items[0] {
            GlobalStatusItem::Workflow { context_label, .. } => {
                assert_eq!(context_label, "(ephemeral)");
            }
            _ => panic!("expected Workflow item"),
        }
    }

    #[test]
    fn global_status_non_worktree_workflow_ticket_targeted() {
        let mut state = AppState::new();
        // Register a ticket so ticket_map is populated.
        state.data.tickets.push(make_ticket("ticket-1", "open"));
        state.data.rebuild_maps();

        let mut run = conductor_core::workflow::WorkflowRun {
            id: "wfrun-ticket".into(),
            workflow_name: "test-workflow".into(),
            worktree_id: None,
            parent_run_id: "root".into(),
            status: WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: Some("ticket-1".into()),
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
        };
        state
            .data
            .active_non_worktree_workflow_runs
            .push(run.clone());

        let gs = state.global_status();
        assert_eq!(gs.running_workflows, 1);
        assert_eq!(gs.total_active(), 1);
        match &gs.active_items[0] {
            GlobalStatusItem::Workflow { context_label, .. } => {
                // make_ticket sets source_id = id = "ticket-1"
                assert_eq!(context_label, "#ticket-1");
            }
            _ => panic!("expected Workflow item"),
        }

        // Unknown ticket_id falls back to "(ephemeral)".
        run.ticket_id = Some("unknown-ticket".into());
        state.data.active_non_worktree_workflow_runs[0] = run;
        let gs2 = state.global_status();
        match &gs2.active_items[0] {
            GlobalStatusItem::Workflow { context_label, .. } => {
                assert_eq!(context_label, "(ephemeral)");
            }
            _ => panic!("expected Workflow item"),
        }
    }

    #[test]
    fn global_status_non_worktree_workflow_no_context_is_ephemeral() {
        let mut state = AppState::new();
        let run = conductor_core::workflow::WorkflowRun {
            id: "wfrun-eph".into(),
            workflow_name: "test-workflow".into(),
            worktree_id: None,
            parent_run_id: "root".into(),
            status: WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
        };
        state.data.active_non_worktree_workflow_runs.push(run);
        let gs = state.global_status();
        assert_eq!(gs.running_workflows, 1);
        match &gs.active_items[0] {
            GlobalStatusItem::Workflow { context_label, .. } => {
                assert_eq!(context_label, "(ephemeral)");
            }
            _ => panic!("expected Workflow item"),
        }
    }

    #[test]
    fn track_status_message_change_sets_timestamp_on_appear() {
        let mut state = AppState::new();
        // Simulate: no message before, message present now.
        state.status_message = Some("new".into());
        state.track_status_message_change(false);
        assert!(state.status_message_at.is_some());
    }

    #[test]
    fn track_status_message_change_clears_timestamp_on_disappear() {
        let mut state = AppState::new();
        state.status_message_at = Some(Instant::now());
        // Simulate: message was present before, gone now.
        state.status_message = None;
        state.track_status_message_change(true);
        assert!(state.status_message_at.is_none());
    }

    #[test]
    fn track_status_message_change_no_op_when_message_persists() {
        let mut state = AppState::new();
        let before = Instant::now() - Duration::from_secs(2);
        state.status_message = Some("persisting".into());
        state.status_message_at = Some(before);
        // Simulate: had message before, still has message now.
        state.track_status_message_change(true);
        // Timestamp should not be reset.
        assert!(state.status_message_at.unwrap() <= before + Duration::from_millis(1));
    }

    // --- visible_workflow_run_rows tests ---

    fn make_wf_run_full(
        id: &str,
        status: WorkflowRunStatus,
        parent_workflow_run_id: Option<&str>,
    ) -> conductor_core::workflow::WorkflowRun {
        conductor_core::workflow::WorkflowRun {
            id: id.into(),
            workflow_name: "test-workflow".into(),
            worktree_id: None,
            parent_run_id: "run-1".into(),
            status,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: parent_workflow_run_id.map(|s| s.into()),
            target_label: None,
            default_bot_name: None,
        }
    }

    #[test]
    fn visible_workflow_run_rows_empty() {
        let state = AppState::new();
        assert!(state.visible_workflow_run_rows().is_empty());
    }

    #[test]
    fn visible_workflow_run_rows_single_parent_no_children() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 1);
        assert!(
            matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 0, collapsed: false } if run_id == "p1")
        );
    }

    #[test]
    fn visible_workflow_run_rows_parent_with_child_expanded() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        ];
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 2);
        assert!(
            matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 1, collapsed: false } if run_id == "p1")
        );
        assert!(matches!(&rows[1], WorkflowRunRow::Child { run_id, .. } if run_id == "c1"));
    }

    #[test]
    fn visible_workflow_run_rows_parent_with_child_collapsed() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        ];
        state.collapsed_workflow_run_ids.insert("p1".into());
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 1);
        assert!(
            matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 1, collapsed: true } if run_id == "p1")
        );
    }

    #[test]
    fn visible_workflow_run_rows_orphaned_child_treated_as_root() {
        let mut state = AppState::new();
        // c1 references a parent not in the list — should appear as a root
        state.data.workflow_runs = vec![make_wf_run_full(
            "c1",
            WorkflowRunStatus::Running,
            Some("nonexistent"),
        )];
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 1);
        assert!(
            matches!(&rows[0], WorkflowRunRow::Parent { run_id, child_count: 0, .. } if run_id == "c1")
        );
    }

    // --- init_collapse_state tests ---

    #[test]
    fn init_collapse_state_running_not_collapsed() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
        state.init_collapse_state();
        assert!(!state.collapsed_workflow_run_ids.contains("p1"));
        assert!(state.collapse_initialized.contains("p1"));
    }

    #[test]
    fn init_collapse_state_terminal_statuses_collapsed() {
        for status in [
            WorkflowRunStatus::Completed,
            WorkflowRunStatus::Failed,
            WorkflowRunStatus::Cancelled,
        ] {
            let mut state = AppState::new();
            state.data.workflow_runs = vec![make_wf_run_full("p1", status.clone(), None)];
            state.init_collapse_state();
            assert!(
                state.collapsed_workflow_run_ids.contains("p1"),
                "expected p1 collapsed for {status:?}"
            );
        }
    }

    #[test]
    fn init_collapse_state_idempotent() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
        state.init_collapse_state();
        assert!(state.collapsed_workflow_run_ids.contains("p1"));
        // Manually expand — second call must not re-collapse since already initialized
        state.collapsed_workflow_run_ids.remove("p1");
        state.init_collapse_state();
        assert!(
            !state.collapsed_workflow_run_ids.contains("p1"),
            "second init_collapse_state call must not re-collapse an already-initialized run"
        );
    }

    #[test]
    fn init_collapse_state_child_runs_not_collapsed() {
        let mut state = AppState::new();
        // A child run with terminal status should not be auto-collapsed
        state.data.workflow_runs = vec![make_wf_run_full(
            "c1",
            WorkflowRunStatus::Completed,
            Some("p1"),
        )];
        state.init_collapse_state();
        assert!(!state.collapsed_workflow_run_ids.contains("c1"));
    }
}
