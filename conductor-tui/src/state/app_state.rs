use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use conductor_core::github::GithubPr;
use conductor_core::repo::Repo;
use conductor_core::tickets::Ticket;
use conductor_core::workflow::{PendingGateRow, WorkflowRunStatus};
use conductor_core::worktree::Worktree;
use ratatui::widgets::ListState;

use super::workflow_rows::max_iteration_for_run;
use super::{
    build_ticket_tree_indices, build_worktree_tree, build_worktree_tree_indices,
    parse_target_label, push_children, push_steps_for_run, ColumnFocus, DashboardRow, DataCache,
    FilterState, Modal, RepoDetailFocus, SettingsCategory, SettingsFocus, TargetType, TreePosition,
    View, WorkflowDefFocus, WorkflowRunDetailFocus, WorkflowRunRow, WorkflowsFocus,
};
use crate::theme::Theme;

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
    pub detail_wt_tree_positions: Vec<TreePosition>,
    pub detail_tickets: Vec<Ticket>,
    pub detail_prs: Vec<GithubPr>,
    pub detail_gates: Vec<PendingGateRow>,
    pub detail_wt_index: usize,
    pub detail_ticket_index: usize,
    pub detail_pr_index: usize,
    pub detail_gate_index: usize,
    /// When the PR list was last successfully fetched (None = never).
    pub pr_last_fetched_at: Option<std::time::Instant>,

    // Pre-filtered ticket lists (closed + text filter applied); index into these for nav/actions
    pub filtered_tickets: Vec<Ticket>,
    pub filtered_detail_tickets: Vec<Ticket>,
    /// Parallel tree-position metadata for `filtered_detail_tickets` (same length).
    pub detail_ticket_tree_positions: Vec<TreePosition>,
    /// Set of ticket IDs whose children are currently collapsed in the ticket list.
    pub collapsed_ticket_ids: HashSet<String>,

    // Agent activity list navigation (replaces the old Paragraph scroll offset)
    pub agent_list_state: RefCell<ListState>,
    /// Repo agent activity list navigation (repo detail view)
    pub repo_agent_list_state: RefCell<ListState>,
    // WorktreeDetail two-panel focus model
    pub worktree_detail_focus: super::WorktreeDetailFocus,
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
    /// Whether the workflow definitions pane is collapsed (session-only, not persisted).
    pub workflow_defs_collapsed: bool,
    pub workflow_def_index: usize,
    pub workflow_run_index: usize,
    pub workflow_step_index: usize,
    pub workflow_run_detail_focus: WorkflowRunDetailFocus,
    /// Selected row index in the WorkflowRunDetail info panel (for j/k navigation and y copy).
    pub workflow_run_info_row: usize,
    /// Vertical scroll offset for the Error pane in WorkflowRunDetail.
    pub error_pane_scroll: usize,
    pub step_agent_event_index: usize,
    /// Currently selected workflow run ID (for detail view)
    pub selected_workflow_run_id: Option<String>,
    /// Stack of parent workflow run IDs when drilling into child workflows.
    /// Each push saves the current run; Esc pops back to it.
    pub workflow_run_nav_stack: Vec<String>,
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
    /// foreach step IDs currently expanded in the step list (session-only, not persisted).
    pub expanded_foreach_step_ids: HashSet<String>,

    pub should_quit: bool,

    /// When false (default), closed tickets are hidden in all ticket views.
    pub show_closed_tickets: bool,

    /// When false (default), completed and cancelled workflow runs are hidden in the workflow column.
    pub show_completed_workflow_runs: bool,

    /// Cached result of `rebuild_workflow_run_rows()`. Invalidated at every mutation site.
    cached_workflow_run_rows: Vec<WorkflowRunRow>,

    /// Semantic colour theme — centralises all Color constants used by the UI.
    pub theme: Theme,

    /// True while a manual ticket sync is running in the background.
    pub ticket_sync_in_progress: bool,

    /// Which column currently has keyboard focus: Content (left) or Workflow (right).
    pub column_focus: ColumnFocus,

    /// When true, show the persistent workflow column on the right side.
    pub workflow_column_visible: bool,

    /// True while a background thread is loading workflow defs for the picker.
    pub loading_workflow_picker_defs: bool,

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

    // ── Workflow name filter ──────────────────────────────────────────────────
    /// Currently active filter string (None = no filter).
    pub workflow_name_filter: Option<String>,
    /// Text being typed in the filter bar.
    pub workflow_filter_input: String,

    // ── Settings view ────────────────────────────────────────────────────────
    /// Which pane of the Settings view has keyboard focus.
    pub settings_focus: SettingsFocus,
    /// Which category is selected in the left pane.
    pub settings_category: SettingsCategory,
    /// Selected row in the left-pane category list.
    pub settings_category_index: usize,
    /// Selected row index in the right-pane settings list.
    pub settings_row_index: usize,
    /// Per-hook last test result: `Ok(())` = fired, `Err(msg)` = error.
    pub settings_hook_test_results: HashMap<usize, Result<(), String>>,
    /// Snapshot of config values for display in the Settings view.
    /// Refreshed whenever Settings is opened or a value is changed.
    pub settings_display: SettingsDisplayCache,
}

