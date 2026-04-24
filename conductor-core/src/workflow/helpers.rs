use crate::workflow_dsl::{WhileNode, WorkflowNode};

use super::action_executor::{ActionParams, ExecutionContext};
use super::engine::ExecutionState;
use super::status::WorkflowStepStatus;

pub(super) fn load_agent_and_build_prompt(
    ectx: &ExecutionContext,
    params: &ActionParams,
) -> crate::error::Result<(crate::agent_config::AgentDef, String)> {
    let working_dir_str = ectx.working_dir.to_string_lossy();
    let agent_def = crate::agent_config::load_agent(
        &working_dir_str,
        &ectx.repo_path,
        &crate::agent_config::AgentSpec::Name(params.name.clone()),
        Some(&ectx.workflow_name),
        &ectx.plugin_dirs,
    )?;
    let prompt =
        crate::workflow::prompt_builder::build_agent_prompt_from_params(&agent_def, params);
    Ok((agent_def, prompt))
}

/// Build a human-readable summary of a workflow execution.
pub(super) fn build_workflow_summary(state: &ExecutionState<'_>) -> String {
    let steps = state
        .wf_mgr
        .get_workflow_steps(&state.workflow_run_id)
        .unwrap_or_default();

    let total = steps.len();
    let count_status =
        |status: WorkflowStepStatus| steps.iter().filter(|s| s.status == status).count();
    let completed = count_status(WorkflowStepStatus::Completed);
    let failed = count_status(WorkflowStepStatus::Failed);
    let skipped = count_status(WorkflowStepStatus::Skipped);
    let timed_out = count_status(WorkflowStepStatus::TimedOut);

    let mut lines = Vec::new();
    lines.push(format!(
        "Workflow '{}': {completed}/{total} steps completed{}{}{}",
        state.workflow_name,
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        },
        if skipped > 0 {
            format!(", {skipped} skipped")
        } else {
            String::new()
        },
        if timed_out > 0 {
            format!(", {timed_out} timed out")
        } else {
            String::new()
        },
    ));

    for step in &steps {
        let marker = step.status.short_label();
        let iter_label = if step.iteration > 0 {
            format!(" (iter {})", step.iteration)
        } else {
            String::new()
        };
        let never_executed = step.status == WorkflowStepStatus::Failed && step.started_at.is_none();
        let step_note = if never_executed {
            " (never executed)"
        } else {
            ""
        };
        lines.push(format!(
            "  [{marker}] {}{iter_label}{step_note}",
            step.step_name
        ));
    }

    if state.all_succeeded {
        lines.push("Status: SUCCESS".to_string());
    } else {
        lines.push("Status: FAILED".to_string());
    }

    lines.join("\n")
}

/// Sanitize a string for use as a tmux window name.
/// Removes characters that tmux treats specially (`.`, `:`, `\`).
#[cfg(test)]
pub(super) fn sanitize_tmux_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '.' | ':' | '\\' | '\'' | '"' => '-',
            c if c.is_ascii_control() => '-',
            _ => c,
        })
        .collect()
}

/// Extract all leaf-node step keys from a workflow node.
///
/// Recurses into control-flow nodes (if/unless/while/always) and collects
/// keys from all trackable leaves: `Call`, `Parallel` agents, `Gate`, and
/// `CallWorkflow`.
pub(super) fn collect_leaf_step_keys(node: &WorkflowNode) -> Vec<String> {
    match node {
        WorkflowNode::Call(c) => vec![c.agent.step_key()],
        WorkflowNode::Parallel(p) => p.calls.iter().map(|a| a.step_key()).collect(),
        WorkflowNode::Gate(g) => vec![g.name.clone()],
        WorkflowNode::CallWorkflow(cw) => vec![format!("workflow:{}", cw.workflow)],
        WorkflowNode::Script(s) => vec![s.name.clone()],
        WorkflowNode::If(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Unless(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::While(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::DoWhile(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Do(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::Always(n) => n.body.iter().flat_map(collect_leaf_step_keys).collect(),
        WorkflowNode::ForEach(n) => vec![format!("foreach:{}", n.name)],
    }
}

/// Find the starting iteration for a while loop on resume.
///
/// Looks at the skip_completed set for step keys that match body nodes of the
/// while loop. Returns the max iteration that has all body nodes completed,
/// so the loop resumes from the iteration where it failed.
pub(super) fn find_max_completed_while_iteration(
    state: &ExecutionState<'_>,
    node: &WhileNode,
) -> u32 {
    let skip_set = match state.resume_ctx {
        Some(ref ctx) => &ctx.skip_completed,
        None => return 0,
    };

    // Collect step keys from all trackable body nodes
    let body_keys: Vec<String> = node.body.iter().flat_map(collect_leaf_step_keys).collect();

    if body_keys.is_empty() {
        return 0;
    }

    // Find the highest iteration where all body nodes are completed
    let mut iter = 0u32;
    loop {
        let all_done = body_keys
            .iter()
            .all(|k| skip_set.contains(&(k.clone(), iter)));
        if !all_done {
            break;
        }
        iter += 1;
    }
    // iter is now the first incomplete iteration — start there
    iter
}
