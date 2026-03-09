//! File-based agent configuration for workflow steps.
//!
//! Reads `.conductor/agents/<name>.md` from the repo root (or worktree).
//! Each agent file uses YAML frontmatter + markdown body (the full prompt).
//!
//! Resolution order for short names (first match wins):
//! 1. `.conductor/workflows/<workflow-name>/agents/<name>.md` — workflow-local override
//! 2. `.conductor/agents/<name>.md` — shared conductor agents
//! 3. `.claude/agents/<name>.md` — Claude Code agents (fallback)
//!
//! Explicit paths (`AgentSpec::Path`) are resolved directly relative to the
//! repository root and bypass the search order.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConductorError, Result};
use crate::text_util::{parse_frontmatter, resolve_conductor_subdir};

/// How to locate an agent — either a short name resolved via search order, or
/// an explicit path relative to the repository root.
///
/// This type belongs to `agent_config` and is independent of the workflow DSL.
/// Callers in the workflow layer convert `AgentRef` (DSL type) to `AgentSpec`
/// before calling [`load_agent`].
#[derive(Debug, Clone)]
pub enum AgentSpec {
    /// Short name (e.g. `plan`) resolved via the search order.
    Name(String),
    /// Explicit path relative to the repository root (e.g. `.claude/agents/plan.md`).
    Path(String),
}

impl AgentSpec {
    /// Human-readable label (the inner string value).
    pub fn label(&self) -> &str {
        match self {
            Self::Name(s) | Self::Path(s) => s.as_str(),
        }
    }
}

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

/// Load an agent definition from an `AgentSpec`.
///
/// - `AgentSpec::Name` — resolved via the search order (workflow-local override,
///   shared conductor agents, Claude Code agents).
/// - `AgentSpec::Path` — resolved as an explicit path relative to the repository
///   root; bypasses the search order entirely.
pub fn load_agent(
    worktree_path: &str,
    repo_path: &str,
    agent_spec: &AgentSpec,
    workflow_name: Option<&str>,
) -> Result<AgentDef> {
    match agent_spec {
        AgentSpec::Name(name) => load_agent_by_name(worktree_path, repo_path, name, workflow_name),
        AgentSpec::Path(rel_path) => load_agent_by_path(repo_path, rel_path),
    }
}

/// Return the first path (formed by joining `base/subdir/filename`) that is a file,
/// checking each base in order.
fn find_agent_path(bases: &[&str], subdir: &Path, filename: &str) -> Option<PathBuf> {
    bases.iter().find_map(|base| {
        let path = PathBuf::from(base).join(subdir).join(filename);
        path.is_file().then_some(path)
    })
}

