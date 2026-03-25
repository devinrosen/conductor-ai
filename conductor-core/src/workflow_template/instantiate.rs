use super::types::WorkflowTemplate;

/// Result of building an instantiation prompt for the agent.
pub struct InstantiationPrompt {
    /// The full prompt to send to the agent.
    pub prompt: String,
    /// Suggested output filename (e.g. "create-issue.wf").
    pub suggested_filename: String,
}

/// Build a rich prompt that instructs an agent to customize a workflow template
/// for a specific repository.
///
/// The agent will inspect the repo, apply the template's hints, and write a
/// customized `.wf` file to `.conductor/workflows/`.
pub fn build_instantiation_prompt(
    template: &WorkflowTemplate,
    repo_path: &str,
    existing_workflows: &[String],
) -> InstantiationPrompt {
    let meta = &template.metadata;

    let existing_section = if existing_workflows.is_empty() {
        "No existing workflows found in this repo.".to_string()
    } else {
        format!(
            "Existing workflows in this repo:\n{}",
            existing_workflows
                .iter()
                .map(|w| format!("  - {w}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let hints_section = if meta.hints.is_empty() {
        String::new()
    } else {
        format!(
            "\n## Customization Hints\n\n{}\n",
            meta.hints
                .iter()
                .map(|h| format!("- {h}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let version_comment = format!("# Generated from template: {} v{}", meta.name, meta.version);

    let suggested_name = meta.name.replace(' ', "-").to_lowercase();

    let prompt = format!(
        r#"You are a workflow scaffolding agent for Conductor, a multi-repo orchestration tool.

## Your Task

Customize the following workflow template for the repository at `{repo_path}`.
Inspect the repo's structure, conventions, and existing configuration to produce
a tailored `.wf` workflow definition.

## Template: {name} (v{version})

{description}

## Template Body

```wf
{body}
```
{hints_section}
## Repo Context

{existing_section}

## Instructions

1. Read the template body above carefully.
2. Inspect the repository to understand its conventions (labels, team structure, CI config, etc.).
3. Customize the template by modifying agent names, descriptions, labels, and other parameters to fit this repo.
4. Write the customized workflow to `.conductor/workflows/{suggested_name}.wf`.
5. The first line of the output file MUST be this version comment:
   `{version_comment}`
6. The workflow MUST be valid Conductor DSL syntax.
7. Do NOT change the overall structure of the workflow — keep the same steps and flow.
8. DO customize string values, agent configurations, and parameters to match this repo's conventions.
9. If you have questions about the repo's conventions, ask via the feedback mechanism before writing the file.

## Output

Write the customized workflow file to: `.conductor/workflows/{suggested_name}.wf`
"#,
        repo_path = repo_path,
        name = meta.name,
        version = meta.version,
        description = meta.description,
        body = template.body,
        hints_section = hints_section,
        existing_section = existing_section,
        suggested_name = suggested_name,
        version_comment = version_comment,
    );

    InstantiationPrompt {
        prompt,
        suggested_filename: format!("{suggested_name}.wf"),
    }
}

/// Build an upgrade prompt that compares an existing workflow against a newer template version.
pub fn build_upgrade_prompt(
    template: &WorkflowTemplate,
    current_wf_content: &str,
    current_version: Option<&str>,
    _repo_path: &str,
) -> InstantiationPrompt {
    let meta = &template.metadata;
    let suggested_name = meta.name.replace(' ', "-").to_lowercase();

    let version_info = match current_version {
        Some(v) => format!("Current workflow was generated from template v{v}."),
        None => "Current workflow version is unknown (no version comment found).".to_string(),
    };

    let version_comment = format!("# Generated from template: {} v{}", meta.name, meta.version);

    let prompt = format!(
        r#"You are a workflow upgrade agent for Conductor, a multi-repo orchestration tool.

## Your Task

Upgrade an existing workflow to match the latest template version while preserving
user customizations.

## Template: {name} (v{version})

{description}

## New Template Body

```wf
{body}
```

## Current Workflow

{version_info}

```wf
{current_wf}
```

## Instructions

1. Compare the current workflow against the new template body.
2. Identify what changed in the template (new steps, modified parameters, etc.).
3. Apply template changes while preserving user customizations (custom agent names, labels, descriptions).
4. Write the upgraded workflow to `.conductor/workflows/{suggested_name}.wf`.
5. The first line MUST be: `{version_comment}`
6. The workflow MUST be valid Conductor DSL syntax.
7. If the user made significant customizations that conflict with template changes, ask via feedback before proceeding.

## Output

Write the upgraded workflow file to: `.conductor/workflows/{suggested_name}.wf`
"#,
        name = meta.name,
        version = meta.version,
        description = meta.description,
        body = template.body,
        version_info = version_info,
        current_wf = current_wf_content,
        suggested_name = suggested_name,
        version_comment = version_comment,
    );

    InstantiationPrompt {
        prompt,
        suggested_filename: format!("{suggested_name}.wf"),
    }
}

/// Extract the template version from a generated `.wf` file's version comment.
///
/// Looks for: `# Generated from template: <name> v<version>`
pub fn extract_template_version(wf_content: &str) -> Option<(&str, &str)> {
    for line in wf_content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# Generated from template: ") {
            if let Some(v_pos) = rest.rfind(" v") {
                let name = &rest[..v_pos];
                let version = &rest[v_pos + 2..];
                if !name.is_empty() && !version.is_empty() {
                    return Some((name, version));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_template::types::TemplateFrontmatter;

    fn make_template() -> WorkflowTemplate {
        WorkflowTemplate {
            metadata: TemplateFrontmatter {
                name: "create-issue".to_string(),
                description: "Creates GitHub issues".to_string(),
                version: "1.0.0".to_string(),
                target_types: vec!["repo".to_string()],
                hints: vec!["Consider label taxonomy".to_string()],
            },
            body: "workflow create_issue {\n    meta { description = \"create issue\" trigger = \"manual\" targets = [\"repo\"] }\n    call agent\n}".to_string(),
        }
    }

    #[test]
    fn test_build_instantiation_prompt() {
        let tmpl = make_template();
        let result = build_instantiation_prompt(&tmpl, "/path/to/repo", &["deploy".to_string()]);
        assert!(result.prompt.contains("create-issue"));
        assert!(result.prompt.contains("v1.0.0"));
        assert!(result.prompt.contains("/path/to/repo"));
        assert!(result.prompt.contains("deploy"));
        assert!(result.prompt.contains("Consider label taxonomy"));
        assert!(result
            .prompt
            .contains("# Generated from template: create-issue v1.0.0"));
        assert_eq!(result.suggested_filename, "create-issue.wf");
    }

    #[test]
    fn test_build_instantiation_prompt_no_existing() {
        let tmpl = make_template();
        let result = build_instantiation_prompt(&tmpl, "/repo", &[]);
        assert!(result.prompt.contains("No existing workflows found"));
    }

    #[test]
    fn test_build_upgrade_prompt() {
        let tmpl = make_template();
        let result = build_upgrade_prompt(&tmpl, "workflow old {}", Some("0.9.0"), "/repo");
        assert!(result.prompt.contains("v0.9.0"));
        assert!(result.prompt.contains("workflow old {}"));
        assert!(result.prompt.contains("Upgrade"));
    }

    #[test]
    fn test_extract_template_version() {
        let content = "# Generated from template: create-issue v1.0.0\nworkflow create_issue {}";
        let result = extract_template_version(content);
        assert_eq!(result, Some(("create-issue", "1.0.0")));
    }

    #[test]
    fn test_extract_template_version_missing() {
        let content = "workflow create_issue {}";
        assert!(extract_template_version(content).is_none());
    }

    #[test]
    fn test_extract_template_version_no_match() {
        let content = "# Some other comment\nworkflow x {}";
        assert!(extract_template_version(content).is_none());
    }
}
