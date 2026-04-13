//! File-based workflow definitions for multi-step agent orchestration.
//!
//! Reads `.conductor/workflows/*.md` from the repo root (or worktree).
//! Each workflow file uses YAML frontmatter + sectioned markdown body.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::agent_config::{default_role, AgentRole};
use crate::error::{ConductorError, Result};
use crate::text_util::{parse_frontmatter, resolve_conductor_subdir_for_file};
use crate::workflow_dsl::WorkflowTrigger;

/// YAML frontmatter for a workflow `.md` file.
#[derive(Debug, Clone, Deserialize)]
struct WorkflowFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default = "default_trigger")]
    trigger: String,
    #[serde(default)]
    steps: Vec<StepFrontmatter>,
}

fn default_trigger() -> String {
    "manual".to_string()
}

/// A single step definition from the YAML frontmatter.
#[derive(Debug, Clone, Deserialize)]
struct StepFrontmatter {
    name: String,
    #[serde(default = "default_role")]
    role: String,
    /// Condition expression: evaluated against prior step outputs.
    /// Format: `step_name.field_name` — truthy check on the named field.
    #[serde(default)]
    condition: Option<String>,
    /// Whether this step can commit code back to a branch.
    #[serde(default)]
    can_commit: bool,
    /// Which markdown section contains the prompt for this step.
    /// Defaults to the step name.
    #[serde(default)]
    prompt_section: Option<String>,
    /// Model override for this step.
    #[serde(default)]
    model: Option<String>,
}

/// Re-export `AgentRole` as `WorkflowRole` for backward compatibility.
pub type WorkflowRole = AgentRole;

/// A parsed step within a workflow definition.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkflowStepDef {
    /// Step identifier (unique within the workflow).
    pub name: String,
    /// Role type: reviewer (read-only) or actor (can write/commit).
    pub role: WorkflowRole,
    /// Optional condition expression. If set, the step is skipped when
    /// the condition evaluates to false.
    pub condition: Option<String>,
    /// Whether this step is allowed to commit code.
    pub can_commit: bool,
    /// The prompt template for this step (from the markdown body).
    pub prompt: String,
    /// Optional model override.
    pub model: Option<String>,
}

/// A complete workflow definition parsed from a `.md` file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkflowDef {
    /// Short identifier, e.g. "test-coverage".
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// When this workflow should be triggered.
    pub trigger: WorkflowTrigger,
    /// Ordered list of steps to execute.
    pub steps: Vec<WorkflowStepDef>,
    /// Source file path (for display/debugging).
    pub source_path: String,
}

/// Parse a workflow `.md` file into a `WorkflowDef`.
fn parse_workflow_file(path: &Path) -> Result<WorkflowDef> {
    let content = fs::read_to_string(path).map_err(|e| {
        ConductorError::Workflow(format!(
            "Failed to read workflow file {}: {e}",
            path.display()
        ))
    })?;

    let (frontmatter, body) = parse_frontmatter(&content).ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Invalid frontmatter in workflow file {}. Expected YAML between --- delimiters.",
            path.display()
        ))
    })?;

    let fm: WorkflowFrontmatter = serde_yml::from_str(frontmatter).map_err(|e| {
        ConductorError::Workflow(format!(
            "Invalid YAML frontmatter in {}: {e}",
            path.display()
        ))
    })?;

    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    if fm.steps.is_empty() {
        return Err(ConductorError::Workflow(format!(
            "Workflow {} has no steps defined in frontmatter.",
            path.display()
        )));
    }

    // Parse markdown body into named sections (## headings).
    let sections = parse_sections(body);

    let trigger: WorkflowTrigger = fm
        .trigger
        .parse()
        .map_err(|e: String| ConductorError::Workflow(format!("In {}: {e}", path.display())))?;

    let mut steps = Vec::new();
    for step_fm in &fm.steps {
        let role: WorkflowRole = step_fm.role.parse().map_err(|e: String| {
            ConductorError::Workflow(format!(
                "In {} step '{}': {e}",
                path.display(),
                step_fm.name
            ))
        })?;

        // Actor steps with can_commit must have role=actor
        if step_fm.can_commit && role != WorkflowRole::Actor {
            return Err(ConductorError::Workflow(format!(
                "In {} step '{}': can_commit requires role: actor",
                path.display(),
                step_fm.name
            )));
        }

        // Resolve the prompt section
        let section_name = step_fm.prompt_section.as_deref().unwrap_or(&step_fm.name);

        let prompt = sections.get(section_name).cloned().unwrap_or_default();
        if prompt.is_empty() {
            return Err(ConductorError::Workflow(format!(
                "In {} step '{}': no markdown section '## {}' found in body.",
                path.display(),
                step_fm.name,
                section_name
            )));
        }

        steps.push(WorkflowStepDef {
            name: step_fm.name.clone(),
            role,
            condition: step_fm.condition.clone(),
            can_commit: step_fm.can_commit,
            prompt: prompt.trim().to_string(),
            model: step_fm.model.clone(),
        });
    }

    Ok(WorkflowDef {
        name: fm.name.unwrap_or_else(|| file_stem.clone()),
        description: fm.description.unwrap_or(file_stem),
        trigger,
        steps,
        source_path: path.to_string_lossy().to_string(),
    })
}