/// Resolve an agent by short name using the search order.
fn load_agent_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Result<AgentDef> {
    let filename = format!("{name}.md");
    let bases = [worktree_path, repo_path];

    // 1. Workflow-local override (worktree, then repo)
    if let Some(wf_name) = workflow_name {
        let subdir = Path::new(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("agents");
        if let Some(path) = find_agent_path(&bases, &subdir, &filename) {
            return parse_agent_file(&path);
        }
    }

    // 2. Shared conductor agents (worktree, then repo)
    if let Some(path) = find_agent_path(&bases, Path::new(".conductor/agents"), &filename) {
        return parse_agent_file(&path);
    }

    // 3. Claude Code agents fallback (worktree, then repo)
    if let Some(path) = find_agent_path(&bases, Path::new(".claude/agents"), &filename) {
        return parse_agent_file(&path);
    }

    Err(ConductorError::AgentConfig(format!(
        "Agent '{name}' not found. Searched:\n\
         {}  .conductor/agents/{filename}\n  .claude/agents/{filename}",
        if let Some(wf) = workflow_name {
            format!("  .conductor/workflows/{wf}/agents/{filename}\n")
        } else {
            String::new()
        }
    )))
}

/// Resolve an agent from an explicit path relative to the repository root.
///
/// The path must remain within the repository root (no escaping via `../`).
fn load_agent_by_path(repo_path: &str, rel_path: &str) -> Result<AgentDef> {
    if Path::new(rel_path).is_absolute() {
        return Err(ConductorError::AgentConfig(format!(
            "Explicit agent path '{rel_path}' must be relative, not absolute"
        )));
    }

    let repo_root = PathBuf::from(repo_path);
    let joined = repo_root.join(rel_path);

    // Canonicalize to resolve `..` components and check bounds.
    let canonical = joined.canonicalize().map_err(|_| {
        ConductorError::AgentConfig(format!(
            "Agent file not found: '{rel_path}' (resolved relative to repo root '{repo_path}')"
        ))
    })?;

    let canonical_repo = repo_root.canonicalize().map_err(|e| {
        ConductorError::AgentConfig(format!(
            "Failed to canonicalize repo root '{repo_path}': {e}"
        ))
    })?;

    if !canonical.starts_with(&canonical_repo) {
        return Err(ConductorError::AgentConfig(format!(
            "Agent path '{rel_path}' escapes the repository root — path traversal is not allowed"
        )));
    }

    if !canonical.is_file() {
        return Err(ConductorError::AgentConfig(format!(
            "Agent file not found: '{rel_path}' (resolved to '{}')",
            canonical.display()
        )));
    }

    parse_agent_file(&canonical)
}

/// Validate that all agents in `specs` can be resolved, returning the labels of
/// any that are missing.
///
/// This is used by both the CLI `workflow validate` command and the workflow
/// executor to check agent availability before starting a run.
pub fn find_missing_agents(
    worktree_path: &str,
    repo_path: &str,
    specs: &[AgentSpec],
    workflow_name: Option<&str>,
) -> Vec<String> {
    specs
        .iter()
        .filter(|spec| load_agent(worktree_path, repo_path, spec, workflow_name).is_err())
        .map(|spec| spec.label().to_string())
        .collect()
}

/// Load all agent definitions from `.conductor/agents/*.md`.
pub fn load_all_agents(worktree_path: &str, repo_path: &str) -> Result<Vec<AgentDef>> {
    let Some(agents_dir) = resolve_conductor_subdir(worktree_path, repo_path, "agents") else {
        return Ok(Vec::new());
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
            &AgentSpec::Name("plan".to_string()),
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
            &AgentSpec::Name("plan".to_string()),
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
            &AgentSpec::Name("plan".to_string()),
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
            &AgentSpec::Name("nonexistent".to_string()),
            None,
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_load_agent_claude_fallback() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Put agent only in .claude/agents/
        let claude_agents = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(
            claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nClaude Code review agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("review".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(def.prompt, "Claude Code review agent.");
    }

    #[test]
    fn test_load_agent_claude_fallback_worktree() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Put agent only in worktree's .claude/agents/
        let claude_agents = worktree.path().join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(
            claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nWorktree Claude Code review agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("review".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(def.prompt, "Worktree Claude Code review agent.");
    }

    #[test]
    fn test_load_agent_claude_worktree_takes_precedence_over_repo() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Both worktree and repo have .claude/agents/ with the same agent
        let wt_claude_agents = worktree.path().join(".claude").join("agents");
        fs::create_dir_all(&wt_claude_agents).unwrap();
        fs::write(
            wt_claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nWorktree Claude agent.",
        )
        .unwrap();

        let repo_claude_agents = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&repo_claude_agents).unwrap();
        fs::write(
            repo_claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nRepo Claude agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("review".to_string()),
            None,
        )
        .unwrap();
        // Worktree .claude/agents should win over repo .claude/agents
        assert_eq!(def.prompt, "Worktree Claude agent.");
    }

    #[test]
    fn test_load_agent_conductor_takes_precedence_over_claude() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Both .conductor/agents/ and .claude/agents/ have the agent
        let conductor_agents = repo.path().join(".conductor").join("agents");
        fs::create_dir_all(&conductor_agents).unwrap();
        fs::write(
            conductor_agents.join("review.md"),
            "---\nrole: reviewer\n---\nConductor review agent.",
        )
        .unwrap();

        let claude_agents = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(
            claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nClaude Code review agent.",
        )
        .unwrap();

        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("review".to_string()),
            None,
        )
        .unwrap();
        // Conductor shared agent should win over .claude/agents/
        assert_eq!(def.prompt, "Conductor review agent.");
    }

    #[test]
    fn test_load_agent_explicit_path() {
        let repo = TempDir::new().unwrap();

        // Create agent at an explicit path
        let agents_dir = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();
        fs::write(
            agents_dir.join("code-review.md"),
            "---\nrole: reviewer\n---\nExplicit path review agent.",
        )
        .unwrap();

        let def = load_agent(
            repo.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Path(".claude/agents/code-review.md".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(def.prompt, "Explicit path review agent.");
    }

    #[test]
    fn test_load_agent_explicit_path_not_found() {
        let repo = TempDir::new().unwrap();

        let result = load_agent(
            repo.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Path(".claude/agents/nonexistent.md".to_string()),
            None,
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found") || err.contains("nonexistent"));
    }

    #[test]
    fn test_load_agent_explicit_path_absolute_rejected() {
        let repo = TempDir::new().unwrap();

        let result = load_agent(
            repo.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Path("/etc/passwd".to_string()),
            None,
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("absolute"));
    }

    #[test]
    fn test_load_agent_explicit_path_traversal_rejected() {
        let repo = TempDir::new().unwrap();
        // Create a file outside the repo root in a sibling directory
        let sibling = TempDir::new().unwrap();
        fs::write(
            sibling.path().join("evil.md"),
            "---\nrole: reviewer\n---\nEvil agent.",
        )
        .unwrap();

        // Try to escape repo root with ../
        let evil_path = "../evil.md".to_string();
        let result = load_agent(
            repo.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Path(evil_path),
            None,
        );
        // Either not found (file doesn't exist at that traversal path) or traversal rejected
        assert!(result.is_err());
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
