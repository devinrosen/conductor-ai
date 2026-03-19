use std::collections::HashMap;

use crate::schema_config;

use super::constants::CONDUCTOR_OUTPUT_INSTRUCTION;
use super::engine::ExecutionState;

/// Replace `{{key}}` placeholders in a prompt with values from `vars`.
pub(super) fn substitute_variables(prompt: &str, vars: &HashMap<&str, String>) -> String {
    let mut result = prompt.to_string();
    for (key, value) in vars {
        let pattern = format!("{{{{{key}}}}}");
        result = result.replace(&pattern, value);
    }
    result
}

/// Build the variable map from execution state (used for substitution in sub-workflow inputs).
pub(super) fn build_variable_map<'a>(state: &'a ExecutionState<'_>) -> HashMap<&'a str, String> {
    let mut vars: HashMap<&str, String> = HashMap::new();
    for (k, v) in &state.inputs {
        vars.insert(k.as_str(), v.clone());
    }
    let prior_context = state
        .contexts
        .last()
        .map(|c| c.context.clone())
        .unwrap_or_default();
    vars.insert("prior_context", prior_context);
    let prior_contexts_json = serde_json::to_string(&state.contexts).unwrap_or_default();
    vars.insert("prior_contexts", prior_contexts_json);
    if let Some(ref feedback) = state.last_gate_feedback {
        vars.insert("gate_feedback", feedback.clone());
    }
    // prior_output: raw JSON from the last step's structured output (if any)
    if let Some(last_output) = state
        .contexts
        .iter()
        .rev()
        .find_map(|c| c.structured_output.as_ref())
    {
        vars.insert("prior_output", last_output.clone());
    }
    // prior_output_file: path to the last script step's stdout temp file (if any)
    if let Some(path) = state
        .contexts
        .iter()
        .rev()
        .find_map(|c| c.output_file.as_ref())
    {
        vars.insert("prior_output_file", path.clone());
    }
    // dry_run: "true" or "false" — lets non-committing agents skip GitHub side effects
    vars.insert("dry_run", state.exec_config.dry_run.to_string());
    vars
}

/// Build a fully-substituted agent prompt from the execution state and agent definition.
///
/// Handles: input variables, prior_context, prior_contexts, prior_output,
/// gate_feedback, dry-run prefix for committing agents, prompt snippets (via
/// `with`), and CONDUCTOR_OUTPUT instruction (generic or schema-specific).
///
/// Prompt composition order:
/// 1. Agent .md body (with variable substitution)
/// 2. `with` prompt snippets (with variable substitution)
/// 3. Schema output instructions / CONDUCTOR_OUTPUT
pub(super) fn build_agent_prompt(
    state: &ExecutionState<'_>,
    agent_def: &crate::agent_config::AgentDef,
    schema: Option<&schema_config::OutputSchema>,
    snippet_text: &str,
) -> String {
    let vars = build_variable_map(state);
    let mut prompt = substitute_variables(&agent_def.prompt, &vars);

    if agent_def.can_commit && state.exec_config.dry_run {
        prompt = format!("DO NOT commit or push any changes. This is a dry run.\n\n{prompt}");
    }

    // Append prompt snippets (already concatenated by caller)
    if !snippet_text.is_empty() {
        let substituted = substitute_variables(snippet_text, &vars);
        prompt.push_str("\n\n");
        prompt.push_str(&substituted);
    }

    // Append output instructions: schema-specific if a schema is provided,
    // otherwise the generic CONDUCTOR_OUTPUT instruction.
    match schema {
        Some(s) => {
            prompt.push('\n');
            prompt.push_str(&schema_config::generate_prompt_instructions(s));
        }
        None => {
            prompt.push_str(CONDUCTOR_OUTPUT_INSTRUCTION);
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::types::{ContextEntry, WorkflowExecConfig};

    fn make_state(conn: &rusqlite::Connection) -> ExecutionState<'_> {
        let config = crate::config::Config::default();
        // Use a leaked config so the borrow lives long enough for the test.
        let config: &'static crate::config::Config = Box::leak(Box::new(config));
        ExecutionState {
            conn,
            config,
            workflow_run_id: String::new(),
            workflow_name: "test-wf".into(),
            worktree_id: None,
            working_dir: String::new(),
            worktree_slug: String::new(),
            repo_path: String::new(),
            ticket_id: None,
            repo_id: None,
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: std::collections::HashMap::new(),
            agent_mgr: crate::agent::AgentManager::new(conn),
            wf_mgr: crate::workflow::manager::WorkflowManager::new(conn),
            parent_run_id: String::new(),
            depth: 0,
            target_label: None,
            step_results: std::collections::HashMap::new(),
            contexts: Vec::new(),
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
            feature_id: None,
            triggered_by_hook: false,
        }
    }

    fn make_entry(step: &str, output_file: Option<&str>) -> ContextEntry {
        ContextEntry {
            step: step.to_string(),
            iteration: 0,
            context: String::new(),
            markers: Vec::new(),
            structured_output: None,
            output_file: output_file.map(str::to_string),
        }
    }

    #[test]
    fn test_prior_output_file_absent_when_no_entry_has_file() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state(&conn);
        state.contexts.push(make_entry("step-a", None));
        state.contexts.push(make_entry("step-b", None));
        let vars = build_variable_map(&state);
        assert!(!vars.contains_key("prior_output_file"));
    }

    #[test]
    fn test_prior_output_file_resolved_from_context_entry() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state(&conn);
        state
            .contexts
            .push(make_entry("script-step", Some("/tmp/out.txt")));
        let vars = build_variable_map(&state);
        assert_eq!(
            vars.get("prior_output_file").map(String::as_str),
            Some("/tmp/out.txt")
        );
    }

    #[test]
    fn test_prior_output_file_picks_most_recent_entry() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state(&conn);
        state
            .contexts
            .push(make_entry("step-1", Some("/tmp/first.txt")));
        state.contexts.push(make_entry("step-2", None));
        state
            .contexts
            .push(make_entry("step-3", Some("/tmp/last.txt")));
        let vars = build_variable_map(&state);
        assert_eq!(
            vars.get("prior_output_file").map(String::as_str),
            Some("/tmp/last.txt"),
        );
    }

    #[test]
    fn test_prior_output_file_skips_none_entries_to_find_earlier_file() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state(&conn);
        state
            .contexts
            .push(make_entry("step-1", Some("/tmp/first.txt")));
        state.contexts.push(make_entry("step-2", None));
        let vars = build_variable_map(&state);
        assert_eq!(
            vars.get("prior_output_file").map(String::as_str),
            Some("/tmp/first.txt"),
        );
    }
}
