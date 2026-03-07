//! File-based agent configuration for workflow steps.
//!
//! Reads `.conductor/agents/<name>.md` from the repo root (or worktree).
//! Each agent file uses YAML frontmatter + markdown body (the full prompt).
//!
//! Resolution order (first match wins):
//! 1. `.conductor/workflows/<workflow-name>/agents/<name>.md` — workflow-local override
//! 2. `.conductor/agents/<name>.md` — shared

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConductorError, Result};
use crate::text_util::parse_frontmatter;

/// YAML frontmatter for an agent `.md` file.
#[derive(Debug, Clone, Deserialize)]
struct AgentFrontmatter {
    #[serde(default = "default_role")]
    role: String,
    #[serde(default)]
    can_commit: bool,
    #[serde(default)]
    model: Option<String>,
}

pub fn default_role() -> String {
    "reviewer".to_string()
}

/// Role type for an agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Actor,
    Reviewer,
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Actor => write!(f, "actor"),
            Self::Reviewer => write!(f, "reviewer"),
        }
    }
}

impl std::str::FromStr for AgentRole {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "actor" => Ok(Self::Actor),
            "reviewer" => Ok(Self::Reviewer),
            _ => Err(format!(
                "unknown AgentRole: {s}. Expected 'actor' or 'reviewer'."
            )),
        }
    }
}

/// A parsed agent definition from a `.md` file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentDef {
    /// Agent identifier (from file stem).
    pub name: String,
    /// Role type: actor or reviewer.
    pub role: AgentRole,
    /// Whether this agent is permitted to commit code.
    pub can_commit: bool,
    /// Optional model override.
    pub model: Option<String>,
    /// The prompt template (full markdown body after frontmatter).
    pub prompt: String,
}

/// Parse an agent `.md` file into an `AgentDef`.
fn parse_agent_file(path: &Path) -> Result<AgentDef> {
    let content = fs::read_to_string(path).map_err(|e| {
        ConductorError::AgentConfig(format!("Failed to read agent file {}: {e}", path.display()))
    })?;

    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let (frontmatter, body) = match parse_frontmatter(&content) {
        Some(pair) => pair,
        None => {
            // No frontmatter — treat entire content as prompt with defaults.
            return Ok(AgentDef {
                name: file_stem,
                role: AgentRole::Reviewer,
                can_commit: false,
                model: None,
                prompt: content.trim().to_string(),
            });
        }
    };

    let fm: AgentFrontmatter = serde_yml::from_str(frontmatter).map_err(|e| {
        ConductorError::AgentConfig(format!(
            "Invalid YAML frontmatter in {}: {e}",
            path.display()
        ))
    })?;

    let role: AgentRole = fm
        .role
        .parse()
        .map_err(|e: String| ConductorError::AgentConfig(format!("In {}: {e}", path.display())))?;

    if fm.can_commit && role != AgentRole::Actor {
        return Err(ConductorError::AgentConfig(format!(
            "In {}: can_commit requires role: actor",
            path.display()
        )));
    }

    Ok(AgentDef {
        name: file_stem,
        role,
        can_commit: fm.can_commit,
        model: fm.model,
        prompt: body.trim().to_string(),
    })
}

