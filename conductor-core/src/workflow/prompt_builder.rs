use std::collections::HashMap;

use crate::schema_config;

use super::constants::CONDUCTOR_OUTPUT_INSTRUCTION;

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

/// For data contexts: substitutes variables but preserves any `{{…}}` text that was not a variable.
#[allow(dead_code)]
pub(super) fn substitute_variables_keep_literal(
    template: &str,
    vars: &HashMap<&str, String>,
) -> String {
    substitute_variables_impl(template, vars, false)
}

fn build_prompt_core(
    agent_def: &crate::agent_config::AgentDef,
    vars: &HashMap<&str, String>,
    schema: Option<&schema_config::OutputSchema>,
    snippets: &[&str],
    retry_error: Option<&str>,
    dry_run: bool,
) -> String {
    let mut prompt = substitute_variables(&agent_def.prompt, vars);

    prompt = format!(
        "Your task below is your ONLY priority. Complete it fully before considering anything else.\n\n{prompt}"
    );

    if let Some(msg) = retry_error {
        prompt = format!(
            "[Previous attempt failed]\nError: {msg}\nPlease re-read the instructions below and correct your output.\n\n{prompt}"
        );
    }

    if dry_run {
        prompt = format!("DO NOT commit or push any changes. This is a dry run.\n\n{prompt}");
    }

    if let Some(fsm_path) = vars.get("fsm_path") {
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

    if !vars.is_empty() {
        prompt.push_str("\n\n## Template Variables\n\n");
        prompt.push_str(
            "The following template placeholders are available and have been substituted in this prompt:\n\n",
        );
        for (key, value) in vars {
            prompt.push_str(&format!("- `{{{{{key}}}}}` = `{value}`\n"));
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
            prompt.push_str(CONDUCTOR_OUTPUT_INSTRUCTION);
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
    let vars: HashMap<&str, String> = params
        .inputs
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
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
        vars.insert("name", "world".to_string());
        let result = substitute_variables("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and ");
    }

    #[test]
    fn test_substitute_variables_keep_literal_preserves_json_braces() {
        let mut vars = HashMap::new();
        vars.insert("name", "world".to_string());
        let result = substitute_variables_keep_literal("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and {{unknown}}");
    }
}
