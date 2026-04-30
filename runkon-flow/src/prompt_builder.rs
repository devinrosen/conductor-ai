use std::collections::HashMap;

use crate::engine::{ExecutionState, ENGINE_INJECTED_KEYS};

fn substitute_variables_impl(
    template: &str,
    vars: &HashMap<&str, String>,
    strip_unresolved: bool,
) -> String {
    // Single-pass tokeniser: scan the original template once, emitting each
    // {{key}} replacement exactly once.  This prevents double-substitution —
    // a replaced value containing {{other}} is written verbatim and never
    // re-scanned, so injected placeholder text cannot escape shell quoting.
    let mut out = String::with_capacity(template.len());
    let mut pos = 0;
    let bytes = template.as_bytes();
    while pos < bytes.len() {
        if bytes[pos..].starts_with(b"{{") {
            if let Some(end_rel) = template[pos + 2..].find("}}") {
                let key = &template[pos + 2..pos + 2 + end_rel];
                if let Some(value) = vars.get(key) {
                    out.push_str(value);
                } else if !strip_unresolved {
                    // Preserve unresolved placeholders literally.
                    out.push_str(&template[pos..pos + 2 + end_rel + 2]);
                }
                pos += 2 + end_rel + 2;
            } else {
                // No closing `}}` — copy the rest verbatim.
                out.push_str(&template[pos..]);
                pos = bytes.len();
            }
        } else {
            // Find the next `{{` and copy everything before it.
            let next = template[pos..]
                .find("{{")
                .map(|i| pos + i)
                .unwrap_or(bytes.len());
            out.push_str(&template[pos..next]);
            pos = next;
        }
    }
    out
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

/// POSIX sh single-quote escape a value so it cannot break out of a shell command.
///
/// Wraps `s` in single quotes and replaces embedded `'` with `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    let prior_contexts_json = if state.contexts.is_empty() {
        "[]".to_string()
    } else {
        crate::helpers::serialize_or_empty_array(
            &state.contexts,
            "build_variable_map:prior_contexts",
        )
    };
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

    // {{base_branch}}: pre-resolved PR base branch from a `resolve-pr-base.sh`
    // script step (or any step that emits `base_branch: "<branch>"` in its
    // FLOW_OUTPUT). #2736 — agents and detect-* scripts read this instead of
    // running `gh pr view` themselves, which is brittle when the agent cd's
    // out of the worktree and the silent fallback diffs against the wrong base.
    //
    // Walk forward through prior contexts; later writes overwrite earlier
    // ones with the same name.
    for c in &state.contexts {
        if let Some(json) = &c.structured_output {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json) {
                if let Some(s) = parsed.get("base_branch").and_then(|v| v.as_str()) {
                    vars.insert("base_branch", s.to_string());
                }
            }
        }
    }

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

    #[test]
    fn substitute_no_double_substitution() {
        // If variable A's value contains {{B}}, B must not be expanded in the output.
        let mut vars = HashMap::new();
        vars.insert("a", "{{b}}".to_string());
        vars.insert("b", "injected".to_string());
        let result = substitute_variables_keep_literal("{{a}}", &vars);
        // Should emit the literal value of a, not expand {{b}} inside it.
        assert_eq!(result, "{{b}}");
    }

    #[test]
    fn shell_quote_no_double_substitution() {
        // Simulates the shell-quoting path used in script execution:
        // a shell-safe var map is built then substituted into the run template.
        let mut vars = HashMap::new();
        vars.insert("cmd", "'{{evil}}'".to_string()); // already shell-quoted value
        vars.insert("evil", ";rm -rf /".to_string());
        // The run template only references {{cmd}}; {{evil}} should not be expanded.
        let result = substitute_variables("run {{cmd}}", &vars);
        assert_eq!(result, "run '{{evil}}'");
    }

    /// `build_variable_map` exposes `{{base_branch}}` from any prior step's
    /// structured_output JSON containing a top-level `base_branch` string.
    /// #2736 — `resolve-pr-base.sh` writes this once at the start of
    /// review-pr.wf and downstream consumers read it without re-running gh.
    #[test]
    fn build_variable_map_exposes_base_branch_from_prior_context() {
        use crate::test_helpers::CountingPersistence;
        use std::sync::Arc;

        let cp = Arc::new(CountingPersistence::new());
        let mut state = crate::test_helpers::make_test_execution_state(
            cp as Arc<dyn crate::traits::persistence::WorkflowPersistence>,
            "run-1".into(),
        );

        // No prior context → {{base_branch}} should be unset.
        let vars = build_variable_map(&state);
        assert!(
            !vars.contains_key("base_branch"),
            "no prior step → no base_branch variable"
        );

        // A prior step with structured_output carrying base_branch → exposed.
        state.contexts.push(crate::types::ContextEntry {
            step: "resolve-pr-base".into(),
            iteration: 0,
            context: "release/0.10.0".into(),
            markers: vec!["base_branch_resolved".into()],
            structured_output: Some(
                r#"{"markers":["base_branch_resolved"],"context":"release/0.10.0","base_branch":"release/0.10.0"}"#
                    .into(),
            ),
            output_file: None,
        });
        let vars = build_variable_map(&state);
        assert_eq!(
            vars.get("base_branch").map(String::as_str),
            Some("release/0.10.0"),
            "base_branch must be exposed from prior structured_output"
        );

        // A later step with no base_branch → previous value persists.
        state.contexts.push(crate::types::ContextEntry {
            step: "detect-file-types".into(),
            iteration: 0,
            context: "code changes".into(),
            markers: vec![],
            structured_output: Some(r#"{"markers":[],"context":"Found 2 files"}"#.into()),
            output_file: None,
        });
        let vars = build_variable_map(&state);
        assert_eq!(
            vars.get("base_branch").map(String::as_str),
            Some("release/0.10.0"),
            "later step without base_branch must not clobber the value"
        );

        // A later step that overwrites base_branch → wins.
        state.contexts.push(crate::types::ContextEntry {
            step: "override".into(),
            iteration: 0,
            context: "main".into(),
            markers: vec![],
            structured_output: Some(
                r#"{"markers":[],"context":"main","base_branch":"main"}"#.into(),
            ),
            output_file: None,
        });
        let vars = build_variable_map(&state);
        assert_eq!(
            vars.get("base_branch").map(String::as_str),
            Some("main"),
            "later step with base_branch must overwrite earlier value"
        );
    }

    /// Substitution: a template referencing {{base_branch}} resolves to the
    /// value exposed by `build_variable_map`. End-to-end verification that the
    /// engine variable injection works for the new variable.
    #[test]
    fn substitute_uses_base_branch_from_variable_map() {
        use crate::test_helpers::CountingPersistence;
        use std::sync::Arc;

        let cp = Arc::new(CountingPersistence::new());
        let mut state = crate::test_helpers::make_test_execution_state(
            cp as Arc<dyn crate::traits::persistence::WorkflowPersistence>,
            "run-1".into(),
        );
        state.contexts.push(crate::types::ContextEntry {
            step: "resolve-pr-base".into(),
            iteration: 0,
            context: "release/0.10.0".into(),
            markers: vec![],
            structured_output: Some(
                r#"{"markers":[],"context":"release/0.10.0","base_branch":"release/0.10.0"}"#
                    .into(),
            ),
            output_file: None,
        });

        let vars = build_variable_map(&state);
        let rendered = substitute_variables("git diff origin/{{base_branch}}...HEAD", &vars);
        assert_eq!(rendered, "git diff origin/release/0.10.0...HEAD");
    }
}