/// Load an agent definition by name with resolution order:
/// 1. `.conductor/workflows/<workflow_name>/agents/<name>.md` (if workflow_name given)
/// 2. `.conductor/agents/<name>.md` in worktree
/// 3. `.conductor/agents/<name>.md` in repo
pub fn load_agent(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Result<AgentDef> {
    let filename = format!("{name}.md");

    // 1. Workflow-local override
    if let Some(wf_name) = workflow_name {
        let wf_local = PathBuf::from(worktree_path)
            .join(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("agents")
            .join(&filename);
        if wf_local.is_file() {
            return parse_agent_file(&wf_local);
        }
        // Also check repo path
        let wf_local_repo = PathBuf::from(repo_path)
            .join(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("agents")
            .join(&filename);
        if wf_local_repo.is_file() {
            return parse_agent_file(&wf_local_repo);
        }
    }

    // 2. Shared in worktree
    let worktree_agent = PathBuf::from(worktree_path)
        .join(".conductor")
        .join("agents")
        .join(&filename);
    if worktree_agent.is_file() {
        return parse_agent_file(&worktree_agent);
    }

    // 3. Shared in repo
    let repo_agent = PathBuf::from(repo_path)
        .join(".conductor")
        .join("agents")
        .join(&filename);
    if repo_agent.is_file() {
        return parse_agent_file(&repo_agent);
    }

    Err(ConductorError::AgentConfig(format!(
        "Agent '{name}' not found. Searched:\n  .conductor/agents/{filename}{}",
        if let Some(wf) = workflow_name {
            format!("\n  .conductor/workflows/{wf}/agents/{filename}")
        } else {
            String::new()
        }
    )))
}

/// Load all agent definitions from `.conductor/agents/*.md`.
pub fn load_all_agents(worktree_path: &str, repo_path: &str) -> Result<Vec<AgentDef>> {
    let worktree_dir = PathBuf::from(worktree_path)
        .join(".conductor")
        .join("agents");
    let agents_dir = if worktree_dir.is_dir() {
        worktree_dir
    } else {
        let repo_dir = PathBuf::from(repo_path).join(".conductor").join("agents");
        if !repo_dir.is_dir() {
            return Ok(Vec::new());
        }
        repo_dir
    };

    let mut entries: Vec<_> = fs::read_dir(&agents_dir)
        .map_err(|e| {
            ConductorError::AgentConfig(format!("Failed to read {}: {e}", agents_dir.display()))
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let mut defs = Vec::new();
    for entry in entries {
        defs.push(parse_agent_file(&entry.path())?);
    }
    Ok(defs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const TEST_AGENT: &str = "\
---
role: actor
can_commit: true
model: claude-opus-4-6
---

You are a software engineer. The ticket is: {{ticket_id}}

Prior step context: {{prior_context}}

Implement the plan written in PLAN.md.
";

    #[test]
    fn test_parse_agent_file_full() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("implement.md");
        fs::write(&file, TEST_AGENT).unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.name, "implement");
        assert_eq!(def.role, AgentRole::Actor);
        assert!(def.can_commit);
        assert_eq!(def.model.as_deref(), Some("claude-opus-4-6"));
        assert!(def.prompt.contains("{{ticket_id}}"));
        assert!(def.prompt.contains("PLAN.md"));
    }

    #[test]
    fn test_parse_agent_file_defaults() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("review.md");
        fs::write(&file, "---\nrole: reviewer\n---\nYou are a code reviewer.").unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.name, "review");
        assert_eq!(def.role, AgentRole::Reviewer);
        assert!(!def.can_commit);
        assert!(def.model.is_none());
        assert_eq!(def.prompt, "You are a code reviewer.");
    }

    #[test]
    fn test_parse_agent_file_no_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("simple.md");
        fs::write(&file, "Just a plain prompt with no frontmatter.").unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.name, "simple");
        assert_eq!(def.role, AgentRole::Reviewer);
        assert_eq!(def.prompt, "Just a plain prompt with no frontmatter.");
    }

    #[test]
    fn test_can_commit_requires_actor() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("bad.md");
        fs::write(&file, "---\nrole: reviewer\ncan_commit: true\n---\nPrompt.").unwrap();

        let result = parse_agent_file(&file);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("can_commit requires role: actor"));
    }

    #[test]
    fn test_load_agent_resolution_order() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Put agent in repo shared dir
        let repo_agents = repo.path().join(".conductor").join("agents");
        fs::create_dir_all(&repo_agents).unwrap();
        fs::write(
            repo_agents.join("plan.md"),
            "---\nrole: actor\n---\nRepo-level plan agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "plan",
            None,
        )
        .unwrap();
        assert_eq!(def.prompt, "Repo-level plan agent.");

        // Now add worktree-level override
        let wt_agents = worktree.path().join(".conductor").join("agents");
        fs::create_dir_all(&wt_agents).unwrap();
        fs::write(
            wt_agents.join("plan.md"),
            "---\nrole: actor\n---\nWorktree-level plan agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "plan",
            None,
        )
        .unwrap();
        assert_eq!(def.prompt, "Worktree-level plan agent.");
    }

    #[test]
    fn test_load_agent_workflow_local() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Shared agent
        let agents = worktree.path().join(".conductor").join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(
            agents.join("plan.md"),
            "---\nrole: actor\n---\nShared plan agent.",
        )
        .unwrap();

        // Workflow-local override
        let wf_agents = worktree
            .path()
            .join(".conductor")
            .join("workflows")
            .join("ticket-to-pr")
            .join("agents");
        fs::create_dir_all(&wf_agents).unwrap();
        fs::write(
            wf_agents.join("plan.md"),
            "---\nrole: actor\n---\nWorkflow-local plan agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "plan",
            Some("ticket-to-pr"),
        )
        .unwrap();
        assert_eq!(def.prompt, "Workflow-local plan agent.");
    }

    #[test]
    fn test_load_agent_not_found() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let result = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "nonexistent",
            None,
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_load_all_agents() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let agents = worktree.path().join(".conductor").join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("plan.md"), "---\nrole: actor\n---\nPlan.").unwrap();
        fs::write(
            agents.join("review.md"),
            "---\nrole: reviewer\n---\nReview.",
        )
        .unwrap();

        let defs = load_all_agents(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "plan");
        assert_eq!(defs[1].name, "review");
    }

    #[test]
    fn test_load_all_agents_no_directory() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let defs = load_all_agents(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn test_agent_role_display_and_parse() {
        assert_eq!(AgentRole::Actor.to_string(), "actor");
        assert_eq!(AgentRole::Reviewer.to_string(), "reviewer");
        assert_eq!("actor".parse::<AgentRole>().unwrap(), AgentRole::Actor);
        assert_eq!(
            "reviewer".parse::<AgentRole>().unwrap(),
            AgentRole::Reviewer
        );
        assert!("invalid".parse::<AgentRole>().is_err());
    }
}