/// Displayable snapshot of conductor config values for the Settings view.
#[derive(Debug, Clone, Default)]
pub struct SettingsDisplayCache {
    pub model: String,
    pub permission_mode: String,
    pub auto_start: String,
    pub sync_interval: String,
    pub auto_cleanup: String,
    pub theme: String,
    /// (on_pattern, run_or_url) pairs for each configured hook.
    pub hooks: Vec<(String, String)>,
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
            detail_wt_tree_positions: Vec::new(),
            detail_tickets: Vec::new(),
            detail_prs: Vec::new(),
            detail_gates: Vec::new(),
            detail_wt_index: 0,
            detail_ticket_index: 0,
            detail_pr_index: 0,
            detail_gate_index: 0,
            pr_last_fetched_at: None,
            filtered_tickets: Vec::new(),
            filtered_detail_tickets: Vec::new(),
            detail_ticket_tree_positions: Vec::new(),
            collapsed_ticket_ids: HashSet::new(),
            agent_list_state: RefCell::new(ListState::default()),
            repo_agent_list_state: RefCell::new(ListState::default()),
            worktree_detail_focus: super::WorktreeDetailFocus::InfoPanel,
            worktree_detail_selected_row: 0,
            repo_detail_info_row: 0,
            filter: FilterState::default(),
            detail_ticket_filter: FilterState::default(),
            label_filter: FilterState::default(),
            status_message: None,
            status_message_at: None,
            github_orgs_cache: Vec::new(),
            workflows_focus: WorkflowsFocus::Runs,
            workflow_defs_collapsed: false,
            workflow_def_index: 0,
            workflow_run_index: 0,
            workflow_step_index: 0,
            workflow_run_detail_focus: WorkflowRunDetailFocus::Steps,
            workflow_run_info_row: 0,
            error_pane_scroll: 0,
            step_agent_event_index: 0,
            selected_workflow_run_id: None,
            workflow_run_nav_stack: Vec::new(),
            collapsed_workflow_run_ids: HashSet::new(),
            collapsed_repo_headers: HashSet::new(),
            collapsed_target_headers: HashSet::new(),
            collapse_initialized: HashSet::new(),
            expanded_step_run_ids: HashSet::new(),
            expanded_foreach_step_ids: HashSet::new(),
            should_quit: false,
            show_closed_tickets: false,
            show_completed_workflow_runs: false,
            cached_workflow_run_rows: Vec::new(),
            ticket_sync_in_progress: false,
            loading_workflow_picker_defs: false,
            column_focus: ColumnFocus::Content,
            workflow_column_visible: true,
            home_dir: dirs::home_dir().map(|p| p.to_string_lossy().into_owned()),
            theme: Theme::default(),
            selected_workflow_def: None,
            workflow_def_detail_scroll: 0,
            workflow_def_focus: WorkflowDefFocus::List,
            workflow_def_step_index: 0,
            workflow_def_expanded_calls: HashSet::new(),
            workflow_name_filter: None,
            workflow_filter_input: String::new(),
            settings_focus: SettingsFocus::CategoryList,
            settings_category: SettingsCategory::General,
            settings_category_index: 0,
            settings_row_index: 0,
            settings_hook_test_results: HashMap::new(),
            settings_display: SettingsDisplayCache::default(),
        }
    }

    /// Total number of visual rows in the repo agent activity list.
    pub fn repo_agent_activity_len(&self) -> usize {
        self.data.repo_agent_activity_len()
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

    /// Returns the currently selected worktree, if any.
    pub fn selected_worktree(&self) -> Option<&Worktree> {
        self.selected_worktree_id
            .as_ref()
            .and_then(|id| self.data.worktrees.iter().find(|w| &w.id == id))
    }

    /// Returns the PR in `detail_prs` whose head branch matches `branch`, if any.
    pub fn find_pr_for_worktree(&self, branch: &str) -> Option<&GithubPr> {
        self.detail_prs.iter().find(|pr| pr.head_ref_name == branch)
    }

    /// Rebuild `detail_worktrees` and `detail_wt_tree_positions` from the current
    /// data cache for the given repo.  Must be called whenever the selected repo
    /// changes or the worktree list is refreshed.
    pub fn rebuild_detail_worktree_tree(&mut self, repo_id: &str) {
        let filtered_wts: Vec<_> = self
            .data
            .worktrees
            .iter()
            .filter(|wt| wt.repo_id == repo_id)
            .cloned()
            .collect();
        let repo_default = self
            .data
            .repos
            .iter()
            .find(|r| r.id == repo_id)
            .map(|r| r.default_branch.as_str())
            .unwrap_or("main");
        let (ordered, positions) = build_worktree_tree(&filtered_wts, repo_default);
        self.detail_worktrees = ordered;
        self.detail_wt_tree_positions = positions;
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

        // Build DFS tree order for detail tickets; also get child→parent map for
        // ancestor promotion during text filter (reuse instead of rebuilding).
        let (dfs_indices, dfs_positions, child_to_parent) =
            build_ticket_tree_indices(&self.detail_tickets, &self.data.ticket_dependencies);

        // When a text filter is active, find all ticket IDs that match plus their ancestors.
        let include_set: Option<std::collections::HashSet<&str>> =
            if let Some(f) = detail_filter_query.as_deref() {
                if !f.is_empty() {
                    let mut set = std::collections::HashSet::new();
                    for t in &self.detail_tickets {
                        if t.matches_filter(f) {
                            set.insert(t.id.as_str());
                            // Walk up the ancestor chain.
                            let mut cur = t.id.as_str();
                            while let Some(&parent) = child_to_parent.get(cur) {
                                if !set.insert(parent) {
                                    break; // already added this ancestor
                                }
                                cur = parent;
                            }
                        }
                    }
                    Some(set)
                } else {
                    None
                }
            } else {
                None
            };

        let mut filtered = Vec::with_capacity(self.detail_tickets.len());
        let mut positions = Vec::with_capacity(self.detail_tickets.len());

        // Walk DFS order; track whether each ancestor is collapsed to skip subtrees.
        // We use the `child_to_parent` map to check collapse status up the chain.
        'outer: for (dfs_idx, pos) in dfs_indices.iter().zip(dfs_positions.iter()) {
            let t = &self.detail_tickets[*dfs_idx];

            // Check if any ancestor is collapsed.
            let mut cur = t.id.as_str();
            while let Some(&parent) = child_to_parent.get(cur) {
                if self.collapsed_ticket_ids.contains(parent) {
                    continue 'outer;
                }
                cur = parent;
            }

            // Apply show_closed filter.
            if !self.show_closed_tickets && t.state == "closed" {
                continue;
            }

            // Apply text/include filter.
            if let Some(ref set) = include_set {
                if !set.contains(t.id.as_str()) {
                    continue;
                }
            }

            filtered.push(t.clone());
            positions.push(pos.clone());
        }

        self.filtered_detail_tickets = filtered;
        self.detail_ticket_tree_positions = positions;
    }

    /// Returns (current_index, list_length) for the currently focused pane.
    pub fn focused_index_and_len(&self) -> (usize, usize) {
        // When workflow column is focused, navigate within workflow panes.
        if self.column_focus == ColumnFocus::Workflow {
            return match self.workflows_focus {
                WorkflowsFocus::Defs => (self.workflow_def_index, self.data.workflow_defs.len()),
                WorkflowsFocus::Gates => (self.detail_gate_index, self.detail_gates.len()),
                WorkflowsFocus::Runs | WorkflowsFocus::Filter => (
                    self.workflow_run_index,
                    self.visible_workflow_run_rows_len(),
                ),
            };
        }
        match self.view {
            View::Dashboard => (self.dashboard_index, self.dashboard_rows().len()),
            View::RepoDetail => match self.repo_detail_focus {
                RepoDetailFocus::Info => (self.repo_detail_info_row, super::repo_info_row::COUNT),
                RepoDetailFocus::Worktrees => (self.detail_wt_index, self.detail_worktrees.len()),
                RepoDetailFocus::Tickets => {
                    (self.detail_ticket_index, self.filtered_detail_tickets.len())
                }
                RepoDetailFocus::Prs => (self.detail_pr_index, self.detail_prs.len()),
                RepoDetailFocus::RepoAgent => {
                    let idx = self.repo_agent_list_state.borrow().selected().unwrap_or(0);
                    (idx, self.repo_agent_activity_len())
                }
            },
            View::WorktreeDetail => {
                let idx = self.agent_list_state.borrow().selected().unwrap_or(0);
                (idx, self.data.agent_activity_len())
            }
            View::WorkflowRunDetail => match self.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Info => (
                    self.workflow_run_info_row,
                    super::workflow_run_info_row::COUNT,
                ),
                WorkflowRunDetailFocus::Error => (self.error_pane_scroll, 0),
                WorkflowRunDetailFocus::Steps => {
                    (self.workflow_step_index, self.data.workflow_steps.len())
                }
                WorkflowRunDetailFocus::AgentActivity => (
                    self.step_agent_event_index,
                    self.data.step_agent_events.len(),
                ),
            },
            View::WorkflowDefDetail => (self.workflow_def_detail_scroll, 0),
            View::Settings => (self.settings_row_index, 0),
        }
    }

    /// Sets the index for the currently focused pane.
    pub fn set_focused_index(&mut self, index: usize) {
        // When workflow column is focused, update workflow pane indices.
        if self.column_focus == ColumnFocus::Workflow {
            match self.workflows_focus {
                WorkflowsFocus::Defs => self.workflow_def_index = index,
                WorkflowsFocus::Gates => self.detail_gate_index = index,
                WorkflowsFocus::Runs | WorkflowsFocus::Filter => self.workflow_run_index = index,
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
                RepoDetailFocus::RepoAgent => {
                    self.repo_agent_list_state.borrow_mut().select(Some(index));
                }
            },
            View::WorktreeDetail => {
                self.agent_list_state.borrow_mut().select(Some(index));
            }
            View::WorkflowRunDetail => match self.workflow_run_detail_focus {
                WorkflowRunDetailFocus::Info => self.workflow_run_info_row = index,
                WorkflowRunDetailFocus::Error => self.error_pane_scroll = index,
                WorkflowRunDetailFocus::Steps => self.workflow_step_index = index,
                WorkflowRunDetailFocus::AgentActivity => self.step_agent_event_index = index,
            },
            View::WorkflowDefDetail => {
                self.workflow_def_detail_scroll = index;
            }
            View::Settings => {
                self.settings_row_index = index;
            }
        }
    }

    fn workflow_name_filter_lower(&self) -> Option<String> {
        self.workflow_name_filter.as_deref().map(str::to_lowercase)
    }

    fn run_matches_name_filter(
        filter_lower: &Option<String>,
        run: &conductor_core::workflow::WorkflowRun,
    ) -> bool {
        match filter_lower {
            None => true,
            Some(f) => run.workflow_name.to_lowercase().contains(f.as_str()),
        }
    }

    /// Recomputes `cached_workflow_run_rows` from current state.
    /// Must be called after any mutation to workflow_runs, workflow_run_steps,
    /// show_completed_workflow_runs, workflow_name_filter, collapsed_workflow_run_ids,
    /// expanded_step_run_ids, collapsed_repo_headers, or collapsed_target_headers.
    pub fn rebuild_workflow_run_rows(&mut self) {
        self.cached_workflow_run_rows = self.compute_workflow_run_rows();
    }

    /// Returns the cached flat, ordered list of visible workflow run rows.
    pub fn visible_workflow_run_rows(&self) -> &[WorkflowRunRow] {
        &self.cached_workflow_run_rows
    }

    fn compute_workflow_run_rows(&self) -> Vec<WorkflowRunRow> {
        let runs = &self.data.workflow_runs;
        let known_ids: HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();

        // Build parent_id → sorted-children map.
        let mut children_map: HashMap<&str, Vec<&conductor_core::workflow::WorkflowRun>> =
            HashMap::new();
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

        // Precompute the lowercased filter once to avoid per-run allocations.
        let name_filter_lower = self.workflow_name_filter_lower();

        if !global_mode {
            // Non-global mode: flat list, optionally hiding completed/cancelled root runs.
            // In repo-detail mode (repo selected, no worktree selected), emit SlugLabel rows
            // above groups of runs sharing the same worktree slug.
            let repo_detail_mode =
                self.selected_repo_id.is_some() && self.selected_worktree_id.is_none();
            let mut result = Vec::new();
            let mut last_emitted_slug: Option<String> = None;
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
                if !Self::run_matches_name_filter(&name_filter_lower, run) {
                    continue;
                }

                if repo_detail_mode {
                    // Extract the worktree slug from target_label (format "repo/slug").
                    // PR-format labels ("owner/repo#42") are TargetType::Pr and must be skipped.
                    let slug: Option<String> = run
                        .target_label
                        .as_deref()
                        .map(parse_target_label)
                        .and_then(|(_, target_key, target_type)| {
                            if target_type == TargetType::Worktree && !target_key.is_empty() {
                                Some(target_key)
                            } else {
                                None
                            }
                        });
                    if let Some(ref s) = slug {
                        if last_emitted_slug.as_deref() != Some(s.as_str()) {
                            result.push(WorkflowRunRow::SlugLabel { label: s.clone() });
                            last_emitted_slug = Some(s.clone());
                        }
                    }
                }

                let child_count = children_map.get(run.id.as_str()).map_or(0, |v| v.len());
                let collapsed = self.collapsed_workflow_run_ids.contains(&run.id);
                let max_iteration =
                    max_iteration_for_run(run.id.as_str(), &self.data.workflow_run_steps);
                result.push(WorkflowRunRow::Parent {
                    run_id: run.id.clone(),
                    collapsed,
                    child_count,
                    max_iteration,
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
        let mut groups: Vec<(
            String,
            String,
            TargetType,
            &conductor_core::workflow::WorkflowRun,
        )> = Vec::new();
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
            if !Self::run_matches_name_filter(&name_filter_lower, run) {
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
            // Fallback: resolve repo slug via worktree_id → repo_id (#1539)
            if repo_slug == "unknown" {
                if let Some(wt_id) = run.worktree_id.as_deref() {
                    if let Some(wt) = self.data.worktrees.iter().find(|w| w.id == wt_id) {
                        if let Some(&slug) = repo_slug_map.get(wt.repo_id.as_str()) {
                            repo_slug = slug.to_string();
                        }
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

        // Pre-compute run counts per repo and per (repo, target) to avoid O(R·N + R·T·N) scans.
        let mut run_counts_by_repo: HashMap<&str, usize> = HashMap::new();
        let mut run_counts_by_target: HashMap<(&str, &str), usize> = HashMap::new();
        for (repo_slug, target_key, _, _) in &groups {
            *run_counts_by_repo.entry(repo_slug.as_str()).or_insert(0) += 1;
            *run_counts_by_target
                .entry((repo_slug.as_str(), target_key.as_str()))
                .or_insert(0) += 1;
        }

        // Build the final visible row list.
        let mut result = Vec::new();
        for repo_slug in &repo_order {
            let run_count = run_counts_by_repo
                .get(repo_slug.as_str())
                .copied()
                .unwrap_or(0);
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
                let target_run_count = run_counts_by_target
                    .get(&(repo_slug.as_str(), target_key.as_str()))
                    .copied()
                    .unwrap_or(0);
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
                    let max_iteration =
                        max_iteration_for_run(run.id.as_str(), &self.data.workflow_run_steps);
                    result.push(WorkflowRunRow::Parent {
                        run_id: run.id.clone(),
                        collapsed,
                        child_count,
                        max_iteration,
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

    /// Returns the number of cached visible workflow run rows.
    pub fn visible_workflow_run_rows_len(&self) -> usize {
        self.cached_workflow_run_rows.len()
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
    /// Each repo appears first, then its worktrees in `build_worktree_tree()` order
    /// with tree-drawing prefixes from `TreePosition::to_prefix()`.
    pub fn dashboard_rows(&self) -> Vec<DashboardRow> {
        // Build an index: repo_id → [(global_wt_idx, &Worktree)]
        let mut wts_by_repo: HashMap<&str, Vec<(usize, &conductor_core::worktree::Worktree)>> =
            HashMap::new();
        for (idx, wt) in self.data.worktrees.iter().enumerate() {
            wts_by_repo
                .entry(wt.repo_id.as_str())
                .or_default()
                .push((idx, wt));
        }

        let mut rows = Vec::new();
        for (repo_idx, repo) in self.data.repos.iter().enumerate() {
            rows.push(DashboardRow::Repo(repo_idx));

            let repo_wts = wts_by_repo.get(repo.id.as_str());
            let empty_wts = Vec::new();
            let repo_wts = repo_wts.unwrap_or(&empty_wts);

            if repo_wts.is_empty() {
                continue;
            }

            // Map local index → global index
            let local_to_global: Vec<usize> = repo_wts.iter().map(|&(idx, _)| idx).collect();

            // Collect worktree refs for tree ordering (no cloning needed)
            let wt_refs: Vec<&Worktree> = repo_wts.iter().map(|&(_, wt)| wt).collect();
            let (ordered_local_indices, positions) =
                build_worktree_tree_indices(&wt_refs, &repo.default_branch);

            for (local_idx, pos) in ordered_local_indices.iter().zip(positions.iter()) {
                let global_idx = local_to_global[*local_idx];
                // Build the full display prefix: "  " indent under repo header,
                // plus tree connectors. to_prefix() returns empty for depth-0
                // roots, so we add their connectors here.
                let prefix = if pos.depth == 0 {
                    if pos.is_last_sibling {
                        "  └ ".to_string()
                    } else {
                        "  ├ ".to_string()
                    }
                } else {
                    format!("  {}", pos.to_prefix())
                };
                rows.push(DashboardRow::Worktree {
                    idx: global_idx,
                    prefix,
                });
            }
        }
        rows
    }

    /// Returns the dashboard row at `dashboard_index`.
    /// Delegates to `dashboard_rows()` to guarantee the two can never diverge.
    pub fn current_dashboard_row(&self) -> Option<DashboardRow> {
        self.dashboard_rows().get(self.dashboard_index).cloned()
    }

    /// Get the currently selected repo, if any.
    /// When the cursor is on a worktree or feature row, returns that item's owning repo.
    pub fn selected_repo(&self) -> Option<&Repo> {
        match self.current_dashboard_row()? {
            DashboardRow::Repo(idx) => self.data.repos.get(idx),
            DashboardRow::Worktree { idx, .. } => {
                let wt = self.data.worktrees.get(idx)?;
                self.data.repos.iter().find(|r| r.id == wt.repo_id)
            }
        }
    }

    /// Get the currently selected ticket from the dashboard list.
    #[allow(dead_code)]
    pub fn selected_ticket(&self) -> Option<&Ticket> {
        self.data.tickets.get(self.ticket_index)
    }

    /// Returns true if the selected workflow run has failed with a non-empty error field.
    pub fn selected_run_has_error(&self) -> bool {
        self.selected_workflow_run_id
            .as_ref()
            .and_then(|id| self.data.workflow_runs.iter().find(|r| &r.id == id))
            .map(|run| {
                run.status == WorkflowRunStatus::Failed
                    && run.error.as_ref().is_some_and(|s| !s.is_empty())
            })
            .unwrap_or(false)
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
