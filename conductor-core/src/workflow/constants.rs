/// Column list for `workflow_run_steps` SELECT queries (used by `row_to_workflow_step`).
pub(super) const STEP_COLUMNS: &str =
    "id, workflow_run_id, step_name, role, can_commit, condition_expr, status, \
     child_run_id, position, started_at, ended_at, result_text, condition_met, \
     iteration, parallel_group_id, context_out, markers_out, retry_count, \
     gate_type, gate_prompt, gate_timeout, gate_approved_by, gate_approved_at, gate_feedback, \
     structured_output, output_file";

/// Table-prefixed variant of `STEP_COLUMNS` for JOIN queries where `s` aliases `workflow_run_steps`.
/// Use this when selecting step columns alongside columns from other tables to avoid ambiguity.
/// Derived from `STEP_COLUMNS` at first access to avoid duplication.
pub(super) static STEP_COLUMNS_WITH_PREFIX: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::db::prefix_columns(STEP_COLUMNS, "s."));

/// Column list for `workflow_runs` SELECT queries (used by `row_to_workflow_run`).
pub(super) const RUN_COLUMNS: &str =
    "id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, \
     started_at, ended_at, result_summary, definition_snapshot, inputs, ticket_id, repo_id, \
     parent_workflow_run_id, target_label, default_bot_name, iteration, blocked_on, feature_id";

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_columns_with_prefix_derivation() {
        assert_eq!(
            STEP_COLUMNS_WITH_PREFIX.as_str(),
            "s.id, s.workflow_run_id, s.step_name, s.role, s.can_commit, s.condition_expr, s.status, \
             s.child_run_id, s.position, s.started_at, s.ended_at, s.result_text, s.condition_met, \
             s.iteration, s.parallel_group_id, s.context_out, s.markers_out, s.retry_count, \
             s.gate_type, s.gate_prompt, s.gate_timeout, s.gate_approved_by, s.gate_approved_at, \
             s.gate_feedback, s.structured_output, s.output_file"
        );

        let cols: Vec<&str> = STEP_COLUMNS_WITH_PREFIX.split(", ").collect();

        // Verify column count parity.
        assert_eq!(STEP_COLUMNS.split(',').count(), cols.len());

        // Spot-check every entry is prefixed with "s.".
        for col in &cols {
            assert!(col.starts_with("s."), "column {col:?} missing 's.' prefix");
        }

        // Spot-check first and last known columns.
        assert_eq!(cols.first().copied(), Some("s.id"));
        assert_eq!(cols.last().copied(), Some("s.output_file"));
    }
}
