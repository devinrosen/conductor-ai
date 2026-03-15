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
    AgentCreatedIssue, AgentRun, AgentRunEvent, FeedbackRequest, TicketAgentTotals,
};
use conductor_core::github::{DiscoveredRepo, GithubPr};
use conductor_core::issue_source::IssueSource;
use conductor_core::repo::Repo;
use conductor_core::tickets::{Ticket, TicketLabel};
use conductor_core::workflow::{
    WorkflowDef, WorkflowRun, WorkflowRunStatus, WorkflowRunStep, WorkflowStepSummary,
};

use crate::theme::Theme;

use conductor_core::worktree::Worktree;
use ratatui::widgets::ListState;
use tui_textarea::TextArea;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    RepoDetail,
    WorktreeDetail,
    WorkflowRunDetail,
    WorkflowDefDetail,
}

/// A row in the unified dashboard list — either a repo header or a worktree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardRow {
    /// Index into `AppState::data.repos`.
    Repo(usize),
    /// Index into `AppState::data.worktrees`.
    Worktree(usize),
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
            Self::Worktrees => Self::Prs,
            Self::Prs => Self::Tickets,
            Self::Tickets => Self::Info,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Info => Self::Tickets,
            Self::Worktrees => Self::Info,
            Self::Prs => Self::Worktrees,
            Self::Tickets => Self::Prs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowsFocus {
    Defs,
    Runs,
}

/// Whether a target header row represents a worktree or a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetType {
    Worktree,
    Pr,
}

/// A row in the visible workflow runs list.
/// Either a group header, a root/parent run, or an indented child run.
#[derive(Debug, Clone)]
pub enum WorkflowRunRow {
    /// Top-level repo group header (global mode only).
    RepoHeader {
        repo_slug: String,
        collapsed: bool,
        run_count: usize,
    },
    /// Second-level target header (worktree or PR) within a repo group (global mode only).
    TargetHeader {
        /// Composite key `"repo_slug/target_key"` used as the collapse-state key.
        target_key: String,
        /// Human-readable label shown in the row.
        label: String,
        target_type: TargetType,
        collapsed: bool,
        run_count: usize,
    },
    Parent {
        run_id: String,
        collapsed: bool,
        child_count: usize,
    },
    Child {
        run_id: String,
        #[allow(dead_code)]
        parent_id: String,
        /// 1 = direct child of root, 2 = grandchild, etc.
        depth: u8,
        /// Current expand/collapse state of THIS node.
        collapsed: bool,
        /// Number of direct children (0 = leaf).
        child_count: usize,
    },
    /// An individual step of a leaf run, shown when the user expands the run.
    Step {
        /// The run that owns this step.
        #[allow(dead_code)]
        run_id: String,
        #[allow(dead_code)]
        step_id: String,
        step_name: String,
        /// Raw status string (e.g. "completed", "running").
        status: String,
        position: i64,
        /// Indentation level = owning run depth + 1.
        depth: u8,
        /// Role of the step (e.g. "actor", "gate", "reviewer").
        role: String,
        /// Parallel group this step belongs to, if any.
        #[allow(dead_code)]
        parallel_group_id: Option<String>,
    },
    /// A synthetic header row grouping parallel steps sharing the same `parallel_group_id`.
    ParallelGroup {
        #[allow(dead_code)]
        group_id: String,
        /// Derived from member statuses: running > waiting > failed > completed > skipped > pending.
        status: String,
        /// Number of steps in this group.
        count: usize,
        depth: u8,
    },
}

impl WorkflowRunRow {
    /// Returns the run ID for `Parent`/`Child` rows; `None` for header/step rows.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            WorkflowRunRow::Parent { run_id, .. } => Some(run_id),
            WorkflowRunRow::Child { run_id, .. } => Some(run_id),
            WorkflowRunRow::RepoHeader { .. }
            | WorkflowRunRow::TargetHeader { .. }
            | WorkflowRunRow::Step { .. }
            | WorkflowRunRow::ParallelGroup { .. } => None,
        }
    }
}

