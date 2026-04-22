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
use crate::text_util::parse_frontmatter;

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
    #[serde(default = "default_runtime")]
    runtime: String,
}

pub fn default_role() -> String {
    "reviewer".to_string()
}

fn default_runtime() -> String {
    "claude".to_string()
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
    /// The runtime to use for this agent (defaults to "claude").
    pub runtime: String,
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
                runtime: default_runtime(),
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
        runtime: fm.runtime,
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
    extra_plugin_dirs: &[String],
) -> Result<AgentDef> {
    match agent_spec {
        AgentSpec::Name(name) => load_agent_by_name(
            worktree_path,
            repo_path,
            name,
            workflow_name,
            extra_plugin_dirs,
        ),
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

/// Verify that `path` (canonicalized) is contained within `base` (canonicalized).
/// Returns the canonicalized path on success.
fn validate_path_within_base(path: &Path, base: &str) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|_| {
        ConductorError::AgentConfig(format!("Agent file not found: '{}'", path.display()))
    })?;
    let canonical_base = PathBuf::from(base).canonicalize().map_err(|e| {
        ConductorError::AgentConfig(format!("Failed to canonicalize base '{base}': {e}"))
    })?;
    if !canonical.starts_with(&canonical_base) {
        return Err(ConductorError::AgentConfig(format!(
            "Agent path '{}' escapes the base directory — path traversal is not allowed",
            path.display()
        )));
    }
    Ok(canonical)
}

/// Verify that `path` is within at least one of `base1` or `base2`.
/// Used for the worktree/repo dual-base check in `load_agent_by_name`.
fn validate_path_within_either_base(path: &Path, base1: &str, base2: &str) -> Result<()> {
    validate_path_within_base(path, base1)
        .or_else(|_| validate_path_within_base(path, base2))
        .map(|_| ())
}

