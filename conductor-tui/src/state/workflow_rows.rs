use super::TargetType;

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
        /// Highest `iteration` value seen across all steps for this run (0-indexed).
        /// 0 means either a single-pass run or steps not yet loaded.
        max_iteration: i64,
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
        /// Highest `iteration` value seen across all steps for this run (0-indexed).
        /// 0 means either a single-pass run or steps not yet loaded.
        max_iteration: i64,
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
    /// A non-interactive worktree slug label shown above a group of runs in repo-detail mode.
    SlugLabel { label: String },
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
            | WorkflowRunRow::ParallelGroup { .. }
            | WorkflowRunRow::SlugLabel { .. } => None,
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

/// Compute the highest iteration seen for each step name.
/// Returns a map of `step_name → max_iteration` for use in filtering
/// duplicated loop iterations from both the tree view and the detail panel.
pub(crate) fn max_iter_by_step_name(
    steps: &[conductor_core::workflow::WorkflowRunStep],
) -> std::collections::HashMap<String, i64> {
    let mut map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for s in steps {
        let e = map.entry(s.step_name.clone()).or_insert(0);
        if s.iteration > *e {
            *e = s.iteration;
        }
    }
    map
}

/// Append step rows for `run_id` when it is in `expanded_step_run_ids`.
pub(super) fn push_steps_for_run(
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
        // Use per-step-name max iteration (via shared helper) so the detail panel and the
        // tree show consistent steps for partially-completed loops.
        let max_iter_by_name = max_iter_by_step_name(steps);
        let mut ordered: Vec<_> = steps
            .iter()
            .filter(|s| s.iteration == *max_iter_by_name.get(&s.step_name).unwrap_or(&0))
            .collect();
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

/// Return the highest iteration number seen in the steps for `run_id`, or 0.
pub(super) fn max_iteration_for_run(
    run_id: &str,
    workflow_run_steps: &std::collections::HashMap<
        String,
        Vec<conductor_core::workflow::WorkflowRunStep>,
    >,
) -> i64 {
    workflow_run_steps
        .get(run_id)
        .map(|steps| steps.iter().map(|s| s.iteration).max().unwrap_or(0))
        .unwrap_or(0)
}

/// Recursively append `Child` rows for `parent_id` into `rows`.
/// `depth` starts at 1 for direct children of a root run.
///
/// Iteration filtering: uses the `iteration` field stored directly on each child
/// `WorkflowRun` record. Groups children by `workflow_name` and keeps only those
/// at the maximum iteration for their name.
///
/// Direct-step interleaving: when the parent is in `expanded_step_run_ids`, non-sub-workflow
/// steps (agent calls, scripts) are interleaved with child runs, sorted by position.
pub(super) fn push_children(
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

    // Build max iteration per workflow_name among children.
    let mut max_iter_by_name: std::collections::HashMap<&str, i64> =
        std::collections::HashMap::new();
    for child in children {
        let e = max_iter_by_name
            .entry(child.workflow_name.as_str())
            .or_insert(0);
        if child.iteration > *e {
            *e = child.iteration;
        }
    }

    // Filter: keep children at their name's max iteration.
    let filtered_children: Vec<&&conductor_core::workflow::WorkflowRun> = children
        .iter()
        .filter(|child| {
            child.iteration
                >= *max_iter_by_name
                    .get(child.workflow_name.as_str())
                    .unwrap_or(&0)
        })
        .collect();

    // Build the set of child workflow run IDs for distinguishing sub-workflow steps from direct steps.
    let child_wf_run_ids: std::collections::HashSet<&str> =
        filtered_children.iter().map(|c| c.id.as_str()).collect();

    // Build a position map for child runs: child_run_id → position from the parent's step list.
    // This is used to sort children and direct steps by their position in the parent workflow.
    let parent_steps = workflow_run_steps.get(parent_id);

    let child_position: std::collections::HashMap<&str, i64> = parent_steps
        .map(|steps| {
            steps
                .iter()
                .filter_map(|s| {
                    s.child_run_id.as_deref().and_then(|cid| {
                        if child_wf_run_ids.contains(cid) {
                            Some((cid, s.position))
                        } else {
                            None
                        }
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Collect direct steps (non-sub-workflow steps) from the parent.
    // push_children is only called when the parent is not collapsed, so direct steps
    // should always be visible alongside child runs — no expanded_step_run_ids gate.
    //
    // Use the GLOBAL max iteration (across all step names) so we only show direct
    // steps from the current loop iteration. Per-step-name max would mix steps from
    // different iterations — e.g. address-reviews from iteration 0 alongside child
    // runs from iteration 1 (because address-reviews hasn't run in iteration 1 yet).
    let direct_steps: Vec<&conductor_core::workflow::WorkflowRunStep> =
        if let Some(steps) = parent_steps {
            let global_max_iter = steps.iter().map(|s| s.iteration).max().unwrap_or(0);
            steps
                .iter()
                .filter(|s| {
                    // Keep only steps from the current (global max) iteration.
                    s.iteration == global_max_iter
                })
                .filter(|s| {
                    // Exclude sub-workflow steps — they already appear as child run rows.
                    // The engine names these "workflow:<name>" in execute_call_workflow.
                    !s.step_name.starts_with("workflow:")
                })
                .collect()
        } else {
            Vec::new()
        };

    // Build a merged, position-sorted list of items (children + direct steps).
    enum TreeItem<'a> {
        ChildRun(&'a conductor_core::workflow::WorkflowRun),
        DirectStep(&'a conductor_core::workflow::WorkflowRunStep),
    }

    let mut items: Vec<(i64, TreeItem<'_>)> = Vec::new();
    for child in &filtered_children {
        let pos = child_position
            .get(child.id.as_str())
            .copied()
            .unwrap_or(i64::MAX);
        items.push((pos, TreeItem::ChildRun(child)));
    }
    for step in &direct_steps {
        items.push((step.position, TreeItem::DirectStep(step)));
    }
    items.sort_by_key(|(pos, _)| *pos);

    for (_, item) in items {
        match item {
            TreeItem::ChildRun(child) => {
                let child_count = children_map.get(child.id.as_str()).map_or(0, |v| v.len());
                let collapsed = collapsed_ids.contains(&child.id);
                let max_iteration = max_iteration_for_run(child.id.as_str(), workflow_run_steps);
                rows.push(WorkflowRunRow::Child {
                    run_id: child.id.clone(),
                    parent_id: parent_id.to_string(),
                    depth,
                    collapsed,
                    child_count,
                    max_iteration,
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
            TreeItem::DirectStep(step) => {
                rows.push(WorkflowRunRow::Step {
                    run_id: parent_id.to_string(),
                    step_id: step.id.clone(),
                    step_name: step.step_name.clone(),
                    status: step.status.to_string(),
                    position: step.position,
                    depth,
                    role: step.role.clone(),
                    parallel_group_id: step.parallel_group_id.clone(),
                });
            }
        }
    }
}

/// Count the rows that `push_steps_for_run` would emit, without building them.
pub(super) fn count_steps_for_run(
    run_id: &str,
    expanded_step_run_ids: &std::collections::HashSet<String>,
    workflow_run_steps: &std::collections::HashMap<
        String,
        Vec<conductor_core::workflow::WorkflowRunStep>,
    >,
) -> usize {
    if !expanded_step_run_ids.contains(run_id) {
        return 0;
    }
    let Some(steps) = workflow_run_steps.get(run_id) else {
        return 0;
    };
    let max_iter_by_name = max_iter_by_step_name(steps);
    let ordered: Vec<_> = steps
        .iter()
        .filter(|s| s.iteration == *max_iter_by_name.get(&s.step_name).unwrap_or(&0))
        .collect();
    let mut count = 0;
    let mut seen_groups: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for step in &ordered {
        match step.parallel_group_id.as_deref() {
            None => count += 1,
            Some(gid) => {
                if seen_groups.insert(gid) {
                    let member_count = ordered
                        .iter()
                        .filter(|s| s.parallel_group_id.as_deref() == Some(gid))
                        .count();
                    // ParallelGroup header row + one Step row per member.
                    count += 1 + member_count;
                }
            }
        }
    }
    count
}

/// Count the rows that `push_children` would emit for `parent_id`, without building them.
pub(super) fn count_children_rows(
    parent_id: &str,
    children_map: &std::collections::HashMap<&str, Vec<&conductor_core::workflow::WorkflowRun>>,
    collapsed_ids: &std::collections::HashSet<String>,
    expanded_step_run_ids: &std::collections::HashSet<String>,
    workflow_run_steps: &std::collections::HashMap<
        String,
        Vec<conductor_core::workflow::WorkflowRunStep>,
    >,
) -> usize {
    let Some(children) = children_map.get(parent_id) else {
        return 0;
    };

    // Build max iteration per workflow_name among children.
    let mut max_iter_by_name: std::collections::HashMap<&str, i64> =
        std::collections::HashMap::new();
    for child in children {
        let e = max_iter_by_name
            .entry(child.workflow_name.as_str())
            .or_insert(0);
        if child.iteration > *e {
            *e = child.iteration;
        }
    }

    // Filter: keep children at their name's max iteration.
    let filtered_children: Vec<_> = children
        .iter()
        .filter(|child| {
            child.iteration
                >= *max_iter_by_name
                    .get(child.workflow_name.as_str())
                    .unwrap_or(&0)
        })
        .collect();

    // Count direct (non-sub-workflow) steps from the parent — always shown alongside children.
    let direct_step_count = if let Some(steps) = workflow_run_steps.get(parent_id) {
        let global_max_iter = steps.iter().map(|s| s.iteration).max().unwrap_or(0);
        steps
            .iter()
            .filter(|s| {
                s.iteration == global_max_iter && !s.step_name.starts_with("workflow:")
            })
            .count()
    } else {
        0
    };

    // Count child run rows: 1 (Child row) + recursive contents if not collapsed.
    let child_rows: usize = filtered_children
        .iter()
        .map(|child| {
            let child_child_count =
                children_map.get(child.id.as_str()).map_or(0, |v| v.len());
            let collapsed = collapsed_ids.contains(&child.id);
            let mut n = 1; // The Child row itself.
            if !collapsed {
                if child_child_count == 0 {
                    n += count_steps_for_run(
                        &child.id,
                        expanded_step_run_ids,
                        workflow_run_steps,
                    );
                } else {
                    n += count_children_rows(
                        &child.id,
                        children_map,
                        collapsed_ids,
                        expanded_step_run_ids,
                        workflow_run_steps,
                    );
                }
            }
            n
        })
        .sum();

    direct_step_count + child_rows
}
