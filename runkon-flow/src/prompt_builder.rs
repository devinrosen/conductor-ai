use std::collections::HashMap;

use crate::engine::ExecutionState;
use crate::engine::ENGINE_INJECTED_KEYS;

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
        // Strip any remaining unresolved {{…}} placeholders
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
pub fn substitute_variables(prompt: &str, vars: &HashMap<&str, String>) -> String {
    substitute_variables_impl(prompt, vars, true)
}

/// For data contexts (env vars, sub-workflow inputs): substitutes variables but
/// preserves any `{{…}}` text that was not a template variable.
pub fn substitute_variables_keep_literal(template: &str, vars: &HashMap<&str, String>) -> String {
    substitute_variables_impl(template, vars, false)
}

/// Build the variable map from execution state (used for substitution in sub-workflow inputs).
pub fn build_variable_map(state: &ExecutionState) -> HashMap<&str, String> {
    let mut vars: HashMap<&str, String> = HashMap::new();

    // Non-injected user-defined inputs
    for (k, v) in &state.inputs {
        if !ENGINE_INJECTED_KEYS.contains(&k.as_str()) {
            vars.insert(k.as_str(), v.clone());
        }
    }

    // Engine-injected variables from the worktree context
    let wt = &state.worktree_ctx;
    if let Some(ref tid) = wt.ticket_id {
        vars.insert("ticket_id", tid.clone());
    }
    if let Some(ref rid) = wt.repo_id {
        vars.insert("repo_id", rid.clone());
    }
    vars.insert("repo_path", wt.repo_path.clone());
    vars.insert("workflow_run_id", state.workflow_run_id.clone());

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
    // dry_run: "true" or "false"
    vars.insert("dry_run", state.exec_config.dry_run.to_string());
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_strips_unresolved() {
        let vars = HashMap::new();
        let result = substitute_variables("hello {{unknown}}", &vars);
        assert_eq!(result, "hello ");
    }

    #[test]
    fn substitute_resolves_known_strips_unknown() {
        let mut vars = HashMap::new();
        vars.insert("name", "world".to_string());
        let result = substitute_variables("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and ");
    }

    #[test]
    fn substitute_keep_literal_preserves_unresolved() {
        let mut vars = HashMap::new();
        vars.insert("name", "world".to_string());
        let result = substitute_variables_keep_literal("hello {{name}} and {{unknown}}", &vars);
        assert_eq!(result, "hello world and {{unknown}}");
    }

    #[test]
    fn substitute_keep_literal_preserves_embedded_json() {
        let json_value = r#"{"risks":["{{deterministic-review.score}}","other"]}"#.to_string();
        let mut vars = HashMap::new();
        vars.insert("prior_output", json_value);
        let result = substitute_variables_keep_literal("{{prior_output}}", &vars);
        assert_eq!(
            result,
            r#"{"risks":["{{deterministic-review.score}}","other"]}"#
        );
    }
}
