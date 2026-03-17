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