/// Resolve an agent by short name using the search order.
fn load_agent_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
    extra_plugin_dirs: &[String],
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
            validate_path_within_either_base(&path, worktree_path, repo_path)?;
            return parse_agent_file(&path);
        }
    }

    // 2. Shared conductor agents (worktree, then repo)
    if let Some(path) = find_agent_path(&bases, Path::new(".conductor/agents"), &filename) {
        validate_path_within_either_base(&path, worktree_path, repo_path)?;
        return parse_agent_file(&path);
    }

    // 3. Claude Code agents fallback (worktree, then repo)
    if let Some(path) = find_agent_path(&bases, Path::new(".claude/agents"), &filename) {
        validate_path_within_either_base(&path, worktree_path, repo_path)?;
        return parse_agent_file(&path);
    }

    // 4. Extra plugin directories (lowest priority)
    for dir in extra_plugin_dirs {
        let path = Path::new(dir).join("agents").join(&filename);
        if path.is_file() {
            validate_path_within_base(&path, dir)?;
            return parse_agent_file(&path);
        }
    }

    let mut searched = String::new();
    if let Some(wf) = workflow_name {
        searched.push_str(&format!("  .conductor/workflows/{wf}/agents/{filename}\n"));
    }
    searched.push_str(&format!("  .conductor/agents/{filename}\n"));
    searched.push_str(&format!("  .claude/agents/{filename}"));
    for dir in extra_plugin_dirs {
        searched.push_str(&format!("\n  {dir}/agents/{filename}"));
    }

    Err(ConductorError::AgentConfig(format!(
        "Agent '{name}' not found. Searched:\n{searched}"
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

    let joined = PathBuf::from(repo_path).join(rel_path);
    let canonical = validate_path_within_base(&joined, repo_path)?;

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
///
/// `extra_plugin_dirs` are additional directories (from `.wf` `plugin_dirs` and
/// CLI `--plugin-dir`) to search for agent definitions. Each directory is probed
/// as `{dir}/agents/{name}.md`.
pub fn find_missing_agents(
    worktree_path: &str,
    repo_path: &str,
    specs: &[AgentSpec],
    workflow_name: Option<&str>,
    extra_plugin_dirs: &[String],
) -> Vec<String> {
    specs
        .iter()
        .filter(|spec| {
            load_agent(
                worktree_path,
                repo_path,
                spec,
                workflow_name,
                extra_plugin_dirs,
            )
            .is_err()
        })
        .map(|spec| spec.label().to_string())
        .collect()
}

/// Collect sorted `.md` entries from a directory, returning an empty vec if the
/// directory does not exist.
fn collect_md_entries(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| ConductorError::AgentConfig(format!("Failed to read {}: {e}", dir.display())))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// Load all agent definitions, scanning in priority order (first definition of a
/// name wins, consistent with [`load_agent`] resolution):
///
/// 1. `.conductor/agents/` in the worktree
/// 2. `.conductor/agents/` in the repo root
/// 3. `.claude/agents/` in the worktree
/// 4. `.claude/agents/` in the repo root
pub fn load_all_agents(worktree_path: &str, repo_path: &str) -> Result<Vec<AgentDef>> {
    let search_dirs: &[PathBuf] = &[
        PathBuf::from(worktree_path)
            .join(".conductor")
            .join("agents"),
        PathBuf::from(repo_path).join(".conductor").join("agents"),
        PathBuf::from(worktree_path).join(".claude").join("agents"),
        PathBuf::from(repo_path).join(".claude").join("agents"),
    ];

    let mut seen_names = std::collections::HashSet::new();
    let mut defs = Vec::new();

    for dir in search_dirs {
        for entry in collect_md_entries(dir)? {
            let def = parse_agent_file(&entry.path())?;
            if seen_names.insert(def.name.clone()) {
                defs.push(def);
            }
        }
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
        assert_eq!(def.runtime, "claude");
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
        assert_eq!(def.runtime, "claude");
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
        assert_eq!(def.runtime, "claude");
    }

    #[test]
    fn test_runtime_explicit_claude() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("agent.md");
        fs::write(
            &file,
            "---\nrole: actor\nruntime: claude\n---\nPrompt body.",
        )
        .unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.runtime, "claude");
    }

    #[test]
    fn test_runtime_explicit_cli() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("agent.md");
        fs::write(&file, "---\nrole: actor\nruntime: cli\n---\nPrompt body.").unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.runtime, "cli");
    }

    #[test]
    fn test_runtime_default_when_omitted() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("agent.md");
        fs::write(&file, "---\nrole: reviewer\n---\nPrompt body.").unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.runtime, "claude");
    }

    #[test]
    fn test_runtime_default_no_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("agent.md");
        fs::write(&file, "Prompt body with no frontmatter at all.").unwrap();

        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.runtime, "claude");
    }

    #[test]
    fn test_runtime_unknown_stored_as_is() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("agent.md");
        fs::write(
            &file,
            "---\nrole: reviewer\nruntime: custom-plugin\n---\nPrompt body.",
        )
        .unwrap();

        // Unknown runtime names are stored as-is; validation is deferred to resolve_runtime
        let def = parse_agent_file(&file).unwrap();
        assert_eq!(def.runtime, "custom-plugin");
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
    fn test_load_all_agents_includes_claude_agents() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Only .claude/agents/ — no .conductor/agents/
        let claude_agents = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(
            claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nClaude review agent.",
        )
        .unwrap();

        let defs = load_all_agents(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "review");
        assert_eq!(defs[0].prompt, "Claude review agent.");
    }

    #[test]
    fn test_load_all_agents_deduplicates_by_name() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // .conductor/agents/ has "plan" — higher priority
        let conductor_agents = repo.path().join(".conductor").join("agents");
        fs::create_dir_all(&conductor_agents).unwrap();
        fs::write(
            conductor_agents.join("plan.md"),
            "---\nrole: actor\n---\nConductor plan.",
        )
        .unwrap();

        // .claude/agents/ also has "plan" and adds "review"
        let claude_agents = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&claude_agents).unwrap();
        fs::write(
            claude_agents.join("plan.md"),
            "---\nrole: actor\n---\nClaude plan.",
        )
        .unwrap();
        fs::write(
            claude_agents.join("review.md"),
            "---\nrole: reviewer\n---\nClaude review.",
        )
        .unwrap();

        let defs = load_all_agents(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();

        // "plan" from .conductor wins; "review" from .claude is added
        assert_eq!(defs.len(), 2);
        let plan = defs.iter().find(|d| d.name == "plan").unwrap();
        assert_eq!(plan.prompt, "Conductor plan.");
        let review = defs.iter().find(|d| d.name == "review").unwrap();
        assert_eq!(review.prompt, "Claude review.");
    }

    #[test]
    fn test_load_all_agents_worktree_takes_precedence() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // Both worktree and repo have .conductor/agents/ with the same name
        let wt_agents = worktree.path().join(".conductor").join("agents");
        fs::create_dir_all(&wt_agents).unwrap();
        fs::write(
            wt_agents.join("plan.md"),
            "---\nrole: actor\n---\nWorktree conductor plan.",
        )
        .unwrap();

        let repo_agents = repo.path().join(".conductor").join("agents");
        fs::create_dir_all(&repo_agents).unwrap();
        fs::write(
            repo_agents.join("plan.md"),
            "---\nrole: actor\n---\nRepo conductor plan.",
        )
        .unwrap();

        let defs = load_all_agents(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].prompt, "Worktree conductor plan.");
    }

    #[test]
    fn test_load_all_agents_merges_all_sources() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        // worktree .conductor/agents/: plan
        let wt_conductor = worktree.path().join(".conductor").join("agents");
        fs::create_dir_all(&wt_conductor).unwrap();
        fs::write(
            wt_conductor.join("plan.md"),
            "---\nrole: actor\n---\nWorktree plan.",
        )
        .unwrap();

        // repo .conductor/agents/: implement
        let repo_conductor = repo.path().join(".conductor").join("agents");
        fs::create_dir_all(&repo_conductor).unwrap();
        fs::write(
            repo_conductor.join("implement.md"),
            "---\nrole: actor\n---\nRepo implement.",
        )
        .unwrap();

        // worktree .claude/agents/: lint
        let wt_claude = worktree.path().join(".claude").join("agents");
        fs::create_dir_all(&wt_claude).unwrap();
        fs::write(
            wt_claude.join("lint.md"),
            "---\nrole: reviewer\n---\nWorktree lint.",
        )
        .unwrap();

        // repo .claude/agents/: review
        let repo_claude = repo.path().join(".claude").join("agents");
        fs::create_dir_all(&repo_claude).unwrap();
        fs::write(
            repo_claude.join("review.md"),
            "---\nrole: reviewer\n---\nRepo review.",
        )
        .unwrap();

        let defs = load_all_agents(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(defs.len(), 4);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"plan"));
        assert!(names.contains(&"implement"));
        assert!(names.contains(&"lint"));
        assert!(names.contains(&"review"));
    }

    #[test]
    fn test_load_agent_via_extra_plugin_dirs() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let plugin = TempDir::new().unwrap();

        // Agent only exists in the plugin directory
        let plugin_agents = plugin.path().join("agents");
        fs::create_dir_all(&plugin_agents).unwrap();
        fs::write(
            plugin_agents.join("executor.md"),
            "---\nrole: actor\n---\nPlugin executor agent.",
        )
        .unwrap();

        let plugin_dirs = vec![plugin.path().to_str().unwrap().to_string()];
        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("executor".to_string()),
            None,
            &plugin_dirs,
        )
        .unwrap();
        assert_eq!(def.prompt, "Plugin executor agent.");
    }

    #[test]
    fn test_load_agent_local_takes_precedence_over_plugin_dirs() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let plugin = TempDir::new().unwrap();

        // Agent exists in both .conductor/agents/ and plugin dir
        let conductor_agents = repo.path().join(".conductor").join("agents");
        fs::create_dir_all(&conductor_agents).unwrap();
        fs::write(
            conductor_agents.join("executor.md"),
            "---\nrole: actor\n---\nLocal conductor executor.",
        )
        .unwrap();

        let plugin_agents = plugin.path().join("agents");
        fs::create_dir_all(&plugin_agents).unwrap();
        fs::write(
            plugin_agents.join("executor.md"),
            "---\nrole: actor\n---\nPlugin executor agent.",
        )
        .unwrap();

        let plugin_dirs = vec![plugin.path().to_str().unwrap().to_string()];
        let def = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("executor".to_string()),
            None,
            &plugin_dirs,
        )
        .unwrap();
        // Local .conductor/agents/ should win over plugin dirs
        assert_eq!(def.prompt, "Local conductor executor.");
    }

    #[test]
    fn test_load_agent_not_found_error_includes_plugin_dirs() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();

        let plugin_dirs = vec!["/some/plugin/dir".to_string()];
        let result = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name("nonexistent".to_string()),
            None,
            &plugin_dirs,
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found"));
        assert!(err.contains("/some/plugin/dir/agents/nonexistent.md"));
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

    #[test]
    fn test_load_agent_name_absolute_rejected() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        // Agent file exists completely outside the search bases.
        fs::write(
            outside.path().join("secret.md"),
            "---\nrole: reviewer\n---\nSecret.",
        )
        .unwrap();

        // When name begins with '/', filename becomes absolute and PathBuf::join
        // discards the base/subdir, making the resolved path escape the search bases.
        let name = format!("{}/secret", outside.path().display());

        let result = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name(name),
            None,
            &[],
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("path traversal") || err.contains("escapes"),
            "Expected path traversal error, got: {err}"
        );
    }

    #[test]
    fn test_load_agent_name_traversal_rejected() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let evil_dir = TempDir::new().unwrap();

        // Agent file exists in a sibling temp dir outside the search bases.
        fs::write(
            evil_dir.path().join("evil.md"),
            "---\nrole: reviewer\n---\nEvil.",
        )
        .unwrap();

        // The OS resolves `..` components only when intermediate directories exist.
        // Create the agents dir so the traversal path resolves to the actual file.
        let agents_dir = worktree.path().join(".conductor").join("agents");
        fs::create_dir_all(&agents_dir).unwrap();

        // Both TempDirs should share the same parent (e.g. /tmp or /var/.../T/).
        // From worktree/.conductor/agents/ (3 levels deep), going up 3 times
        // reaches the common parent, then we descend into evil_dir.
        let wt_parent = worktree.path().parent().unwrap();
        let evil_parent = evil_dir.path().parent().unwrap();
        if wt_parent != evil_parent {
            // Platforms where temp dirs have different parents — skip.
            return;
        }
        let evil_dir_name = evil_dir.path().file_name().unwrap().to_str().unwrap();
        let name = format!("../../../{evil_dir_name}/evil");

        let result = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name(name),
            None,
            &[],
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("path traversal") || err.contains("escapes"),
            "Expected path traversal error, got: {err}"
        );
    }

    #[test]
    fn test_load_agent_plugin_dir_traversal_rejected() {
        let plugin = TempDir::new().unwrap();
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        // Agent file exists completely outside the plugin dir.
        fs::write(
            outside.path().join("stolen.md"),
            "---\nrole: reviewer\n---\nStolen.",
        )
        .unwrap();

        // Absolute name causes PathBuf::join to discard the plugin dir prefix.
        let name = format!("{}/stolen", outside.path().display());

        let plugin_dirs = vec![plugin.path().to_str().unwrap().to_string()];
        let result = load_agent(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            &AgentSpec::Name(name),
            None,
            &plugin_dirs,
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("path traversal") || err.contains("escapes"),
            "Expected path traversal error, got: {err}"
        );
    }
}
