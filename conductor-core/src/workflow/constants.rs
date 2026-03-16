/// Column list for `workflow_run_steps` SELECT queries (used by `row_to_workflow_step`).
pub(super) const STEP_COLUMNS: &str =
    "id, workflow_run_id, step_name, role, can_commit, condition_expr, status, \
     child_run_id, position, started_at, ended_at, result_text, condition_met, \
     iteration, parallel_group_id, context_out, markers_out, retry_count, \
     gate_type, gate_prompt, gate_timeout, gate_approved_by, gate_approved_at, gate_feedback, \
     structured_output, output_file";

/// Column list for `workflow_runs` SELECT queries (used by `row_to_workflow_run`).
pub(super) const RUN_COLUMNS: &str =
    "id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
     started_at, ended_at, result_summary, definition_snapshot, inputs, ticket_id, repo_id, \
     parent_workflow_run_id, target_label, default_bot_name";

/// Instruction appended to every agent prompt for structured output.
pub const CONDUCTOR_OUTPUT_INSTRUCTION: &str = r#"
When you have finished your work, output the following block exactly as the
last thing in your response. Do not include this block in code examples or
anywhere else — only as the final output.

<<<CONDUCTOR_OUTPUT>>>
{"markers": [], "context": ""}
<<<END_CONDUCTOR_OUTPUT>>>

markers: array of string signals consumed by the workflow engine
         (e.g. ["has_review_issues", "has_critical_issues"])
context: one or two sentence summary of what you did or found,
         passed to the next step as {{prior_context}}
"#;
