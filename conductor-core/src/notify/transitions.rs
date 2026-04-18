use crate::agent::{AgentRun, AgentRunStatus};
use crate::workflow::{WorkflowRun, WorkflowRunStatus};

use super::parse_target_label;

/// A workflow run that freshly transitioned to a terminal state.
pub struct WorkflowTerminalTransition {
    pub run_id: String,
    pub workflow_name: String,
    pub target_label: Option<String>,
    pub succeeded: bool,
    pub parent_workflow_run_id: Option<String>,
    pub repo_slug: String,
    pub branch: String,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub repo_id: Option<String>,
    pub worktree_id: Option<String>,
}

/// Detect workflow runs that have freshly transitioned to a terminal status.
///
/// `seen` is updated in-place, stale entries are pruned, and `initialized`
/// prevents spurious notifications on the first call.
pub fn detect_workflow_terminal_transitions<'a>(
    runs: impl Iterator<Item = &'a WorkflowRun>,
    seen: &mut std::collections::HashMap<String, WorkflowRunStatus>,
    initialized: &mut bool,
) -> Vec<WorkflowTerminalTransition> {
    let runs: Vec<_> = runs.collect();
    let mut transitions = Vec::new();

    for run in &runs {
        // Sub-workflow notifications are suppressed — failures propagate to the root run.
        if run.parent_workflow_run_id.is_some() {
            seen.insert(run.id.clone(), run.status.clone());
            continue;
        }

        let now_terminal = matches!(
            run.status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed
        );
        if *initialized {
            let prev_status = seen.get(&run.id);
            let status_changed = prev_status.map(|s| s != &run.status).unwrap_or(true);
            if now_terminal && status_changed {
                let succeeded = matches!(run.status, WorkflowRunStatus::Completed);
                // Parse repo_slug/branch from target_label (format: "repo_slug/branch")
                let (repo_slug, branch) = {
                    let (r, b) = parse_target_label(run.target_label.as_deref());
                    (r.to_string(), b.to_string())
                };
                let duration_ms = run.total_duration_ms.map(|ms| ms as u64);
                let error = if !succeeded { run.error.clone() } else { None };
                transitions.push(WorkflowTerminalTransition {
                    run_id: run.id.clone(),
                    workflow_name: run.display_name().to_string(),
                    target_label: run.target_label.clone(),
                    succeeded,
                    parent_workflow_run_id: run.parent_workflow_run_id.clone(),
                    repo_slug,
                    branch,
                    duration_ms,
                    error,
                    repo_id: run.repo_id.clone(),
                    worktree_id: run.worktree_id.clone(),
                });
            }
        }
        seen.insert(run.id.clone(), run.status.clone());
    }

    *initialized = true;

    // Prune stale entries to prevent unbounded growth
    let current_ids: std::collections::HashSet<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    seen.retain(|id, _| current_ids.contains(id.as_str()));

    transitions
}

/// An agent run that freshly transitioned to a terminal state.
pub struct AgentTerminalTransition {
    pub run_id: String,
    pub worktree_slug: Option<String>,
    pub succeeded: bool,
    pub error_msg: Option<String>,
    pub repo_slug: String,
    pub branch: String,
    pub duration_ms: Option<u64>,
}

/// Detect agent runs that have freshly transitioned to a terminal status.
///
/// Works identically to `detect_new_terminal_transitions` for workflow runs:
/// `seen` is updated in-place, stale entries are pruned, and `initialized`
/// prevents spurious notifications on the first call.
///
/// `runs` is an iterator of `(worktree_slug, &AgentRun)` pairs.
pub fn detect_agent_terminal_transitions<'a>(
    runs: impl Iterator<Item = (Option<&'a str>, &'a AgentRun)>,
    seen: &mut std::collections::HashMap<String, AgentRunStatus>,
    initialized: &mut bool,
) -> Vec<AgentTerminalTransition> {
    let runs: Vec<_> = runs.collect();
    let mut transitions = Vec::new();

    for (slug, run) in &runs {
        let now_terminal = matches!(
            run.status,
            AgentRunStatus::Completed | AgentRunStatus::Failed | AgentRunStatus::Cancelled
        );
        if *initialized {
            let prev = seen.get(&run.id);
            let changed = prev.map(|s| s != &run.status).unwrap_or(true);
            if now_terminal && changed {
                let succeeded = run.status == AgentRunStatus::Completed;
                let duration_ms = run.duration_ms.map(|ms| ms as u64);
                transitions.push(AgentTerminalTransition {
                    run_id: run.id.clone(),
                    worktree_slug: slug.map(|s| s.to_string()),
                    succeeded,
                    error_msg: if !succeeded {
                        run.result_text.clone()
                    } else {
                        None
                    },
                    repo_slug: String::new(),
                    branch: String::new(),
                    duration_ms,
                });
            }
        }
        seen.insert(run.id.clone(), run.status.clone());
    }

    *initialized = true;

    // Prune stale entries to prevent unbounded growth
    let current_ids: std::collections::HashSet<&str> =
        runs.iter().map(|(_, r)| r.id.as_str()).collect();
    seen.retain(|id, _| current_ids.contains(id.as_str()));

    transitions
}
