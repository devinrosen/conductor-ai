use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use conductor_core::agent::{
    AgentCreatedIssue, AgentRun, AgentRunEvent, FeedbackRequest, TicketAgentTotals,
};
use conductor_core::config::WorkTarget;
use conductor_core::github::DiscoveredRepo;
use conductor_core::issue_source::IssueSource;
use conductor_core::repo::Repo;
use conductor_core::tickets::Ticket;
use conductor_core::workflow::{WorkflowRun, WorkflowRunStep};
use conductor_core::workflow_dsl::WorkflowDef;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowsFocus {
    Defs,
    Runs,
}

impl WorkflowsFocus {
    pub fn toggle(self) -> Self {
        match self {
            Self::Defs => Self::Runs,
            Self::Runs => Self::Defs,
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
            Modal::IssueSourceManager { .. } => write!(f, "Modal::IssueSourceManager"),
            Modal::ModelPicker {
                ref context_label, ..
            } => {
                write!(f, "Modal::ModelPicker(ctx={context_label:?})")
            }
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
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteWorktree {
        repo_slug: String,
        wt_slug: String,
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
    DeleteIssueSource {
        source_id: String,
        repo_id: String,
        repo_slug: String,
        remote_url: String,
    },
    CancelWorkflow {
        workflow_run_id: String,
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
#[allow(clippy::enum_variant_names)]
pub enum FormAction {
    AddRepo,
    AddWorkTarget,
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
    /// Workflow definitions for the currently viewed worktree
    pub workflow_defs: Vec<WorkflowDef>,
    /// Workflow runs for the currently viewed worktree
    pub workflow_runs: Vec<WorkflowRun>,
    /// Steps for the currently viewed workflow run
    pub workflow_steps: Vec<WorkflowRunStep>,
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
    pub detail_wt_index: usize,
    pub detail_ticket_index: usize,

    // Agent activity list navigation (replaces the old Paragraph scroll offset)
    pub agent_list_state: RefCell<ListState>,
    /// Tracks pending `g` keypress for `gg` chord (go to top)
    pub pending_g: bool,

    // Filter
    pub filter_active: bool,
    pub filter_text: String,

    // Status bar message
    pub status_message: Option<String>,

    /// Cached org list so navigating back from repo modal doesn't re-fetch.
    pub github_orgs_cache: Vec<String>,

    // Workflow state
    pub workflows_focus: WorkflowsFocus,
    pub workflow_def_index: usize,
    pub workflow_run_index: usize,
    pub workflow_step_index: usize,
    /// Currently selected workflow run ID (for detail view)
    pub selected_workflow_run_id: Option<String>,

    pub should_quit: bool,
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
            detail_wt_index: 0,
            detail_ticket_index: 0,
            agent_list_state: RefCell::new(ListState::default()),
            pending_g: false,
            filter_active: false,
            filter_text: String::new(),
            status_message: None,
            github_orgs_cache: Vec::new(),
            workflows_focus: WorkflowsFocus::Defs,
            workflow_def_index: 0,
            workflow_run_index: 0,
            workflow_step_index: 0,
            selected_workflow_run_id: None,
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
            View::WorktreeDetail => {
                let idx = self.agent_list_state.borrow().selected().unwrap_or(0);
                (idx, self.data.agent_activity_len())
            }
            View::Tickets => (self.ticket_index, self.data.tickets.len()),
            View::Workflows => match self.workflows_focus {
                WorkflowsFocus::Defs => (self.workflow_def_index, self.data.workflow_defs.len()),
                WorkflowsFocus::Runs => (self.workflow_run_index, self.data.workflow_runs.len()),
            },
            View::WorkflowRunDetail => (self.workflow_step_index, self.data.workflow_steps.len()),
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
            View::WorktreeDetail => {
                self.agent_list_state.borrow_mut().select(Some(index));
            }
            View::Tickets => self.ticket_index = index,
            View::Workflows => match self.workflows_focus {
                WorkflowsFocus::Defs => self.workflow_def_index = index,
                WorkflowsFocus::Runs => self.workflow_run_index = index,
            },
            View::WorkflowRunDetail => self.workflow_step_index = index,
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
}
