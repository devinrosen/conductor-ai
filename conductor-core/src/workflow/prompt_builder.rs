use std::collections::HashMap;

use crate::schema_config;

use super::FLOW_OUTPUT_INSTRUCTION;

fn substitute_variables_impl(
    template: &str,
    vars: &HashMap<&str, &str>,
    strip_unresolved: bool,
) -> String {
    // Single-pass scan: one output allocation regardless of variable count.
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(open) = remaining.find("{{") {
        result.push_str(&remaining[..open]);
        remaining = &remaining[open + 2..];
        if let Some(close) = remaining.find("}}") {
            let key = &remaining[..close];
            remaining = &remaining[close + 2..];
            if let Some(val) = vars.get(key) {
                result.push_str(val);
            } else if !strip_unresolved {
                result.push_str("{{");
                result.push_str(key);
                result.push_str("}}");
            }
            // strip_unresolved: just drop the placeholder — push nothing
        } else {
            // Unclosed `{{` — emit it literally and stop scanning.
            result.push_str("{{");
            break;
        }
    }
    result.push_str(remaining);
    result
}

/// For agent prompts: substitutes variables AND strips unresolved `{{…}}` placeholders.
pub(super) fn substitute_variables(prompt: &str, vars: &HashMap<&str, &str>) -> String {
    substitute_variables_impl(prompt, vars, true)
}

/// For data contexts: substitutes variables but preserves any `{{…}}` text that was not a variable.
#[allow(dead_code)]
pub(super) fn substitute_variables_keep_literal(
    template: &str,
    vars: &HashMap<&str, &str>,
) -> String {
    substitute_variables_impl(template, vars, false)
}

fn build_prompt_core(
    agent_def: &crate::agent_config::AgentDef,
    vars: &HashMap<&str, &str>,
    schema: Option<&schema_config::OutputSchema>,
    snippets: &[&str],
    retry_error: Option<&str>,
    dry_run: bool,
) -> String {
    let substituted = substitute_variables(&agent_def.prompt, vars);
    let mut prompt = String::with_capacity(substituted.len() * 2);

    if dry_run {
        prompt.push_str("DO NOT commit or push any changes. This is a dry run.\n\n");
    }
    if let Some(msg) = retry_error {
        prompt.push_str("[Previous attempt failed]\nError: ");
        prompt.push_str(msg);
        prompt.push_str("\nPlease re-read the instructions below and correct your output.\n\n");
    }
    prompt.push_str("Your task below is your ONLY priority. Complete it fully before considering anything else.\n\n");
    prompt.push_str(&substituted);

    if let Some(fsm_path) = vars.get("fsm_path") {
        if !fsm_path.is_empty() {
            prompt.push_str("\n\n## Mandatory First Action\n\n");
            prompt.push_str("Before doing ANYTHING else, read the FSM definition file at:\n");
            prompt.push('`');
            prompt.push_str(fsm_path);
            prompt.push_str("`\n\n");
            prompt.push_str(
                "This file defines the state machine that governs your behavior in this workflow. ",
            );
            prompt
                .push_str("You MUST read and understand it before proceeding with any other work.");
        }
    }

    if !vars.is_empty() {
        prompt.push_str("\n\n## Template Variables\n\n");
        prompt.push_str(
            "The following template placeholders are available and have been substituted in this prompt:\n\n",
        );
        for (key, value) in vars {
            prompt.push_str("- `{{");
            prompt.push_str(key);
            prompt.push_str("}}` = `");
            prompt.push_str(value);
            prompt.push_str("`\n");
        }
    }

    for snippet in snippets {
        if !snippet.is_empty() {
            let substituted = substitute_variables(snippet, vars);
            prompt.push_str("\n\n");
            prompt.push_str(&substituted);
        }
    }

    match schema {
        Some(s) => {
            prompt.push('\n');
            prompt.push_str(&schema_config::generate_prompt_instructions(s));
        }
        None => {
            prompt.push_str(FLOW_OUTPUT_INSTRUCTION);
        }
    }
    prompt
}

/// Build a fully-substituted agent prompt from pre-resolved `ActionParams`.
///
/// Used by `ClaudeAgentExecutor` and `ApiCallExecutor` which have no access to `ExecutionState`.
pub(super) fn build_agent_prompt_from_params(
    agent_def: &crate::agent_config::AgentDef,
    params: &super::action_executor::ActionParams,
) -> String {
    let vars: HashMap<&str, &str> = params
        .inputs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let snippet_refs: Vec<&str> = params.snippets.iter().map(String::as_str).collect();
    build_prompt_core(
        agent_def,
        &vars,
        params.schema.as_ref(),
        &snippet_refs,
        params.retry_error.as_deref(),
        agent_def.can_commit && params.dry_run,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_variables_strips_unresolved_placeholders() {
        let vars = HashMap::new();
        let result = substitute_variables("hello {{unknown}}", &vars);
        assert_eq!(result, "hello ");
    }

    #[test]
    fn test_substitute_variables_resolves_known_strips_unknown() {
        let mut vars = HashMap::new();
        vars.insert("name", "world");
        let result = substitute_variables("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and ");
    }

    #[test]
    fn test_substitute_variables_keep_literal_preserves_json_braces() {
        let mut vars = HashMap::new();
        vars.insert("name", "world");
        let result = substitute_variables_keep_literal("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and {{unknown}}");
    }

    #[test]
    fn test_substitute_variables_multiple_unresolved() {
        let mut vars = HashMap::new();
        vars.insert("known", "X");
        let result = substitute_variables("{{known}} {{unk1}} {{unk2}}", &vars);
        assert_eq!(result, "X  ");
    }

    #[test]
    fn test_substitute_variables_embedded_json_in_value_not_reprocessed() {
        // Single-pass: {{...}} tokens inside a substituted value are NOT re-scanned.
        // This matters when agent prior_output itself contains template-like text.
        let mut vars = HashMap::new();
        vars.insert("prior_output", "result: {{some_json_key}}");
        let result = substitute_variables("Output: {{prior_output}}", &vars);
        assert_eq!(result, "Output: result: {{some_json_key}}");
    }
}
