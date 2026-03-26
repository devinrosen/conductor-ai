use std::fmt;

use conductor_core::github::DiscoveredRepo;
use conductor_core::issue_source::IssueSource;
use conductor_core::tickets::Ticket;
use tui_textarea::TextArea;

use super::{
    BranchPickerItem, ConfirmAction, FormAction, FormField, InputAction, TreePosition,
    WorkflowPickerItem, WorkflowPickerTarget,
};

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
        /// When true, show a "Default (per-agent frontmatter)" row at index 0.
        /// Used for run-time pickers (agent + workflow) where the user can opt out of overriding.
        allow_default: bool,
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
    /// Generic workflow picker — opened by `w` key or after worktree creation.
    WorkflowPicker {
        target: WorkflowPickerTarget,
        items: Vec<WorkflowPickerItem>,
        selected: usize,
    },
    /// Template picker: browse and select a built-in workflow template for instantiation.
    TemplatePicker {
        items: Vec<conductor_core::workflow_template::WorkflowTemplate>,
        selected: usize,
        repo_slug: String,
        repo_path: String,
        worktree_path: Option<String>,
    },
    /// Non-dismissable progress indicator shown while a background operation runs.
    Progress {
        message: String,
    },
    /// Branch picker shown during worktree creation: select a target branch.
    BranchPicker {
        repo_slug: String,
        wt_name: String,
        ticket_id: Option<String>,
        items: Vec<BranchPickerItem>,
        tree_positions: Vec<TreePosition>,
        selected: usize,
    },
    /// Branch picker for changing worktree base branch (separate from creation-flow BranchPicker).
    BaseBranchPicker {
        repo_slug: String,
        wt_slug: String,
        items: Vec<BranchPickerItem>,
        tree_positions: Vec<TreePosition>,
        selected: usize,
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
    /// In-app notification list modal.
    Notifications {
        notifications: Vec<conductor_core::notification_manager::Notification>,
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
            Modal::BaseBranchPicker { .. } => write!(f, "Modal::BaseBranchPicker"),
            Modal::BranchPicker { .. } => write!(f, "Modal::BranchPicker"),
            Modal::WorkflowPicker { ref target, .. } => {
                write!(f, "Modal::WorkflowPicker(target={target:?})")
            }
            Modal::TemplatePicker { selected, .. } => {
                write!(f, "Modal::TemplatePicker(selected={selected})")
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
            Modal::Notifications { selected, .. } => {
                write!(f, "Modal::Notifications(selected={selected})")
            }
        }
    }
}
