use std::collections::HashMap;

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
#[allow(dead_code)]
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
