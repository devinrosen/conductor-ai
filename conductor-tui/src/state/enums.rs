#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    RepoDetail,
    WorktreeDetail,
    WorkflowRunDetail,
    WorkflowDefDetail,
    Settings,
    /// Feature list view (repo-scoped or all repos).
    Features,
    /// Feature detail view: metadata + linked tickets + active worktrees.
    FeatureDetail,
}

/// Which panel of the Features list view has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeaturesFocus {
    #[default]
    List,
}

/// Which pane of the Settings view has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsFocus {
    /// Left pane: category list.
    #[default]
    CategoryList,
    /// Right pane: settings rows for the selected category.
    SettingsList,
}

/// Top-level categories in the Settings view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsCategory {
    #[default]
    General,
    Appearance,
    Notifications,
}

impl SettingsCategory {
    pub fn all() -> &'static [SettingsCategory] {
        &[
            SettingsCategory::General,
            SettingsCategory::Appearance,
            SettingsCategory::Notifications,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Notifications => "Notifications",
        }
    }
}

/// A row in the unified dashboard list — repo header or worktree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashboardRow {
    /// Index into `AppState::data.repos`.
    Repo(usize),
    /// Index into `AppState::data.worktrees`. `prefix` carries the tree-drawing
    /// prefix string (e.g. "├ ", "│ └ ") produced by `TreePosition::to_prefix()`.
    Worktree { idx: usize, prefix: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoDetailFocus {
    Info,
    Worktrees,
    Tickets,
    Prs,
    RepoAgent,
}

impl RepoDetailFocus {
    pub fn next(self) -> Self {
        match self {
            Self::Info => Self::Worktrees,
            Self::Worktrees => Self::Prs,
            Self::Prs => Self::Tickets,
            Self::Tickets => Self::RepoAgent,
            Self::RepoAgent => Self::Info,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Info => Self::RepoAgent,
            Self::Worktrees => Self::Info,
            Self::Prs => Self::Worktrees,
            Self::Tickets => Self::Prs,
            Self::RepoAgent => Self::Tickets,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowsFocus {
    Defs,
    Gates,
    Runs,
}

/// Whether a target header row represents a worktree or a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetType {
    Worktree,
    Pr,
}

impl WorkflowsFocus {
    /// Cycle forward: Gates → Runs → Defs → Gates, skipping Gates when empty.
    pub fn next_for_gates(self, has_gates: bool) -> Self {
        match self {
            Self::Gates => Self::Runs,
            Self::Runs => Self::Defs,
            Self::Defs => {
                if has_gates {
                    Self::Gates
                } else {
                    Self::Runs
                }
            }
        }
    }

    /// Cycle backward: Defs → Runs → Gates → Defs, skipping Gates when empty.
    pub fn prev_for_gates(self, has_gates: bool) -> Self {
        match self {
            Self::Defs => Self::Runs,
            Self::Runs => {
                if has_gates {
                    Self::Gates
                } else {
                    Self::Defs
                }
            }
            Self::Gates => Self::Defs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRunDetailFocus {
    Info,
    Error,
    Steps,
    AgentActivity,
}

impl WorkflowRunDetailFocus {
    /// Cycle forward: Info → Error (if has_error) → Steps → AgentActivity (if has_agent) → Info.
    pub fn next(self, has_agent: bool, has_error: bool) -> Self {
        match self {
            Self::Info => {
                if has_error {
                    Self::Error
                } else {
                    Self::Steps
                }
            }
            Self::Error => Self::Steps,
            Self::Steps => {
                if has_agent {
                    Self::AgentActivity
                } else {
                    Self::Info
                }
            }
            Self::AgentActivity => Self::Info,
        }
    }

    /// Cycle backward: Info ← Error (if has_error) ← Steps ← AgentActivity (if has_agent) ← Info.
    pub fn prev(self, has_agent: bool, has_error: bool) -> Self {
        match self {
            Self::Info => {
                if has_agent {
                    Self::AgentActivity
                } else {
                    Self::Steps
                }
            }
            Self::Error => Self::Info,
            Self::Steps => {
                if has_error {
                    Self::Error
                } else {
                    Self::Info
                }
            }
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
    pub const PR: usize = 9;
    /// Total number of navigable rows (used for bounds clamping).
    pub const COUNT: usize = 10;
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

/// Named row indices for the WorkflowRunDetail info panel.
/// These constants must stay in sync with the row order rendered in
/// `ui/workflows.rs::render_run_detail()`.
pub mod workflow_run_info_row {
    pub const RUN_ID: usize = 0;
    pub const WORKFLOW: usize = 1;
    pub const STATUS: usize = 2;
    pub const BRANCH: usize = 3;
    pub const PATH: usize = 4;
    pub const TICKET: usize = 5;
    pub const STARTED: usize = 6;
    pub const SUMMARY: usize = 7;
    /// Total number of navigable rows (used for bounds clamping).
    pub const COUNT: usize = 8;
}

/// One selectable item in the workflow picker modal.
///
/// `Workflow` is used in all picker contexts.  `StartAgent` and `Skip` are
/// extras appended only by the **post-create** flow (after a worktree is
/// created with a linked ticket) so the user can launch an agent or dismiss
/// instead of choosing a workflow.
#[derive(Clone, Debug)]
pub enum WorkflowPickerItem {
    Workflow(conductor_core::workflow::WorkflowDef),
    /// Non-selectable section header row, grouping workflows by their `group` field.
    Header(String),
    /// Post-create only: launch an AI agent on the new worktree.
    StartAgent,
    /// Post-create only: dismiss without running anything.
    Skip,
}

impl WorkflowPickerItem {
    pub fn is_selectable(&self) -> bool {
        !matches!(self, WorkflowPickerItem::Header(_))
    }

    pub fn name(&self) -> &str {
        match self {
            WorkflowPickerItem::Workflow(def) => def.display_name(),
            WorkflowPickerItem::Header(label) => label.as_str(),
            WorkflowPickerItem::StartAgent => "Start agent",
            WorkflowPickerItem::Skip => "Skip",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            WorkflowPickerItem::Workflow(def) => &def.description,
            WorkflowPickerItem::Header(_) => "",
            WorkflowPickerItem::StartAgent => "Launch an AI agent to work on this ticket",
            WorkflowPickerItem::Skip => "Dismiss and do nothing",
        }
    }
}

/// One selectable row in the branch picker modal.
#[derive(Debug, Clone)]
pub struct BranchPickerItem {
    /// `None` → repo default branch; `Some(branch)` → feature branch name.
    pub branch: Option<String>,
    /// Number of worktrees on this branch (0 for default branch entry).
    pub worktree_count: i64,
    /// Number of linked tickets (0 for default branch entry).
    pub ticket_count: i64,
    /// The base branch this item is based on (`None` for default branch entry).
    pub base_branch: Option<String>,
    /// Days since last activity (commit or worktree creation), if stale.
    pub stale_days: Option<u64>,
}

impl BranchPickerItem {
    /// Build the picker list from features (each paired with its pre-computed stale days)
    /// and unregistered (orphan) branches.
    /// The first entry is always `None` (repo default branch sentinel).
    pub fn from_features_and_orphans_with_stale(
        features: &[(conductor_core::feature::FeatureRow, Option<u64>)],
        orphans: &[conductor_core::feature::UnregisteredBranch],
    ) -> Vec<Self> {
        let mut items = vec![Self {
            branch: None,
            worktree_count: 0,
            ticket_count: 0,
            base_branch: None,
            stale_days: None,
        }];
        for (f, sd) in features {
            items.push(Self {
                branch: Some(f.branch.clone()),
                worktree_count: f.worktree_count,
                ticket_count: f.ticket_count,
                base_branch: Some(f.base_branch.clone()),
                stale_days: *sd,
            });
        }
        for orphan in orphans {
            items.push(Self {
                branch: Some(orphan.branch.clone()),
                worktree_count: orphan.worktree_count,
                ticket_count: 0,
                base_branch: orphan.base_branch.clone(),
                stale_days: None,
            });
        }
        items
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
    PostCreate {
        worktree_id: String,
        worktree_path: String,
        worktree_slug: String,
        ticket_id: String,
        repo_path: String,
    },
}

impl WorkflowPickerTarget {
    /// Returns the workflow target filter string for this picker target.
    pub fn target_filter(&self) -> &'static str {
        match self {
            Self::Pr { .. } => "pr",
            Self::Worktree { .. } => "worktree",
            Self::Ticket { .. } => "ticket",
            Self::Repo { .. } => "repo",
            Self::WorkflowRun { .. } => "workflow_run",
            Self::PostCreate { .. } => "worktree",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Carry creation params through the clone-warning confirm flow.
    CreateWorktree {
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
        from_pr: Option<u32>,
        from_branch: Option<String>,
        /// When true, skip the dirty-state error in `ensure_base_up_to_date()`.
        /// Set after the user confirms they want to proceed despite uncommitted changes.
        force_dirty: bool,
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
    ClearConversation {
        repo_slug: String,
        wt_slug: String,
        wt_id: String,
    },
    Quit,
}

#[derive(Debug, Clone, Default)]
pub enum FormFieldType {
    #[default]
    Text,
    Boolean,
}

#[derive(Debug, Clone)]
pub struct FormField {
    pub label: String,
    pub value: String,
    pub placeholder: String,
    pub manually_edited: bool,
    pub required: bool,
    pub readonly: bool,
    #[allow(dead_code)]
    pub field_type: FormFieldType,
}

#[derive(Debug, Clone)]
pub struct RunWorkflowAction {
    pub target: WorkflowPickerTarget,
    pub workflow_def: conductor_core::workflow::WorkflowDef,
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
    RunWorkflow(Box<RunWorkflowAction>),
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
        from_branch: Option<String>,
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
        slug: String,
    },
    /// Submit a response to a pending feedback request.
    FeedbackResponse {
        feedback_id: String,
    },
    /// Prompt for a repo-scoped read-only agent.
    RepoAgentPrompt {
        repo_id: String,
        repo_path: String,
        repo_slug: String,
        resume_session_id: Option<String>,
    },
    /// Second step: model picker for workflow runs.
    /// Carries the workflow target + inputs through the modal roundtrip.
    WorkflowModelOverride {
        action: Box<RunWorkflowAction>,
        inputs: std::collections::HashMap<String, String>,
    },
    /// Settings view: set the global model string (blank to clear).
    SettingsSetModel,
    /// Settings view: set the sync interval in minutes.
    SettingsSetSyncInterval,
}
