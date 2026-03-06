//! File-based reviewer configuration for multi-agent PR review swarms.
//!
//! Reads `.conductor/reviewers/*.md` from the repo root (not the PR worktree)
//! and `.conductor/review.toml` for swarm-level settings.
//! Each reviewer file uses YAML frontmatter + markdown body.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConductorError, Result};

const REVIEWER_HINT: &str = "See .conductor/reviewers/ in conductor-ai for reference roles.";

/// Swarm-level review settings from `.conductor/review.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewSettings {
    /// Whether to post an aggregated review comment to the PR (default: true).
    #[serde(default = "default_true")]
    pub post_to_pr: bool,
    /// Whether to auto-enqueue for merge when all required reviewers approve (default: true).
    #[serde(default = "default_true")]
    pub auto_merge: bool,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            post_to_pr: true,
            auto_merge: true,
        }
    }
}

/// YAML frontmatter fields for a reviewer role `.md` file.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ReviewerFrontmatter {
    name: Option<String>,
    description: Option<String>,
    model: Option<String>,
    #[serde(default = "default_true")]
    required: bool,
    color: Option<String>,
    source: Option<String>,
}

fn default_true() -> bool {
    true
}

/// A single reviewer role parsed from a `.md` file.
#[derive(Debug, Clone)]
pub struct ReviewerRole {
    /// Short identifier, e.g. "architecture", "security".
    pub name: String,
    /// Human-readable focus area description.
    pub focus: String,
    /// System prompt (the markdown body of the file).
    pub system_prompt: String,
    /// If true, blocking findings from this reviewer prevent auto-merge.
    pub required: bool,
}

/// Parse a reviewer `.md` file into a `ReviewerRole`.
///
/// The file format is YAML frontmatter delimited by `---` lines, followed by
/// a markdown body that becomes the system prompt.
fn parse_reviewer_file(path: &Path) -> Result<ReviewerRole> {
    let content = fs::read_to_string(path).map_err(|e| {
        ConductorError::Config(format!(
            "Failed to read reviewer file {}: {e}",
            path.display()
        ))
    })?;

    let (frontmatter, body) = parse_frontmatter(&content).ok_or_else(|| {
        ConductorError::Config(format!(
            "Invalid frontmatter in reviewer file {}. Expected YAML between --- delimiters.",
            path.display()
        ))
    })?;

    let fm: ReviewerFrontmatter = serde_yml::from_str(frontmatter).map_err(|e| {
        ConductorError::Config(format!(
            "Invalid YAML frontmatter in {}: {e}",
            path.display()
        ))
    })?;

    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(ReviewerRole {
        name: fm.name.unwrap_or_else(|| file_stem.clone()),
        focus: fm.description.unwrap_or(file_stem),
        system_prompt: body.trim().to_string(),
        required: fm.required,
    })
}

/// Split a file's content into (frontmatter_yaml, body).
///
/// Returns `None` if the content doesn't start with `---` or has no closing `---`.
fn parse_frontmatter(content: &str) -> Option<(&str, &str)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    // Skip the opening `---` line
    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    // Find the closing `---`
    let close_pos = after_open.find("\n---")?;
    let yaml = &after_open[..close_pos];
    let rest = &after_open[close_pos + 4..]; // skip "\n---"
                                             // Skip the newline after closing ---
    let body = rest.strip_prefix('\n').unwrap_or(rest);
    Some((yaml, body))
}

/// Load review settings from `.conductor/review.toml` in the given repo path.
///
/// Returns `ReviewSettings::default()` if the file doesn't exist.
pub fn load_review_settings(repo_path: &str) -> Result<ReviewSettings> {
    let settings_path = PathBuf::from(repo_path)
        .join(".conductor")
        .join("review.toml");

    if !settings_path.is_file() {
        return Ok(ReviewSettings::default());
    }

    let content = fs::read_to_string(&settings_path).map_err(|e| {
        ConductorError::Config(format!("Failed to read {}: {e}", settings_path.display()))
    })?;

    toml::from_str(&content).map_err(|e| {
        ConductorError::Config(format!("Invalid TOML in {}: {e}", settings_path.display()))
    })
}