/// Parse markdown body into named sections keyed by `## heading`.
fn parse_sections(body: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            // Save previous section
            if let Some(name) = current_name.take() {
                sections.insert(name, current_lines.join("\n"));
            }
            current_name = Some(heading.trim().to_string());
            current_lines.clear();
        } else {
            current_lines.push(line);
        }
    }

    // Save last section
    if let Some(name) = current_name {
        sections.insert(name, current_lines.join("\n"));
    }

    sections
}

/// Load all workflow definitions from `.conductor/workflows/*.md`.
///
/// Merges definitions from both `repo_path` and `worktree_path`. Worktree
/// definitions override repo definitions when both define a workflow with
/// the same name (keyed by `def.name`, not filename).
pub fn load_workflow_defs(worktree_path: &str, repo_path: &str) -> Result<Vec<WorkflowDef>> {
    let mut map: HashMap<String, WorkflowDef> = HashMap::new();

    // Load repo defs first (lower priority).
    if !repo_path.is_empty() {
        let repo_dir = Path::new(repo_path).join(".conductor").join("workflows");
        if repo_dir.is_dir() {
            for def in scan_md_dir(&repo_dir)? {
                map.insert(def.name.clone(), def);
            }
        }
    }

    // Load worktree defs second (higher priority — overwrite repo defs on name conflict).
    // Guard: skip if worktree_path is empty or identical to repo_path (avoids double-counting).
    if !worktree_path.is_empty() && worktree_path != repo_path {
        let wt_dir = Path::new(worktree_path)
            .join(".conductor")
            .join("workflows");
        if wt_dir.is_dir() {
            for def in scan_md_dir(&wt_dir)? {
                map.insert(def.name.clone(), def);
            }
        }
    }

    let mut defs: Vec<WorkflowDef> = map.into_values().collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(defs)
}

/// Scan a single `.md` workflow directory and return parsed defs.
fn scan_md_dir(dir: &Path) -> Result<Vec<WorkflowDef>> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| ConductorError::Workflow(format!("Failed to read {}: {e}", dir.display())))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    entries
        .iter()
        .map(|entry| parse_workflow_file(&entry.path()))
        .collect()
}

