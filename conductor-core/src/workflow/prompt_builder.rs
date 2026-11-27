//! Placeholder substitution for host-side prompt text. Delegates to runkon's
//! rule so conductor and the engine agree: a `{{` is a placeholder only when the
//! text up to `}}` (spaces and tabs trimmed) is a variable name, `{{#if name}} …
//! {{else}} … {{/if}}` blocks are conditionals, and anything else is kept literally.
use std::collections::HashMap;

use runkon_flow::prompt_builder::{substitute_with, Unresolved};

fn substitute_variables_impl(
    template: &str,
    vars: &HashMap<&str, &str>,
    unresolved: Unresolved,
) -> String {
    substitute_with(
        template,
        |k| vars.get(k).copied(),
        |v, out| out.push_str(v),
        unresolved,
    )
}

/// For agent prompts: substitutes variables AND strips unresolved `{{name}}` placeholders.
#[allow(dead_code)]
pub(super) fn substitute_variables(prompt: &str, vars: &HashMap<&str, &str>) -> String {
    substitute_variables_impl(prompt, vars, Unresolved::Strip)
}

/// For data contexts: substitutes variables but keeps any `{{…}}` text that was not a resolvable variable.
#[allow(dead_code)]
pub(super) fn substitute_variables_keep_literal(
    template: &str,
    vars: &HashMap<&str, &str>,
) -> String {
    substitute_variables_impl(template, vars, Unresolved::KeepLiteral)
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

    #[test]
    fn literal_open_braces_survive_and_branch_substitutes() {
        // runkon#205 acceptance case.
        let mut vars = HashMap::new();
        vars.insert("feature_base_branch", "release/x");
        let t = "intro\nstill shows a literal `{{`, dry run.\nmore\nfetch {{feature_base_branch}} now\n";
        let want = "intro\nstill shows a literal `{{`, dry run.\nmore\nfetch release/x now\n";
        assert_eq!(substitute_variables(t, &vars), want);
        assert_eq!(substitute_variables_keep_literal(t, &vars), want);
    }

    #[test]
    fn spaced_name_resolves() {
        let mut vars = HashMap::new();
        vars.insert("name", "world");
        assert_eq!(substitute_variables("hi {{ name }}", &vars), "hi world");
    }

    #[test]
    fn if_else_blocks() {
        let mut vars = HashMap::new();
        vars.insert("x", "yes");
        vars.insert("empty", "");
        let t = "{{#if x}}A{{else}}B{{/if}}";
        assert_eq!(substitute_variables(t, &vars), "A");
        let t = "{{#if empty}}A{{else}}B{{/if}}";
        assert_eq!(substitute_variables(t, &vars), "B");
        let t = "{{#if unset}}A{{else}}B{{/if}}";
        assert_eq!(substitute_variables(t, &vars), "B");
    }

    #[test]
    fn non_names_are_kept_in_both_modes() {
        let vars = HashMap::new();
        let t = "a {{...}} b {{<gate>.feedback}} c";
        assert_eq!(substitute_variables(t, &vars), t);
        assert_eq!(substitute_variables_keep_literal(t, &vars), t);
    }
}