/// Load all reviewer roles from `.conductor/reviewers/*.md`.
///
/// Checks `worktree_path` first (supports developing/testing reviewer files in a
/// branch before merging), then falls back to `repo_path` (the main checkout).
///
/// Returns an error with a helpful message if neither location has the directory.
pub fn load_reviewer_roles(worktree_path: &str, repo_path: &str) -> Result<Vec<ReviewerRole>> {
    // Prefer the worktree so new/modified reviewer files can be tested in-branch.
    // Fall back to the main repo checkout when the worktree doesn't have them.
    let worktree_dir = PathBuf::from(worktree_path)
        .join(".conductor")
        .join("reviewers");
    let reviewers_dir = if worktree_dir.is_dir() {
        worktree_dir
    } else {
        let repo_dir = PathBuf::from(repo_path)
            .join(".conductor")
            .join("reviewers");
        if !repo_dir.is_dir() {
            return Err(ConductorError::Config(format!(
                "No .conductor/reviewers/ directory found in {} or {}. \
                 Create it and add reviewer role .md files. {REVIEWER_HINT}",
                worktree_path, repo_path
            )));
        }
        repo_dir
    };

    let mut roles: Vec<ReviewerRole> = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&reviewers_dir)
        .map_err(|e| {
            ConductorError::Config(format!("Failed to read {}: {e}", reviewers_dir.display()))
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();

    // Sort by filename for deterministic ordering
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        roles.push(parse_reviewer_file(&entry.path())?);
    }

    if roles.is_empty() {
        return Err(ConductorError::Config(format!(
            "No .md files found in {}. Add reviewer role files. {REVIEWER_HINT}",
            reviewers_dir.display()
        )));
    }

    Ok(roles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_reviewer_file(dir: &Path, name: &str, content: &str) {
        let reviewers_dir = dir.join(".conductor").join("reviewers");
        fs::create_dir_all(&reviewers_dir).unwrap();
        fs::write(reviewers_dir.join(name), content).unwrap();
    }

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = "---\nname: security\nrequired: true\n---\nYou are a security reviewer.";
        let (yaml, body) = parse_frontmatter(content).unwrap();
        assert!(yaml.contains("name: security"));
        assert_eq!(body, "You are a security reviewer.");
    }

    #[test]
    fn test_parse_frontmatter_no_opening() {
        assert!(parse_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn test_parse_frontmatter_no_closing() {
        assert!(parse_frontmatter("---\nname: test\nno closing").is_none());
    }

    #[test]
    fn test_parse_reviewer_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("security.md");
        fs::write(
            &file_path,
            "---\nname: security\ndescription: Input validation, auth gaps\nrequired: true\n---\nYou are a security reviewer.\nFocus on injection risks.",
        ).unwrap();

        let role = parse_reviewer_file(&file_path).unwrap();
        assert_eq!(role.name, "security");
        assert_eq!(role.focus, "Input validation, auth gaps");
        assert!(role.required);
        assert!(role.system_prompt.contains("security reviewer"));
        assert!(role.system_prompt.contains("injection risks"));
    }

    #[test]
    fn test_parse_reviewer_file_defaults_name_to_stem() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("my-reviewer.md");
        fs::write(&file_path, "---\nrequired: false\n---\nReview the code.").unwrap();

        let role = parse_reviewer_file(&file_path).unwrap();
        assert_eq!(role.name, "my-reviewer");
        assert!(!role.required);
    }

    #[test]
    fn test_parse_reviewer_file_default_required_true() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.md");
        fs::write(&file_path, "---\nname: test\n---\nReview.").unwrap();

        let role = parse_reviewer_file(&file_path).unwrap();
        assert!(role.required);
    }

    #[test]
    fn test_load_reviewer_roles_from_worktree() {
        let tmp = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        write_reviewer_file(
            tmp.path(),
            "architecture.md",
            "---\nname: architecture\ndescription: Design review\nrequired: true\n---\nYou are an architect.",
        );
        write_reviewer_file(
            tmp.path(),
            "security.md",
            "---\nname: security\ndescription: Security review\nrequired: false\n---\nYou review security.",
        );

        // Worktree has reviewers — should use those regardless of repo
        let roles =
            load_reviewer_roles(tmp.path().to_str().unwrap(), repo.path().to_str().unwrap())
                .unwrap();
        assert_eq!(roles.len(), 2);
        assert_eq!(roles[0].name, "architecture");
        assert_eq!(roles[1].name, "security");
    }

    #[test]
    fn test_load_reviewer_roles_falls_back_to_repo() {
        let worktree = TempDir::new().unwrap(); // no .conductor/reviewers/
        let repo = TempDir::new().unwrap();
        write_reviewer_file(
            repo.path(),
            "security.md",
            "---\nname: security\ndescription: Security review\nrequired: true\n---\nYou review security.",
        );

        // Worktree has no reviewers — should fall back to repo
        let roles = load_reviewer_roles(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, "security");
    }

    #[test]
    fn test_load_reviewer_roles_no_directory() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let result = load_reviewer_roles(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No .conductor/reviewers/ directory"));
        assert!(err.contains("conductor-ai"));
    }

    #[test]
    fn test_load_reviewer_roles_empty_directory() {
        let worktree = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        // Put the empty dir in the worktree — it will be found and then error on no .md files
        fs::create_dir_all(worktree.path().join(".conductor").join("reviewers")).unwrap();
        let result = load_reviewer_roles(
            worktree.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
        );
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No .md files"));
    }

    #[test]
    fn test_load_reviewer_roles_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let reviewers_dir = tmp.path().join(".conductor").join("reviewers");
        fs::create_dir_all(&reviewers_dir).unwrap();
        fs::write(
            reviewers_dir.join("security.md"),
            "---\nname: security\n---\nReview.",
        )
        .unwrap();
        fs::write(reviewers_dir.join("README.txt"), "not a reviewer").unwrap();

        let roles =
            load_reviewer_roles(tmp.path().to_str().unwrap(), repo.path().to_str().unwrap())
                .unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, "security");
    }

    #[test]
    fn test_load_review_settings_defaults() {
        let tmp = TempDir::new().unwrap();
        let settings = load_review_settings(tmp.path().to_str().unwrap()).unwrap();
        assert!(settings.post_to_pr);
        assert!(settings.auto_merge);
    }

    #[test]
    fn test_load_review_settings_from_file() {
        let tmp = TempDir::new().unwrap();
        let conductor_dir = tmp.path().join(".conductor");
        fs::create_dir_all(&conductor_dir).unwrap();
        fs::write(
            conductor_dir.join("review.toml"),
            "post_to_pr = false\nauto_merge = false\n",
        )
        .unwrap();

        let settings = load_review_settings(tmp.path().to_str().unwrap()).unwrap();
        assert!(!settings.post_to_pr);
        assert!(!settings.auto_merge);
    }

    #[test]
    fn test_load_review_settings_partial() {
        let tmp = TempDir::new().unwrap();
        let conductor_dir = tmp.path().join(".conductor");
        fs::create_dir_all(&conductor_dir).unwrap();
        fs::write(conductor_dir.join("review.toml"), "auto_merge = false\n").unwrap();

        let settings = load_review_settings(tmp.path().to_str().unwrap()).unwrap();
        assert!(settings.post_to_pr); // default
        assert!(!settings.auto_merge);
    }

    #[test]
    fn test_frontmatter_with_all_fields() {
        let content = "---\n\
            name: security\n\
            description: Input validation, auth gaps, injection risks\n\
            model: opus\n\
            required: true\n\
            color: red\n\
            source: github:LivelyVideo/claude-plugin-marketplace/plugins/base-agents/agents/security.md\n\
            ---\n\
            You are a security reviewer.";
        let (yaml, body) = parse_frontmatter(content).unwrap();

        let fm: ReviewerFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.name.unwrap(), "security");
        assert_eq!(
            fm.description.unwrap(),
            "Input validation, auth gaps, injection risks"
        );
        assert_eq!(fm.model.unwrap(), "opus");
        assert!(fm.required);
        assert_eq!(fm.color.unwrap(), "red");
        assert!(fm.source.unwrap().contains("claude-plugin-marketplace"));
        assert_eq!(body, "You are a security reviewer.");
    }
}