/// Load a single workflow definition by name, targeting the file directly.
///
/// Looks for `<name>.md` in `.conductor/workflows/`, checking `worktree_path`
/// first then falling back to `repo_path`. Avoids parsing all workflow files.
pub fn load_workflow_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
) -> Result<WorkflowDef> {
    crate::workflow_dsl::validate_workflow_name(name)?;

    let filename = format!("{name}.md");
    let workflows_dir =
        resolve_conductor_subdir_for_file(worktree_path, repo_path, "workflows", &filename)
            .ok_or_else(|| {
                ConductorError::Workflow(format!(
                    "Workflow '{name}' not found in .conductor/workflows/"
                ))
            })?;

    parse_workflow_file(&workflows_dir.join(&filename))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_workflow_file(dir: &Path, name: &str, content: &str) {
        let workflows_dir = dir.join(".conductor").join("workflows");
        fs::create_dir_all(&workflows_dir).unwrap();
        fs::write(workflows_dir.join(name), content).unwrap();
    }

    const TEST_WORKFLOW: &str = "\
---
name: test-coverage
description: Validate PR has sufficient tests; write and commit missing ones
trigger: manual
steps:
  - name: analyze
    role: reviewer
    prompt_section: analyze
  - name: write-tests
    condition: analyze.has_missing_tests
    role: actor
    can_commit: true
    prompt_section: write
---

## analyze

You are a test coverage reviewer. Analyze the PR diff and identify any functions
or code paths that lack test coverage.

## write

You are a test engineer. Based on the analysis above, write the missing tests
and commit them to the branch.
";

    #[test]
    fn test_parse_sections() {
        let body = "## analyze\nYou are a reviewer.\n\n## write\nYou are a writer.";
        let sections = parse_sections(body);
        assert_eq!(sections.len(), 2);
        assert!(sections["analyze"].contains("You are a reviewer."));
        assert!(sections["write"].contains("You are a writer."));
    }

    #[test]
    fn test_parse_sections_empty() {
        let sections = parse_sections("no sections here");
        assert!(sections.is_empty());
    }

    #[test]
    fn test_parse_workflow_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test-coverage.md");
        fs::write(&file_path, TEST_WORKFLOW).unwrap();

        let def = parse_workflow_file(&file_path).unwrap();
        assert_eq!(def.name, "test-coverage");
        assert_eq!(def.trigger, WorkflowTrigger::Manual);
        assert_eq!(def.steps.len(), 2);

        assert_eq!(def.steps[0].name, "analyze");
        assert_eq!(def.steps[0].role, WorkflowRole::Reviewer);
        assert!(def.steps[0].condition.is_none());
        assert!(!def.steps[0].can_commit);
        assert!(def.steps[0].prompt.contains("test coverage reviewer"));

        assert_eq!(def.steps[1].name, "write-tests");
        assert_eq!(def.steps[1].role, WorkflowRole::Actor);
        assert_eq!(
            def.steps[1].condition.as_deref(),
            Some("analyze.has_missing_tests")
        );
        assert!(def.steps[1].can_commit);
        assert!(def.steps[1].prompt.contains("test engineer"));
    }

    #[test]
    fn test_parse_workflow_name_defaults_to_stem() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("lint-fix.md");
        fs::write(
            &file_path,
            "---\nsteps:\n  - name: fix\n    role: actor\n    can_commit: true\n---\n\n## fix\n\nFix lint errors.",
        )
        .unwrap();

        let def = parse_workflow_file(&file_path).unwrap();
        assert_eq!(def.name, "lint-fix");
        assert_eq!(def.trigger, WorkflowTrigger::Manual);
    }

    #[test]
    fn test_parse_workflow_no_steps_error() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("empty.md");
        fs::write(&file_path, "---\nname: empty\n---\n\n## body\n\nNo steps.").unwrap();

        let result = parse_workflow_file(&file_path);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("no steps"));
    }

    #[test]
    fn test_parse_workflow_missing_section_error() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("bad.md");
        fs::write(
            &file_path,
            "---\nsteps:\n  - name: analyze\n    prompt_section: nonexistent\n---\n\n## other\n\nWrong section.",
        )
        .unwrap();

        let result = parse_workflow_file(&file_path);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_can_commit_requires_actor_role() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("bad.md");
        fs::write(
            &file_path,
            "---\nsteps:\n  - name: review\n    role: reviewer\n    can_commit: true\n---\n\n## review\n\nReview.",
        )
        .unwrap();

        let result = parse_workflow_file(&file_path);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("can_commit requires role: actor"));
    }

    #[test]
    fn test_load_workflow_defs_from_worktree() {
        let tmp = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(tmp.path(), "test-coverage.md", TEST_WORKFLOW);

        let defs = load_workflow_defs(tmp.path().to_str().unwrap(), repo.path().to_str().unwrap())
            .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "test-coverage");
    }

    #[test]
    fn test_load_workflow_defs_falls_back_to_repo() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(repo.path(), "test-coverage.md", TEST_WORKFLOW);

        let defs = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn test_load_workflow_by_name() {
        let tmp = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(tmp.path(), "test-coverage.md", TEST_WORKFLOW);

        let def = load_workflow_by_name(
            tmp.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "test-coverage",
        )
        .unwrap();
        assert_eq!(def.name, "test-coverage");
        assert_eq!(def.steps.len(), 2);
    }

    #[test]
    fn test_load_workflow_by_name_not_found() {
        let tmp = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(tmp.path(), "test-coverage.md", TEST_WORKFLOW);

        let result = load_workflow_by_name(
            tmp.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "nonexistent",
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_load_workflow_by_name_rejects_invalid() {
        let result = load_workflow_by_name("/any", "/any", "../etc/passwd");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("Invalid workflow name"));
    }

    #[test]
    fn test_load_workflow_by_name_falls_back_to_repo() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(repo.path(), "test-coverage.md", TEST_WORKFLOW);

        let def = load_workflow_by_name(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "test-coverage",
        )
        .unwrap();
        assert_eq!(def.name, "test-coverage");
    }

    #[test]
    fn test_load_workflow_by_name_no_workflows_dir() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let result = load_workflow_by_name(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "test-coverage",
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_load_workflow_defs_no_directory_returns_empty() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let defs = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn test_load_workflow_defs_same_path_no_double_count() {
        // When worktree_path == repo_path the guard must skip the second pass,
        // so each workflow is counted exactly once.
        let dir = TempDir::new().unwrap();
        write_workflow_file(dir.path(), "test-coverage.md", TEST_WORKFLOW);
        let path = dir.path().to_str().unwrap();

        let defs = load_workflow_defs(path, path).unwrap();
        assert_eq!(defs.len(), 1, "same path must not double-count workflows");
    }

    #[test]
    fn test_workflow_role_display_and_parse() {
        assert_eq!(WorkflowRole::Reviewer.to_string(), "reviewer");
        assert_eq!(WorkflowRole::Actor.to_string(), "actor");
        assert_eq!(
            "reviewer".parse::<WorkflowRole>().unwrap(),
            WorkflowRole::Reviewer
        );
        assert_eq!(
            "actor".parse::<WorkflowRole>().unwrap(),
            WorkflowRole::Actor
        );
        assert!("invalid".parse::<WorkflowRole>().is_err());
    }

    #[test]
    fn test_workflow_trigger_display_and_parse() {
        assert_eq!(WorkflowTrigger::Manual.to_string(), "manual");
        assert_eq!(WorkflowTrigger::Pr.to_string(), "pr");
        assert_eq!(WorkflowTrigger::Scheduled.to_string(), "scheduled");
        assert_eq!(
            "manual".parse::<WorkflowTrigger>().unwrap(),
            WorkflowTrigger::Manual
        );
        assert!("invalid".parse::<WorkflowTrigger>().is_err());
    }

    #[test]
    fn test_step_prompt_section_defaults_to_name() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("simple.md");
        // No prompt_section specified — should default to step name
        fs::write(
            &file_path,
            "---\nsteps:\n  - name: analyze\n---\n\n## analyze\n\nDo analysis.",
        )
        .unwrap();

        let def = parse_workflow_file(&file_path).unwrap();
        assert_eq!(def.steps[0].name, "analyze");
        assert!(def.steps[0].prompt.contains("Do analysis."));
    }

    #[test]
    fn test_pr_trigger() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("auto.md");
        fs::write(
            &file_path,
            "---\ntrigger: pr\nsteps:\n  - name: check\n---\n\n## check\n\nCheck the PR.",
        )
        .unwrap();

        let def = parse_workflow_file(&file_path).unwrap();
        assert_eq!(def.trigger, WorkflowTrigger::Pr);
    }

    // A workflow with a different name from TEST_WORKFLOW, for merge tests.
    const ALT_WORKFLOW: &str = "\
---
name: lint-fix
description: Fix lint issues automatically
trigger: manual
steps:
  - name: fix
    role: actor
    can_commit: true
