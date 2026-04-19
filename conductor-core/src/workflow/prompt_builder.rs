use std::collections::HashMap;

use crate::schema_config;

use super::constants::CONDUCTOR_OUTPUT_INSTRUCTION;
use super::engine::{ExecutionState, ENGINE_INJECTED_KEYS};
use super::run_context::{RunContext, WorktreeRunContext};

fn substitute_variables_impl(
    template: &str,
    vars: &HashMap<&str, String>,
    strip_unresolved: bool,
) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        let pattern = format!("{{{{{key}}}}}");
        result = result.replace(&pattern, value);
    }
    if strip_unresolved {
        // Strip any remaining unresolved {{…}} placeholders so they don't
        // leak as literal text into agent prompts.
        while let Some(start) = result.find("{{") {
            if let Some(end) = result[start..].find("}}") {
                result.replace_range(start..start + end + 2, "");
            } else {
                break;
            }
        }
    }
    result
}

/// For agent prompts: substitutes variables AND strips unresolved `{{…}}` placeholders.
pub(super) fn substitute_variables(prompt: &str, vars: &HashMap<&str, String>) -> String {
    substitute_variables_impl(prompt, vars, true)
}

/// For data contexts (env vars, sub-workflow inputs): substitutes variables but
/// preserves any `{{…}}` text that was not a template variable.
pub(super) fn substitute_variables_keep_literal(
    template: &str,
    vars: &HashMap<&str, String>,
) -> String {
    substitute_variables_impl(template, vars, false)
}

