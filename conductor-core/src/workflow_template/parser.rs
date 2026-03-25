use crate::error::{ConductorError, Result};
use crate::workflow_dsl::parse_workflow_str;

use super::types::{TemplateFrontmatter, WorkflowTemplate};

/// Parse a `.wft` template string into a `WorkflowTemplate`.
///
/// Format:
/// ```text
/// ---
/// name: my-template
/// description: Does a thing
/// version: "1.0.0"
/// target_types: [repo]
/// hints:
///   - Consider the repo's label taxonomy
/// ---
/// workflow my_workflow {
///     ...
/// }
/// ```
///
/// The body is validated via `parse_workflow_str` to catch syntax errors at
/// template parse time rather than instantiation time.
pub fn parse_wft(input: &str, source: &str) -> Result<WorkflowTemplate> {
    // Split on `---` delimiters
    let trimmed = input.trim_start();
    if !trimmed.starts_with("---") {
        return Err(ConductorError::Workflow(format!(
            "Template {source}: missing opening `---` frontmatter delimiter"
        )));
    }

    let after_open = &trimmed[3..];
    let close_pos = after_open.find("\n---").ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Template {source}: missing closing `---` frontmatter delimiter"
        ))
    })?;

    let yaml_str = &after_open[..close_pos];
    let body_start = close_pos + 4; // skip "\n---"
    let body = after_open[body_start..].trim().to_string();

    if body.is_empty() {
        return Err(ConductorError::Workflow(format!(
            "Template {source}: empty workflow body after frontmatter"
        )));
    }

    // Parse frontmatter
    let metadata: TemplateFrontmatter = serde_yml::from_str(yaml_str).map_err(|e| {
        ConductorError::Workflow(format!("Template {source}: invalid frontmatter YAML: {e}"))
    })?;

    // Validate the .wf body
    parse_workflow_str(&body, source)?;

    Ok(WorkflowTemplate { metadata, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_wft() -> String {
        r#"---
name: test-template
description: A test template
version: "1.0.0"
target_types:
  - repo
hints:
  - Check labels
---
workflow test {
    meta { description = "test" trigger = "manual" targets = ["repo"] }
    call agent
}
"#
        .to_string()
    }

    #[test]
    fn test_parse_valid_wft() {
        let result = parse_wft(&valid_wft(), "test.wft");
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        let tmpl = result.unwrap();
        assert_eq!(tmpl.metadata.name, "test-template");
        assert_eq!(tmpl.metadata.description, "A test template");
        assert_eq!(tmpl.metadata.version, "1.0.0");
        assert_eq!(tmpl.metadata.target_types, vec!["repo"]);
        assert_eq!(tmpl.metadata.hints, vec!["Check labels"]);
        assert!(tmpl.body.contains("workflow test"));
    }

    #[test]
    fn test_parse_missing_open_delimiter() {
        let result = parse_wft("no frontmatter here", "test.wft");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing opening `---`"), "got: {err}");
    }

    #[test]
    fn test_parse_missing_close_delimiter() {
        let input = "---\nname: x\n";
        let result = parse_wft(input, "test.wft");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing closing `---`"), "got: {err}");
    }

    #[test]
    fn test_parse_empty_body() {
        let input = "---\nname: x\ndescription: x\nversion: \"1.0.0\"\n---\n";
        let result = parse_wft(input, "test.wft");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("empty workflow body"), "got: {err}");
    }

    #[test]
    fn test_parse_invalid_yaml() {
        let input = "---\n[invalid yaml\n---\nworkflow x { meta { description = \"x\" trigger = \"manual\" } call a }";
        let result = parse_wft(input, "test.wft");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid frontmatter YAML"), "got: {err}");
    }

    #[test]
    fn test_parse_invalid_wf_body() {
        let input = r#"---
name: bad
description: bad body
version: "1.0.0"
---
this is not valid wf syntax
"#;
        let result = parse_wft(input, "test.wft");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_optional_fields_default() {
        let input = r#"---
name: minimal
description: minimal template
version: "1.0.0"
---
workflow minimal {
    meta { description = "minimal" trigger = "manual" targets = ["repo"] }
    call agent
}
"#;
        let result = parse_wft(input, "test.wft");
        assert!(result.is_ok());
        let tmpl = result.unwrap();
        assert!(tmpl.metadata.target_types.is_empty());
        assert!(tmpl.metadata.hints.is_empty());
    }
}