/// Parse a `target_label` string into `(repo_slug, target_key, TargetType)`.
///
/// Two formats exist:
/// - Worktree: `"repo_slug/wt_slug"` → `(repo_slug, wt_slug, Worktree)`
/// - PR: `"owner/repo#N"` → `("unknown", label, Pr)` — caller should fall back to repo_id lookup
/// - No slash: `("unknown", label, Worktree)`
pub fn parse_target_label(label: &str) -> (String, String, TargetType) {
    if label.contains('#') {
        // PR format: "owner/repo#N" — we cannot derive the conductor repo slug from the label.
        return ("unknown".to_string(), label.to_string(), TargetType::Pr);
    }
    if let Some(slash_pos) = label.find('/') {
        let repo_slug = label[..slash_pos].to_string();
        let target_key = label[slash_pos + 1..].to_string();
        return (repo_slug, target_key, TargetType::Worktree);
    }
    (
        "unknown".to_string(),
        label.to_string(),
        TargetType::Worktree,
    )
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

/// Focus state for the workflow column when viewing workflow definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkflowDefFocus {
    /// Definition list has focus; right column shows workflow runs.
    #[default]
    List,
    /// Step tree has focus; right column shows the step tree for the selected def.
    Steps,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnFocus {
    #[default]
    Content, // left column (repos, worktrees, detail panels)
    Workflow, // right persistent workflow column
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
        /// Snapshot of all available themes at picker-open time (built-ins + custom).
        /// Stored here so list changes mid-open don't cause index mismatches.
        themes: Vec<(String, String)>,
        /// Pre-loaded `Theme` objects corresponding 1-to-1 with `themes`.
        /// Enables O(1) in-memory preview on keypress with no file I/O.
        loaded_themes: Vec<crate::theme::Theme>,
        /// Index into `themes`.
        selected: usize,
        /// Theme active when the picker was opened; restored on Esc.
        original_theme: crate::theme::Theme,
        /// Name of the theme active when the picker was opened (from config),
        /// used to restore the correct highlighted row if the picker is
        /// re-opened after an Esc cancel.
        original_name: String,
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
            Modal::ThemePicker {
                selected,
                ref original_name,
                ..
            } => {
                write!(
                    f,
                    "Modal::ThemePicker(selected={selected}, original={original_name:?})"
                )
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
    UnregisterRepo {
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
    RegisterRepo,
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
    /// Steps for every leaf run in the current scope (run_id → ordered steps).
    /// Populated by the background poller on every tick.
    pub workflow_run_steps: HashMap<String, Vec<WorkflowRunStep>>,
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
    /// The view the user was in before entering WorkflowRunDetail (for back navigation).
    pub previous_view: Option<View>,
    /// The `selected_worktree_id` that was active before entering WorkflowRunDetail.
    pub previous_selected_worktree_id: Option<Option<String>>,
    pub repo_detail_focus: RepoDetailFocus,
    pub modal: Modal,
    pub data: DataCache,

    // Selection indices
    pub dashboard_index: usize,
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
    /// Set of repo slugs whose group header is collapsed in global mode.
    pub collapsed_repo_headers: HashSet<String>,
    /// Set of composite `"repo_slug/target_key"` strings whose target header is collapsed in global mode.
    pub collapsed_target_headers: HashSet<String>,
    /// Tracks which run IDs have had their default collapse state initialized.
    collapse_initialized: HashSet<String>,
    /// Set of leaf run IDs whose steps are currently expanded inline.
    pub expanded_step_run_ids: HashSet<String>,

    pub should_quit: bool,

    /// When false (default), closed tickets are hidden in all ticket views.
    pub show_closed_tickets: bool,

    /// When false (default), completed and cancelled workflow runs are hidden in the workflow column.
    pub show_completed_workflow_runs: bool,

    /// Semantic colour theme — centralises all Color constants used by the UI.
    pub theme: Theme,

    /// True while a manual ticket sync is running in the background.
    pub ticket_sync_in_progress: bool,

    /// Which column currently has keyboard focus: Content (left) or Workflow (right).
    pub column_focus: ColumnFocus,

    /// When true, show the persistent workflow column on the right side.
    pub workflow_column_visible: bool,

    /// Cached home directory path for `~` substitution in path display. Never changes.
    pub home_dir: Option<String>,

    /// The workflow definition currently being viewed in WorkflowDefDetail.
    pub selected_workflow_def: Option<conductor_core::workflow::WorkflowDef>,
    /// Vertical scroll offset for the steps pane in WorkflowDefDetail.
    pub workflow_def_detail_scroll: usize,

    /// Which sub-pane of the workflow column Defs view has focus.
    pub workflow_def_focus: WorkflowDefFocus,
    /// Selected row index in the step tree pane (when `workflow_def_focus == Steps`).
    pub workflow_def_step_index: usize,
    /// Set of dot-path strings identifying expanded `CallWorkflow` nodes in the step tree.
    /// Cleared whenever `workflow_def_index` changes.
    pub workflow_def_expanded_calls: HashSet<String>,
}

/// Append step rows for `run_id` when it is in `expanded_step_run_ids`.
fn push_steps_for_run(
    run_id: &str,
    depth: u8,
    rows: &mut Vec<WorkflowRunRow>,
    expanded_step_run_ids: &std::collections::HashSet<String>,
    workflow_run_steps: &std::collections::HashMap<
        String,
        Vec<conductor_core::workflow::WorkflowRunStep>,
    >,
) {
    if !expanded_step_run_ids.contains(run_id) {
        return;
    }
    if let Some(steps) = workflow_run_steps.get(run_id) {
        let mut ordered: Vec<_> = steps.iter().collect();
        ordered.sort_by_key(|s| s.position);
        let mut seen_groups: std::collections::HashSet<String> = std::collections::HashSet::new();
        for step in &ordered {
            match &step.parallel_group_id {
                None => {
                    rows.push(WorkflowRunRow::Step {
                        run_id: run_id.to_string(),
                        step_id: step.id.clone(),
                        step_name: step.step_name.clone(),
                        status: step.status.to_string(),
                        position: step.position,
                        depth,
                        role: step.role.clone(),
                        parallel_group_id: None,
                    });
                }
                Some(gid) => {
                    if seen_groups.contains(gid) {
                        // Already emitted this group's header and members.
                        continue;
                    }
                    seen_groups.insert(gid.clone());
                    // Collect all members of this group.
                    let members: Vec<_> = ordered
                        .iter()
                        .filter(|s| s.parallel_group_id.as_deref() == Some(gid))
                        .collect();
                    let group_status = derive_parallel_group_status(&members);
                    rows.push(WorkflowRunRow::ParallelGroup {
                        group_id: gid.clone(),
                        status: group_status,
                        count: members.len(),
                        depth,
                    });
                    for member in members {
                        rows.push(WorkflowRunRow::Step {
                            run_id: run_id.to_string(),
                            step_id: member.id.clone(),
                            step_name: member.step_name.clone(),
                            status: member.status.to_string(),
                            position: member.position,
                            depth: depth + 1,
                            role: member.role.clone(),
                            parallel_group_id: member.parallel_group_id.clone(),
                        });
                    }
                }
            }
        }
    }
}

/// Derive a single aggregate status for a parallel group from its members.
/// Priority: running > waiting > failed > completed > skipped > pending.
fn derive_parallel_group_status(members: &[&&conductor_core::workflow::WorkflowRunStep]) -> String {
    let statuses: Vec<String> = members.iter().map(|s| s.status.to_string()).collect();
    for s in &[
        "running",
        "waiting",
        "failed",
        "completed",
        "skipped",
        "pending",
    ] {
        if statuses.iter().any(|st| st == s) {
            return s.to_string();
        }
    }
    "pending".to_string()
}

/// Recursively append `Child` rows for `parent_id` into `rows`.
/// `depth` starts at 1 for direct children of a root run.
fn push_children(
    parent_id: &str,
    depth: u8,
    rows: &mut Vec<WorkflowRunRow>,
    children_map: &std::collections::HashMap<&str, Vec<&conductor_core::workflow::WorkflowRun>>,
    collapsed_ids: &std::collections::HashSet<String>,
    expanded_step_run_ids: &std::collections::HashSet<String>,
    workflow_run_steps: &std::collections::HashMap<
        String,
        Vec<conductor_core::workflow::WorkflowRunStep>,
    >,
) {
    let Some(children) = children_map.get(parent_id) else {
        return;
    };
    for child in children {
        let child_count = children_map.get(child.id.as_str()).map_or(0, |v| v.len());
        let collapsed = collapsed_ids.contains(&child.id);
        rows.push(WorkflowRunRow::Child {
            run_id: child.id.clone(),
            parent_id: parent_id.to_string(),
            depth,
            collapsed,
            child_count,
        });
        if !collapsed {
            if child_count == 0 {
                push_steps_for_run(
                    &child.id,
                    depth + 1,
                    rows,
                    expanded_step_run_ids,
                    workflow_run_steps,
                );
            } else {
                push_children(
                    &child.id,
                    depth + 1,
                    rows,
                    children_map,
                    collapsed_ids,
                    expanded_step_run_ids,
                    workflow_run_steps,
                );
            }
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            view: View::Dashboard,
            previous_view: None,
            previous_selected_worktree_id: None,
            repo_detail_focus: RepoDetailFocus::Worktrees,
            modal: Modal::None,
            data: DataCache::default(),
            dashboard_index: 0,
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
            collapsed_repo_headers: HashSet::new(),
            collapsed_target_headers: HashSet::new(),
            collapse_initialized: HashSet::new(),
            expanded_step_run_ids: HashSet::new(),
            should_quit: false,
            show_closed_tickets: false,
            show_completed_workflow_runs: false,
            ticket_sync_in_progress: false,
            column_focus: ColumnFocus::Content,
            workflow_column_visible: true,
            home_dir: dirs::home_dir().map(|p| p.to_string_lossy().into_owned()),
            theme: Theme::default(),
            selected_workflow_def: None,
            workflow_def_detail_scroll: 0,
            workflow_def_focus: WorkflowDefFocus::List,
            workflow_def_step_index: 0,
            workflow_def_expanded_calls: HashSet::new(),
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
        // When workflow column is focused, navigate within workflow panes.
        if self.column_focus == ColumnFocus::Workflow {
            return match self.workflows_focus {
                WorkflowsFocus::Defs => (self.workflow_def_index, self.data.workflow_defs.len()),
                WorkflowsFocus::Runs => (
                    self.workflow_run_index,
                    self.visible_workflow_run_rows().len(),
                ),
            };
        }
        match self.view {
            View::Dashboard => (self.dashboard_index, self.dashboard_rows().len()),
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
            View::WorkflowRunDetail => match self.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Steps => {
                    (self.workflow_step_index, self.data.workflow_steps.len())
                }
                WorkflowRunDetailFocus::AgentActivity => (
                    self.step_agent_event_index,
                    self.data.step_agent_events.len(),
                ),
            },
            View::WorkflowDefDetail => (self.workflow_def_detail_scroll, 0),
        }
    }

    /// Sets the index for the currently focused pane.
    pub fn set_focused_index(&mut self, index: usize) {
        // When workflow column is focused, update workflow pane indices.
        if self.column_focus == ColumnFocus::Workflow {
            match self.workflows_focus {
                WorkflowsFocus::Defs => self.workflow_def_index = index,
                WorkflowsFocus::Runs => self.workflow_run_index = index,
            }
            return;
        }
        match self.view {
            View::Dashboard => self.dashboard_index = index,
            View::RepoDetail => match self.repo_detail_focus {
                RepoDetailFocus::Info => self.repo_detail_info_row = index,
                RepoDetailFocus::Worktrees => self.detail_wt_index = index,
                RepoDetailFocus::Tickets => self.detail_ticket_index = index,
                RepoDetailFocus::Prs => self.detail_pr_index = index,
            },
            View::WorktreeDetail => {
                self.agent_list_state.borrow_mut().select(Some(index));
            }
            View::WorkflowRunDetail => match self.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Steps => self.workflow_step_index = index,
                WorkflowRunDetailFocus::AgentActivity => self.step_agent_event_index = index,
            },
            View::WorkflowDefDetail => {
                self.workflow_def_detail_scroll = index;
            }
        }
    }

    /// Returns the flat, ordered list of visible workflow run rows.
    /// Roots appear first; their expanded children follow immediately after.
    /// Runs returned DESC by the DB (newest first); children are sorted ASC (oldest first).
    ///
    /// In global mode (no worktree selected), runs are grouped by repo → target with
    /// collapsible `RepoHeader` and `TargetHeader` rows prepended to each group.
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

        // Global mode: no worktree and no repo selected (Dashboard / all-repos view).
        // Repo-scoped and worktree-scoped modes both use the flat list.
        let global_mode = self.selected_worktree_id.is_none() && self.selected_repo_id.is_none();

        if !global_mode {
            // Non-global mode: flat list, optionally hiding completed/cancelled root runs.
            let mut result = Vec::new();
            for run in runs {
                if child_ids.contains(run.id.as_str()) {
                    continue;
                }
                if !self.show_completed_workflow_runs
                    && matches!(
                        run.status,
                        WorkflowRunStatus::Completed | WorkflowRunStatus::Cancelled
                    )
                {
                    continue;
                }
                let child_count = children_map.get(run.id.as_str()).map_or(0, |v| v.len());
                let collapsed = self.collapsed_workflow_run_ids.contains(&run.id);
                result.push(WorkflowRunRow::Parent {
                    run_id: run.id.clone(),
                    collapsed,
                    child_count,
                });
                if !collapsed {
                    if child_count == 0 {
                        push_steps_for_run(
                            &run.id,
                            1,
                            &mut result,
                            &self.expanded_step_run_ids,
                            &self.data.workflow_run_steps,
                        );
                    } else {
                        push_children(
                            &run.id,
                            1,
                            &mut result,
                            &children_map,
                            &self.collapsed_workflow_run_ids,
                            &self.expanded_step_run_ids,
                            &self.data.workflow_run_steps,
                        );
                    }
                }
            }
            return result;
        }

        // Global mode: group root runs by (repo_slug, target_key).
        let repo_slug_map: HashMap<&str, &str> = self
            .data
            .repos
            .iter()
            .map(|r| (r.id.as_str(), r.slug.as_str()))
            .collect();

        // Collect (repo_slug, target_key, target_type, run) for every root run,
        // preserving the DB order (newest first).
        let mut groups: Vec<(String, String, TargetType, &WorkflowRun)> = Vec::new();
        for run in runs {
            if child_ids.contains(run.id.as_str()) {
                continue;
            }
            if !self.show_completed_workflow_runs
                && matches!(
                    run.status,
                    WorkflowRunStatus::Completed | WorkflowRunStatus::Cancelled
                )
            {
                continue;
            }
            let (mut repo_slug, target_key, target_type) = run
                .target_label
                .as_deref()
                .map(parse_target_label)
                .unwrap_or_else(|| ("unknown".to_string(), String::new(), TargetType::Worktree));

            // For PR runs (or any run where repo_slug could not be parsed from label),
            // fall back to repo_id lookup.
            if repo_slug == "unknown" {
                if let Some(rid) = run.repo_id.as_deref() {
                    if let Some(&slug) = repo_slug_map.get(rid) {
                        repo_slug = slug.to_string();
                    }
                }
            }

            groups.push((repo_slug, target_key, target_type, run));
        }

        // Determine ordered list of repos and, within each repo, ordered targets.
        let mut seen_repos: HashSet<String> = HashSet::new();
        let mut repo_order: Vec<String> = Vec::new();
        let mut seen_targets: HashSet<String> = HashSet::new(); // "repo/target_key"
                                                                // target_order[repo_slug] = Vec<(target_key, TargetType)>
        let mut target_order: HashMap<String, Vec<(String, TargetType)>> = HashMap::new();

        for (repo_slug, target_key, target_type, _) in &groups {
            if seen_repos.insert(repo_slug.clone()) {
                repo_order.push(repo_slug.clone());
            }
            let composite = format!("{}/{}", repo_slug, target_key);
            if seen_targets.insert(composite) {
                target_order
                    .entry(repo_slug.clone())
                    .or_default()
                    .push((target_key.clone(), target_type.clone()));
            }
        }

        // Build the final visible row list.
        let mut result = Vec::new();
        for repo_slug in &repo_order {
            let run_count = groups
                .iter()
                .filter(|(rs, _, _, _)| rs == repo_slug)
                .count();
            let repo_collapsed = self.collapsed_repo_headers.contains(repo_slug.as_str());

            result.push(WorkflowRunRow::RepoHeader {
                repo_slug: repo_slug.clone(),
                collapsed: repo_collapsed,
                run_count,
            });

            if repo_collapsed {
                continue;
            }

            let repo_targets = target_order
                .get(repo_slug)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);

            for (target_key, target_type) in repo_targets {
                let composite_key = format!("{}/{}", repo_slug, target_key);
                let target_run_count = groups
                    .iter()
                    .filter(|(rs, tk, _, _)| rs == repo_slug && tk == target_key)
                    .count();
                let target_collapsed = self.collapsed_target_headers.contains(&composite_key);

                let label = if target_key.is_empty() {
                    repo_slug.clone()
                } else {
                    target_key.clone()
                };

                result.push(WorkflowRunRow::TargetHeader {
                    target_key: composite_key.clone(),
                    label,
                    target_type: target_type.clone(),
                    collapsed: target_collapsed,
                    run_count: target_run_count,
                });

                if target_collapsed {
                    continue;
                }

                for (rs, tk, _, run) in &groups {
                    if rs != repo_slug || tk != target_key {
                        continue;
                    }
                    let child_count = children_map.get(run.id.as_str()).map_or(0, |v| v.len());
                    let collapsed = self.collapsed_workflow_run_ids.contains(&run.id);
                    result.push(WorkflowRunRow::Parent {
                        run_id: run.id.clone(),
                        collapsed,
                        child_count,
                    });
                    if !collapsed {
                        if child_count == 0 {
                            push_steps_for_run(
                                &run.id,
                                1,
                                &mut result,
                                &self.expanded_step_run_ids,
                                &self.data.workflow_run_steps,
                            );
                        } else {
                            push_children(
                                &run.id,
                                1,
                                &mut result,
                                &children_map,
                                &self.collapsed_workflow_run_ids,
                                &self.expanded_step_run_ids,
                                &self.data.workflow_run_steps,
                            );
                        }
                    }
                }
            }
        }
        result
    }

    /// Auto-initialize collapse state for newly-seen terminal-status parent runs.
    /// Count root-level workflow runs that are hidden because `show_completed_workflow_runs` is false.
    pub fn hidden_workflow_run_count(&self) -> usize {
        if self.show_completed_workflow_runs {
            return 0;
        }
        let known_ids: HashSet<&str> = self
            .data
            .workflow_runs
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        let child_ids: HashSet<&str> = self
            .data
            .workflow_runs
            .iter()
            .filter_map(|r| r.parent_workflow_run_id.as_deref())
            .filter(|pid| known_ids.contains(pid))
            .collect();
        self.data
            .workflow_runs
            .iter()
            .filter(|r| !child_ids.contains(r.id.as_str()))
            .filter(|r| {
                matches!(
                    r.status,
                    WorkflowRunStatus::Completed | WorkflowRunStatus::Cancelled
                )
            })
            .count()
    }

    /// Call this after updating `self.data.workflow_runs`.
    /// Terminal runs (completed/failed/cancelled) are collapsed on first appearance.
    /// Running leaf runs (no children) are auto-expanded to show steps.
    pub fn init_collapse_state(&mut self) {
        let parent_ids: std::collections::HashSet<&str> = self
            .data
            .workflow_runs
            .iter()
            .filter_map(|r| r.parent_workflow_run_id.as_deref())
            .collect();

        for run in &self.data.workflow_runs {
            if self.collapse_initialized.contains(&run.id) {
                continue;
            }
            let is_leaf = !parent_ids.contains(run.id.as_str());

            // Root-run only: collapse terminal runs so the list stays tidy
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

            // All depths: auto-expand running leaf runs to show their steps
            if matches!(run.status, WorkflowRunStatus::Running) && is_leaf {
                self.expanded_step_run_ids.insert(run.id.clone());
            }

            self.collapse_initialized.insert(run.id.clone());
        }
    }

    /// Ordered list of rows for the unified dashboard panel.
    /// Each repo appears first, followed immediately by its worktrees.
    pub fn dashboard_rows(&self) -> Vec<DashboardRow> {
        let mut rows = Vec::new();
        for (repo_idx, repo) in self.data.repos.iter().enumerate() {
            rows.push(DashboardRow::Repo(repo_idx));
            for (wt_idx, wt) in self.data.worktrees.iter().enumerate() {
                if wt.repo_id == repo.id {
                    rows.push(DashboardRow::Worktree(wt_idx));
                }
            }
        }
        rows
    }

    /// Get the currently selected repo, if any.
    /// When the cursor is on a worktree row, returns that worktree's owning repo.
    pub fn selected_repo(&self) -> Option<&Repo> {
        match self.dashboard_rows().get(self.dashboard_index)? {
            DashboardRow::Repo(idx) => self.data.repos.get(*idx),
            DashboardRow::Worktree(idx) => {
                let wt = self.data.worktrees.get(*idx)?;
                self.data.repos.iter().find(|r| r.id == wt.repo_id)
            }
        }
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
        assert_eq!(RepoDetailFocus::Worktrees.next(), RepoDetailFocus::Prs);
        assert_eq!(RepoDetailFocus::Prs.next(), RepoDetailFocus::Tickets);
        assert_eq!(RepoDetailFocus::Tickets.next(), RepoDetailFocus::Info);
    }

    #[test]
    fn repo_detail_focus_prev_cycles_backward() {
        assert_eq!(RepoDetailFocus::Info.prev(), RepoDetailFocus::Tickets);
        assert_eq!(RepoDetailFocus::Worktrees.prev(), RepoDetailFocus::Info);
        assert_eq!(RepoDetailFocus::Prs.prev(), RepoDetailFocus::Worktrees);
        assert_eq!(RepoDetailFocus::Tickets.prev(), RepoDetailFocus::Prs);
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

    /// Helper: put state into single-worktree (non-global) mode.
    fn set_worktree_mode(state: &mut AppState) {
        state.selected_worktree_id = Some("wt-id".into());
    }

    #[test]
    fn visible_workflow_run_rows_empty() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        assert!(state.visible_workflow_run_rows().is_empty());
    }

    #[test]
    fn visible_workflow_run_rows_single_parent_no_children() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
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
        set_worktree_mode(&mut state);
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
        set_worktree_mode(&mut state);
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
        set_worktree_mode(&mut state);
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

    // --- global mode grouping tests ---

    fn make_wf_run_with_label(
        id: &str,
        target_label: Option<&str>,
        repo_id: Option<&str>,
    ) -> conductor_core::workflow::WorkflowRun {
        conductor_core::workflow::WorkflowRun {
            id: id.into(),
            workflow_name: "test-workflow".into(),
            worktree_id: None,
            parent_run_id: "run-1".into(),
            status: WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: None,
            result_summary: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: repo_id.map(|s| s.into()),
            parent_workflow_run_id: None,
            target_label: target_label.map(|s| s.into()),
            default_bot_name: None,
        }
    }

    #[test]
    fn parse_target_label_worktree_format() {
        let (repo, key, ty) = parse_target_label("my-repo/feat-123");
        assert_eq!(repo, "my-repo");
        assert_eq!(key, "feat-123");
        assert_eq!(ty, TargetType::Worktree);
    }

    #[test]
    fn parse_target_label_pr_format() {
        let (repo, key, ty) = parse_target_label("owner/repo#42");
        assert_eq!(repo, "unknown");
        assert_eq!(key, "owner/repo#42");
        assert_eq!(ty, TargetType::Pr);
    }

    #[test]
    fn parse_target_label_no_slash() {
        let (repo, key, ty) = parse_target_label("standalone");
        assert_eq!(repo, "unknown");
        assert_eq!(key, "standalone");
        assert_eq!(ty, TargetType::Worktree);
    }

    #[test]
    fn global_mode_groups_by_repo_then_target() {
        // Two worktree runs for the same repo, one for another repo.
        let mut state = AppState::new(); // global mode (no selected_worktree_id)
        state.data.workflow_runs = vec![
            make_wf_run_with_label("r1", Some("repo-a/feat-1"), None),
            make_wf_run_with_label("r2", Some("repo-a/feat-2"), None),
            make_wf_run_with_label("r3", Some("repo-b/feat-3"), None),
        ];
        let rows = state.visible_workflow_run_rows();

        // Expected structure (8 rows total):
        // RepoHeader(repo-a), TargetHeader(feat-1), Parent(r1),
        //                     TargetHeader(feat-2), Parent(r2),
        // RepoHeader(repo-b), TargetHeader(feat-3), Parent(r3)
        assert_eq!(rows.len(), 8);
        assert!(
            matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "repo-a")
        );
        assert!(
            matches!(&rows[1], WorkflowRunRow::TargetHeader { label, .. } if label == "feat-1")
        );
        assert!(matches!(&rows[2], WorkflowRunRow::Parent { run_id, .. } if run_id == "r1"));
        assert!(
            matches!(&rows[3], WorkflowRunRow::TargetHeader { label, .. } if label == "feat-2")
        );
        assert!(matches!(&rows[4], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
        assert!(
            matches!(&rows[5], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "repo-b")
        );
        assert!(
            matches!(&rows[6], WorkflowRunRow::TargetHeader { label, .. } if label == "feat-3")
        );
        assert!(matches!(&rows[7], WorkflowRunRow::Parent { run_id, .. } if run_id == "r3"));
    }

    #[test]
    fn global_mode_collapsed_repo_hides_children() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![
            make_wf_run_with_label("r1", Some("repo-a/feat-1"), None),
            make_wf_run_with_label("r2", Some("repo-b/feat-2"), None),
        ];
        state.collapsed_repo_headers.insert("repo-a".into());
        let rows = state.visible_workflow_run_rows();
        // repo-a collapsed → only header, repo-b expanded → header + target + run
        assert_eq!(rows.len(), 4);
        assert!(
            matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, collapsed: true, .. } if repo_slug == "repo-a")
        );
        assert!(
            matches!(&rows[1], WorkflowRunRow::RepoHeader { repo_slug, collapsed: false, .. } if repo_slug == "repo-b")
        );
    }

    #[test]
    fn global_mode_collapsed_target_hides_runs() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![
            make_wf_run_with_label("r1", Some("repo-a/feat-1"), None),
            make_wf_run_with_label("r2", Some("repo-a/feat-2"), None),
        ];
        state
            .collapsed_target_headers
            .insert("repo-a/feat-1".into());
        let rows = state.visible_workflow_run_rows();
        // RepoHeader, TargetHeader(feat-1 collapsed), TargetHeader(feat-2), Parent(r2)
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[0], WorkflowRunRow::RepoHeader { .. }));
        assert!(
            matches!(&rows[1], WorkflowRunRow::TargetHeader { label, collapsed: true, .. } if label == "feat-1")
        );
        assert!(
            matches!(&rows[2], WorkflowRunRow::TargetHeader { label, collapsed: false, .. } if label == "feat-2")
        );
        assert!(matches!(&rows[3], WorkflowRunRow::Parent { run_id, .. } if run_id == "r2"));
    }

    #[test]
    fn global_mode_pr_run_uses_repo_id_fallback() {
        use conductor_core::repo::Repo;
        let mut state = AppState::new();
        state.data.repos = vec![Repo {
            id: "repo-id-1".into(),
            slug: "my-repo".into(),
            remote_url: String::new(),
            local_path: String::new(),
            default_branch: String::new(),
            workspace_dir: String::new(),
            created_at: String::new(),
            model: None,
            allow_agent_issue_creation: false,
        }];
        state.data.workflow_runs = vec![make_wf_run_with_label(
            "pr1",
            Some("owner/repo#99"),
            Some("repo-id-1"),
        )];
        let rows = state.visible_workflow_run_rows();
        // RepoHeader should show "my-repo" (from repo_id lookup, not "unknown")
        assert!(
            matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "my-repo")
        );
    }

    #[test]
    fn global_mode_run_without_label_buckets_under_unknown() {
        let mut state = AppState::new();
        state.data.workflow_runs = vec![make_wf_run_with_label("r1", None, None)];
        let rows = state.visible_workflow_run_rows();
        assert!(
            matches!(&rows[0], WorkflowRunRow::RepoHeader { repo_slug, .. } if repo_slug == "unknown")
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

    #[test]
    fn init_collapse_state_running_leaf_auto_expanded() {
        let mut state = AppState::new();
        // A running root run with no children is a leaf — it should land in expanded_step_run_ids
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Running, None)];
        state.init_collapse_state();
        assert!(
            state.expanded_step_run_ids.contains("p1"),
            "running leaf run must be auto-expanded into expanded_step_run_ids"
        );
        assert!(!state.collapsed_workflow_run_ids.contains("p1"));
    }

    #[test]
    fn init_collapse_state_running_non_leaf_not_auto_expanded() {
        let mut state = AppState::new();
        // p1 has a child c1, so p1 is NOT a leaf — it must not land in expanded_step_run_ids
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        ];
        state.init_collapse_state();
        assert!(
            !state.expanded_step_run_ids.contains("p1"),
            "running non-leaf run must NOT be auto-expanded into expanded_step_run_ids"
        );
    }

    // --- multi-level expand/collapse tests ---

    #[test]
    fn visible_workflow_run_rows_grandchild_expanded() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        // p1 → c1 → gc1 (three levels, all expanded)
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
            make_wf_run_full("gc1", WorkflowRunStatus::Running, Some("c1")),
        ];
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
        assert!(
            matches!(&rows[1], WorkflowRunRow::Child { run_id, depth: 1, child_count: 1, collapsed: false, .. } if run_id == "c1")
        );
        assert!(
            matches!(&rows[2], WorkflowRunRow::Child { run_id, depth: 2, child_count: 0, collapsed: false, .. } if run_id == "gc1")
        );
    }

    #[test]
    fn visible_workflow_run_rows_intermediate_child_collapsed() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        // p1 → c1 → gc1; collapse c1 — gc1 should be hidden
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
            make_wf_run_full("gc1", WorkflowRunStatus::Running, Some("c1")),
        ];
        state.collapsed_workflow_run_ids.insert("c1".into());
        let rows = state.visible_workflow_run_rows();
        // p1 (expanded) + c1 (collapsed) = 2 rows; gc1 hidden
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
        assert!(
            matches!(&rows[1], WorkflowRunRow::Child { run_id, depth: 1, child_count: 1, collapsed: true, .. } if run_id == "c1")
        );
    }

    #[test]
    fn visible_workflow_run_rows_child_depth_values() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        // p1 → c1 (depth 1) → c2 (depth 2) → c3 (depth 3)
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
            make_wf_run_full("c2", WorkflowRunStatus::Running, Some("c1")),
            make_wf_run_full("c3", WorkflowRunStatus::Running, Some("c2")),
        ];
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 4);
        assert!(
            matches!(&rows[1], WorkflowRunRow::Child { run_id, depth: 1, .. } if run_id == "c1")
        );
        assert!(
            matches!(&rows[2], WorkflowRunRow::Child { run_id, depth: 2, .. } if run_id == "c2")
        );
        assert!(
            matches!(&rows[3], WorkflowRunRow::Child { run_id, depth: 3, .. } if run_id == "c3")
        );
    }

    #[test]
    fn visible_workflow_run_rows_leaf_child_count_zero() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Running, None),
            make_wf_run_full("c1", WorkflowRunStatus::Running, Some("p1")),
        ];
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 2);
        assert!(
            matches!(&rows[1], WorkflowRunRow::Child { run_id, child_count: 0, collapsed: false, depth: 1, .. } if run_id == "c1")
        );
    }

    // --- Step row tests ---

    fn make_wf_step(
        id: &str,
        run_id: &str,
        step_name: &str,
        position: i64,
    ) -> conductor_core::workflow::WorkflowRunStep {
        conductor_core::workflow::WorkflowRunStep {
            id: id.into(),
            workflow_run_id: run_id.into(),
            step_name: step_name.into(),
            role: "actor".into(),
            can_commit: false,
            condition_expr: None,
            status: conductor_core::workflow::WorkflowStepStatus::Completed,
            child_run_id: None,
            position,
            started_at: None,
            ended_at: None,
            result_text: None,
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: None,
            markers_out: None,
            retry_count: 0,
            gate_type: None,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: None,
        }
    }

    #[test]
    fn visible_workflow_run_rows_step_rows_appear_when_expanded() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.show_completed_workflow_runs = true;
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
        state.data.workflow_run_steps.insert(
            "p1".into(),
            vec![
                make_wf_step("s1", "p1", "lint", 0),
                make_wf_step("s2", "p1", "test", 1),
            ],
        );
        // Not expanded yet — no Step rows.
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));

        // Expand the step list for p1.
        state.expanded_step_run_ids.insert("p1".into());
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 3); // Parent + 2 Step rows
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
        assert!(matches!(&rows[1], WorkflowRunRow::Step { step_name, .. } if step_name == "lint"));
        assert!(matches!(&rows[2], WorkflowRunRow::Step { step_name, .. } if step_name == "test"));
    }

    #[test]
    fn visible_workflow_run_rows_steps_sorted_by_position() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.show_completed_workflow_runs = true;
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
        // Insert steps out-of-order by position.
        state.data.workflow_run_steps.insert(
            "p1".into(),
            vec![
                make_wf_step("s3", "p1", "deploy", 2),
                make_wf_step("s1", "p1", "lint", 0),
                make_wf_step("s2", "p1", "test", 1),
            ],
        );
        state.expanded_step_run_ids.insert("p1".into());
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 4);
        assert!(
            matches!(&rows[1], WorkflowRunRow::Step { step_name, position: 0, .. } if step_name == "lint")
        );
        assert!(
            matches!(&rows[2], WorkflowRunRow::Step { step_name, position: 1, .. } if step_name == "test")
        );
        assert!(
            matches!(&rows[3], WorkflowRunRow::Step { step_name, position: 2, .. } if step_name == "deploy")
        );
    }

    #[test]
    fn visible_workflow_run_rows_steps_for_leaf_child_run() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.show_completed_workflow_runs = true;
        // p1 → c1 (leaf). Steps should appear under c1 when expanded.
        state.data.workflow_runs = vec![
            make_wf_run_full("p1", WorkflowRunStatus::Completed, None),
            make_wf_run_full("c1", WorkflowRunStatus::Completed, Some("p1")),
        ];
        state
            .data
            .workflow_run_steps
            .insert("c1".into(), vec![make_wf_step("s1", "c1", "review", 0)]);
        state.expanded_step_run_ids.insert("c1".into());
        let rows = state.visible_workflow_run_rows();
        // Parent + Child + Step
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
        assert!(matches!(&rows[1], WorkflowRunRow::Child { run_id, .. } if run_id == "c1"));
        assert!(
            matches!(&rows[2], WorkflowRunRow::Step { run_id, step_name, depth: 2, .. } if run_id == "c1" && step_name == "review")
        );
    }

    #[test]
    fn visible_workflow_run_rows_filters_completed_by_default() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.data.workflow_runs = vec![
            make_wf_run_full("r1", WorkflowRunStatus::Completed, None),
            make_wf_run_full("r2", WorkflowRunStatus::Cancelled, None),
            make_wf_run_full("r3", WorkflowRunStatus::Failed, None),
            make_wf_run_full("r4", WorkflowRunStatus::Running, None),
        ];
        // Default: completed + cancelled hidden, failed + running shown.
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "r3"));
        assert!(matches!(&rows[1], WorkflowRunRow::Parent { run_id, .. } if run_id == "r4"));
        assert_eq!(state.hidden_workflow_run_count(), 2);

        // Toggle on: all four shown.
        state.show_completed_workflow_runs = true;
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(state.hidden_workflow_run_count(), 0);
    }

    #[test]
    fn visible_workflow_run_rows_no_steps_without_data() {
        // Even if a run is in expanded_step_run_ids, if there are no steps in
        // workflow_run_steps for that run, no Step rows should appear.
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.show_completed_workflow_runs = true;
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];
        state.expanded_step_run_ids.insert("p1".into());
        let rows = state.visible_workflow_run_rows();
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
    }

    #[test]
    fn visible_workflow_run_rows_parallel_group_header_and_members() {
        let mut state = AppState::new();
        set_worktree_mode(&mut state);
        state.show_completed_workflow_runs = true;
        state.data.workflow_runs = vec![make_wf_run_full("p1", WorkflowRunStatus::Completed, None)];

        // Two parallel steps sharing group id "g1", plus one sequential step.
        let mut lint = make_wf_step("s1", "p1", "lint", 0);
        lint.parallel_group_id = Some("g1".into());
        let mut test = make_wf_step("s2", "p1", "test", 1);
        test.parallel_group_id = Some("g1".into());
        let deploy = make_wf_step("s3", "p1", "deploy", 2);

        state
            .data
            .workflow_run_steps
            .insert("p1".into(), vec![lint, test, deploy]);
        state.expanded_step_run_ids.insert("p1".into());

        let rows = state.visible_workflow_run_rows();
        // Parent + ParallelGroup header + 2 member Steps + 1 sequential Step = 5
        assert_eq!(rows.len(), 5);
        assert!(matches!(&rows[0], WorkflowRunRow::Parent { run_id, .. } if run_id == "p1"));
        assert!(matches!(
            &rows[1],
            WorkflowRunRow::ParallelGroup {
                count: 2,
                depth: 1,
                ..
            }
        ));
        assert!(
            matches!(&rows[2], WorkflowRunRow::Step { step_name, depth: 2, .. } if step_name == "lint")
        );
        assert!(
            matches!(&rows[3], WorkflowRunRow::Step { step_name, depth: 2, .. } if step_name == "test")
        );
        assert!(
            matches!(&rows[4], WorkflowRunRow::Step { step_name, depth: 1, .. } if step_name == "deploy")
        );
    }

    // --- ColumnFocus navigation tests ---

    #[test]
    fn focused_index_and_len_workflow_column_defs() {
        let mut state = AppState::new();
        state.column_focus = ColumnFocus::Workflow;
        state.workflows_focus = WorkflowsFocus::Defs;
        state.workflow_def_index = 2;
        // With no defs loaded the len is 0; the important thing is the index
        // comes from workflow_def_index (not from a content-column list).
        let (idx, len) = state.focused_index_and_len();
        assert_eq!(idx, 2);
        assert_eq!(len, state.data.workflow_defs.len());
    }

    #[test]
    fn focused_index_and_len_workflow_column_runs() {
        let mut state = AppState::new();
        state.column_focus = ColumnFocus::Workflow;
        state.workflows_focus = WorkflowsFocus::Runs;
        state.workflow_run_index = 1;
        // Use non-global mode (worktree selected) so the single run produces exactly one row.
        state.selected_worktree_id = Some("w1".into());
        state.data.workflow_runs = vec![make_wf_run_full("r1", WorkflowRunStatus::Running, None)];
        let (idx, len) = state.focused_index_and_len();
        assert_eq!(idx, 1);
        assert_eq!(len, 1); // one visible row
    }

    #[test]
    fn focused_index_and_len_content_column_not_affected_by_workflow_index() {
        let mut state = AppState::new();
        state.column_focus = ColumnFocus::Content;
        state.workflows_focus = WorkflowsFocus::Runs;
        state.workflow_run_index = 99; // should be ignored — Dashboard/Repos is the default view
        let (idx, len) = state.focused_index_and_len();
        assert_eq!(idx, 0);
        assert_eq!(len, 0); // no repos loaded
    }

    #[test]
    fn set_focused_index_workflow_column_defs() {
        let mut state = AppState::new();
        state.column_focus = ColumnFocus::Workflow;
        state.workflows_focus = WorkflowsFocus::Defs;
        state.set_focused_index(3);
        assert_eq!(state.workflow_def_index, 3);
    }

    #[test]
    fn set_focused_index_workflow_column_runs() {
        let mut state = AppState::new();
        state.column_focus = ColumnFocus::Workflow;
        state.workflows_focus = WorkflowsFocus::Runs;
        state.set_focused_index(7);
        assert_eq!(state.workflow_run_index, 7);
    }

    #[test]
    fn set_focused_index_content_column_does_not_touch_workflow_indices() {
        let mut state = AppState::new();
        state.column_focus = ColumnFocus::Content;
        state.workflows_focus = WorkflowsFocus::Defs;
        state.workflow_def_index = 5;
        state.set_focused_index(2); // targets dashboard_index (Dashboard default)
        assert_eq!(state.workflow_def_index, 5); // unchanged
        assert_eq!(state.dashboard_index, 2);
    }
}