---

## fix

Fix all lint errors and commit.
";

    // TEST_WORKFLOW overridden with a distinct description to identify it in merge tests.
    const REPO_WORKFLOW: &str = "\
---
name: test-coverage
description: Repo version
trigger: manual
steps:
  - name: analyze
    role: reviewer
---

## analyze

Repo analyze prompt.
";

    const WORKTREE_WORKFLOW: &str = "\
---
name: test-coverage
description: Worktree version
trigger: manual
steps:
  - name: analyze
    role: reviewer
---

## analyze

Worktree analyze prompt.
";

    #[test]
    fn test_load_workflow_defs_repo_only() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(repo.path(), "test-coverage.md", REPO_WORKFLOW);

        let defs = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "test-coverage");
        assert_eq!(defs[0].description, "Repo version");
    }

    #[test]
    fn test_load_workflow_defs_worktree_only() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(worktree.path(), "test-coverage.md", WORKTREE_WORKFLOW);

        let defs = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "test-coverage");
        assert_eq!(defs[0].description, "Worktree version");
    }

    #[test]
    fn test_load_workflow_defs_merge_no_conflict() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(repo.path(), "test-coverage.md", TEST_WORKFLOW);
        write_workflow_file(worktree.path(), "lint-fix.md", ALT_WORKFLOW);

        let defs = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"test-coverage"));
        assert!(names.contains(&"lint-fix"));
    }

    #[test]
    fn test_load_workflow_defs_merge_worktree_wins() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_workflow_file(repo.path(), "test-coverage.md", REPO_WORKFLOW);
        write_workflow_file(worktree.path(), "test-coverage.md", WORKTREE_WORKFLOW);

        let defs = load_workflow_defs(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        // Only one "test-coverage" should survive — the worktree version.
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "test-coverage");
        assert_eq!(defs[0].description, "Worktree version");
    }
}
