//! Placeholder detection shared by the validator and the engine.
//!
//! Since runkon 0.11 a `{{` is a placeholder only when the text up to `}}`
//! (spaces and tabs trimmed) is a variable name, so the validator uses runkon's
//! own `is_variable_name` and [`placeholder_names`] and cannot disagree with
//! the substitution. Anything else, such as a literal `{{` in prose, is kept in
//! the prompt, which makes the "runaway placeholder" warning (#3339) obsolete.

pub(crate) use runkon_flow::prompt_builder::placeholder_names;

/// Start of the system-event summary recorded for a prompt warning. `step-log`
/// reads it back to show warnings recorded on runs made before runkon 0.11.
pub(crate) const PROMPT_WARNING_PREFIX: &str = "Prompt warning:";

#[cfg(test)]
mod tests {
    use super::*;
    use runkon_flow::prompt_builder::is_variable_name;
    use std::collections::HashMap;

    #[test]
    fn spaced_name_is_listed_trimmed() {
        assert_eq!(placeholder_names("a {{ name }} b"), vec!["name"]);
    }

    #[test]
    fn conditional_tags_are_not_listed() {
        assert_eq!(
            placeholder_names("{{#if x}}{{y}}{{else}}z{{/if}}"),
            vec!["y"]
        );
        assert!(placeholder_names("{{#if x}}y{{/if}}").is_empty());
    }

    #[test]
    fn non_names_and_literals_are_not_listed() {
        assert!(placeholder_names("a `{{` b {{...}} c {{<g>.feedback}} d {{ open").is_empty());
    }

    #[test]
    fn variable_name_rule() {
        assert!(is_variable_name("a.b-c"));
        assert!(!is_variable_name("1a"));
        assert!(!is_variable_name(""));
    }

    #[test]
    fn matches_engine_substitution() {
        // Every name `placeholder_names` lists is replaced by substitution, and
        // nothing else is.
        let text = "a `{{` b\nc\nd {{x}} e {{ y }} f {{...}} g {{open";
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "X".to_string());
        vars.insert("y".to_string(), "Y".to_string());
        let rendered = runkon_flow::prompt_builder::substitute_variables(text, &vars);
        assert_eq!(placeholder_names(text), vec!["x", "y"]);
        assert_eq!(rendered, "a `{{` b\nc\nd X e Y f {{...}} g {{open");
    }
}