/// Build the variable map from execution state (used for substitution in sub-workflow inputs).
pub(super) fn build_variable_map<'a>(state: &'a ExecutionState<'_>) -> HashMap<&'a str, String> {
    let mut vars: HashMap<&str, String> = HashMap::new();

    // Non-injected user-defined inputs (e.g. feature_base_branch, worktree_branch, fsm_path)
    for (k, v) in &state.inputs {
        if !ENGINE_INJECTED_KEYS.contains(&k.as_str()) {
            vars.insert(k.as_str(), v.clone());
        }
    }

    // ENGINE_INJECTED_KEYS read through the RunContext trait facade.
    // Keys in ENGINE_INJECTED_KEYS are &'static str so they satisfy the &'a str bound.
    let ctx = WorktreeRunContext::new(state);
    for (k, v) in ctx.injected_variables() {
        if let Some(&static_key) = ENGINE_INJECTED_KEYS.iter().find(|&&sk| sk == k.as_str()) {
            vars.insert(static_key, v);
        }
    }
    let prior_context = state
        .contexts
        .last()
        .map(|c| c.context.clone())
        .unwrap_or_default();
    vars.insert("prior_context", prior_context);
    let prior_contexts_json = serde_json::to_string(&state.contexts).unwrap_or_default();
    vars.insert("prior_contexts", prior_contexts_json);
    if let Some(ref gf) = state.last_gate_feedback {
        vars.insert("gate_feedback", gf.clone());
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
    retry_context: Option<&str>,
) -> String {
    let vars = build_variable_map(state);
    let mut prompt = substitute_variables(&agent_def.prompt, &vars);

    // Task reinforcement directive
    prompt = format!(
        "Your task below is your ONLY priority. Complete it fully before considering anything else.\n\n{prompt}"
    );

    // Retry failure preamble: prepended before the task reinforcement so the
    // agent sees it first when retrying after a failed attempt.
    if let Some(msg) = retry_context {
        prompt = format!(
            "[Previous attempt failed]\nError: {msg}\nPlease re-read the instructions below and correct your output.\n\n{prompt}"
        );
    }

    if agent_def.can_commit && state.exec_config.dry_run {
        prompt = format!("DO NOT commit or push any changes. This is a dry run.\n\n{prompt}");
    }

    // FSM mandatory first action: when an FSM path is provided, tell the
    // agent to read it before doing anything else.
    if let Some(fsm_path) = state.inputs.get("fsm_path") {
        if !fsm_path.is_empty() {
            prompt = format!(
                "{prompt}\n\n## Mandatory First Action\n\n\
                 Before doing ANYTHING else, read the FSM definition file at:\n\
                 `{fsm_path}`\n\n\
                 This file defines the state machine that governs your behavior in this workflow. \
                 You MUST read and understand it before proceeding with any other work."
            );
        }
    }

    // Template variables section — list ALL substituted variables, not just inputs
    if !vars.is_empty() {
        prompt.push_str("\n\n## Template Variables\n\n");
        prompt.push_str(
            "The following template placeholders are available and have been substituted in this prompt:\n\n",
        );
        for (key, value) in &vars {
            prompt.push_str(&format!("- `{{{{{key}}}}}` = `{value}`\n"));
        }
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
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: Vec::new(),
            resume_ctx: None,
            default_bot_name: None,
            triggered_by_hook: false,
            conductor_bin_dir: None,
            extra_plugin_dirs: vec![],
            last_heartbeat_at: ExecutionState::new_heartbeat(),
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
    fn test_substitute_variables_strips_unresolved_placeholders() {
        let vars = HashMap::new();
        let result = substitute_variables("hello {{unknown}}", &vars);
        assert_eq!(result, "hello ");
    }

    #[test]
    fn test_substitute_variables_resolves_known_strips_unknown() {
        let mut vars = HashMap::new();
        vars.insert("name", "world".to_string());
        let result = substitute_variables("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and ");
    }

    #[test]
    fn test_substitute_variables_multiple_unresolved() {
        let vars = HashMap::new();
        let result = substitute_variables("{{a}} middle {{b}} end {{c}}", &vars);
        assert_eq!(result, " middle  end ");
    }

    #[test]
    fn test_substitute_variables_keep_literal_preserves_json_braces() {
        let mut vars = HashMap::new();
        vars.insert("name", "world".to_string());
        let result = substitute_variables_keep_literal("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and {{unknown}}");
    }

    #[test]
    fn test_substitute_variables_keep_literal_preserves_embedded_json() {
        let json_value = r#"{"risks":["{{deterministic-review.score}}","other"]}"#.to_string();
        let mut vars = HashMap::new();
        vars.insert("prior_output", json_value);
        let result = substitute_variables_keep_literal("{{prior_output}}", &vars);
        assert_eq!(
            result,
            r#"{"risks":["{{deterministic-review.score}}","other"]}"#
        );
    }

    #[test]
    fn test_substitute_variables_strips_unresolved_for_prompts() {
        let mut vars = HashMap::new();
        vars.insert("name", "world".to_string());
        let result = substitute_variables("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and ");
    }

    #[test]
    fn test_build_agent_prompt_no_retry_context() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state(&conn);
        let agent_def = crate::agent_config::AgentDef {
            name: "test-agent".into(),
            prompt: "Do the thing.".into(),
            role: crate::agent_config::AgentRole::Actor,
            can_commit: false,
            model: None,
        };
        let result = build_agent_prompt(&state, &agent_def, None, "", None);
        assert!(
            !result.contains("[Previous attempt failed]"),
            "No retry preamble expected when retry_context is None"
        );
    }

    #[test]
    fn test_build_agent_prompt_with_retry_context() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state(&conn);
        let agent_def = crate::agent_config::AgentDef {
            name: "test-agent".into(),
            prompt: "Do the thing.".into(),
            role: crate::agent_config::AgentRole::Actor,
            can_commit: false,
            model: None,
        };
        let error_msg = "schema validation failed: missing field 'context'";
        let result = build_agent_prompt(&state, &agent_def, None, "", Some(error_msg));
        assert!(
            result.contains("[Previous attempt failed]"),
            "Retry preamble expected when retry_context is Some"
        );
        assert!(
            result.contains(error_msg),
            "Error message should appear in retry preamble"
        );
        assert!(
            result.contains("Please re-read the instructions below and correct your output."),
            "Correction instruction should appear in retry preamble"
        );
        // The preamble should appear before the task reinforcement line
        let preamble_pos = result.find("[Previous attempt failed]").unwrap();
        let reinforcement_pos = result
            .find("Your task below is your ONLY priority")
            .unwrap();
        assert!(
            preamble_pos < reinforcement_pos,
            "Retry preamble should appear before task reinforcement"
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
