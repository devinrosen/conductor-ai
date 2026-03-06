//! File-based reviewer configuration for multi-agent PR review swarms.
//!
//! Reads `.conductor/reviewers/*.md` from the repo worktree. Each file uses
//! YAML frontmatter + markdown body (same format as the claude-plugin-marketplace).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{ConductorError, Result};

/// YAML frontmatter fields for a reviewer role `.md` file.
#[derive(Debug, Clone, Deserialize)]
struct ReviewerFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[allow(dead_code)]
    model: Option<String>,
    #[serde(default = "default_true")]
    required: bool,
    #[allow(dead_code)]
    color: Option<String>,
    #[allow(dead_code)]
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

    let fm: ReviewerFrontmatter = serde_yaml::from_str(frontmatter).map_err(|e| {
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

/// Load all reviewer roles from `.conductor/reviewers/*.md` in the given worktree path.
///
/// Returns an error with a helpful message if the directory doesn't exist.
pub fn load_reviewer_roles(worktree_path: &str) -> Result<Vec<ReviewerRole>> {
    let reviewers_dir = PathBuf::from(worktree_path)
        .join(".conductor")
        .join("reviewers");

    if !reviewers_dir.is_dir() {
        return Err(ConductorError::Config(format!(
            "No .conductor/reviewers/ directory found in {}. \
             Create it and add reviewer role .md files. \
             See .conductor/reviewers/ in conductor-ai for reference roles.",
            worktree_path
        )));
    }

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
            "No .md files found in {}. Add reviewer role files. \
             See .conductor/reviewers/ in conductor-ai for reference roles.",
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
    fn test_load_reviewer_roles() {
        let tmp = TempDir::new().unwrap();
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

        let roles = load_reviewer_roles(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(roles.len(), 2);
        // Sorted by filename
        assert_eq!(roles[0].name, "architecture");
        assert_eq!(roles[1].name, "security");
    }

    #[test]
    fn test_load_reviewer_roles_no_directory() {
        let tmp = TempDir::new().unwrap();
        let result = load_reviewer_roles(tmp.path().to_str().unwrap());
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No .conductor/reviewers/ directory"));
        assert!(err.contains("conductor-ai"));
    }

    #[test]
    fn test_load_reviewer_roles_empty_directory() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".conductor").join("reviewers")).unwrap();
        let result = load_reviewer_roles(tmp.path().to_str().unwrap());
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("No .md files"));
    }

    #[test]
    fn test_load_reviewer_roles_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let reviewers_dir = tmp.path().join(".conductor").join("reviewers");
        fs::create_dir_all(&reviewers_dir).unwrap();
        fs::write(
            reviewers_dir.join("security.md"),
            "---\nname: security\n---\nReview.",
        )
        .unwrap();
        fs::write(reviewers_dir.join("README.txt"), "not a reviewer").unwrap();

        let roles = load_reviewer_roles(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, "security");
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

        let fm: ReviewerFrontmatter = serde_yaml::from_str(yaml).unwrap();
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
