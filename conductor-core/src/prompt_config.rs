//! Prompt snippet resolution for workflow steps.
//!
//! Loads `.conductor/prompts/<name>.md` files that provide reusable instruction
//! blocks appended to agent prompts at execution time via the `with` keyword.
//!
//! Resolution order for short names (first match wins):
//! 1. `.conductor/workflows/<workflow-name>/prompts/<name>.md` — workflow-local override
//! 2. `.conductor/prompts/<name>.md` — shared conductor prompts
//!
//! If the value contains `/`, treat as an explicit path relative to the repo root.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{ConductorError, Result};

/// How to locate a prompt snippet — either a short name or an explicit path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptSnippetRef {
    /// Short name (e.g. `review-diff-scope`) resolved via search order.
    Name(String),
    /// Explicit path relative to the repo root.
    Path(String),
}

impl PromptSnippetRef {
    /// Create a `PromptSnippetRef` from a raw string value.
    ///
    /// Values containing `/` or `\` are treated as explicit paths; otherwise as names.
    pub fn from_str_value(s: &str) -> Self {
        if s.contains('/') || s.contains('\\') {
            Self::Path(s.to_string())
        } else {
            Self::Name(s.to_string())
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &str {
        match self {
            Self::Name(s) | Self::Path(s) => s.as_str(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading and resolution
// ---------------------------------------------------------------------------

/// Load a prompt snippet by reference, returning its text content.
pub fn load_prompt_snippet(
    worktree_path: &str,
    repo_path: &str,
    snippet_ref: &PromptSnippetRef,
    workflow_name: Option<&str>,
) -> Result<String> {
    match snippet_ref {
        PromptSnippetRef::Name(name) => {
            load_snippet_by_name(worktree_path, repo_path, name, workflow_name)
        }
        PromptSnippetRef::Path(rel_path) => load_snippet_by_path(repo_path, rel_path),
    }
}

/// Load and concatenate multiple prompt snippets into a single string.
///
/// Each snippet is separated by `\n\n`. Returns an empty string if `refs` is empty.
pub fn load_and_concat_snippets(
    worktree_path: &str,
    repo_path: &str,
    refs: &[String],
    workflow_name: Option<&str>,
) -> Result<String> {
    if refs.is_empty() {
        return Ok(String::new());
    }

    let mut parts = Vec::with_capacity(refs.len());
    for name in refs {
        let snippet_ref = PromptSnippetRef::from_str_value(name);
        let content = load_prompt_snippet(worktree_path, repo_path, &snippet_ref, workflow_name)?;
        parts.push(content);
    }
    Ok(parts.join("\n\n"))
}

/// Check that a name does not contain path traversal components.
fn validate_name_segment(name: &str, label: &str) -> Result<()> {
    if name.contains("..") || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(ConductorError::Workflow(format!(
            "{label} '{name}' contains invalid characters (path separators or '..' are not allowed)"
        )));
    }
    Ok(())
}

/// Return the first path that is a file, checking each base.
fn find_snippet_path(bases: &[&str], subdir: &Path, filename: &str) -> Option<PathBuf> {
    bases.iter().find_map(|base| {
        let path = PathBuf::from(base).join(subdir).join(filename);
        path.is_file().then_some(path)
    })
}

/// Resolve a snippet by short name using the search order.
fn load_snippet_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
    workflow_name: Option<&str>,
) -> Result<String> {
    validate_name_segment(name, "Prompt snippet name")?;
    if let Some(wf) = workflow_name {
        validate_name_segment(wf, "Workflow name")?;
    }

    let filename = format!("{name}.md");
    let bases = [worktree_path, repo_path];

    // 1. Workflow-local override (worktree, then repo)
    if let Some(wf_name) = workflow_name {
        let subdir = Path::new(".conductor")
            .join("workflows")
            .join(wf_name)
            .join("prompts");
        if let Some(path) = find_snippet_path(&bases, &subdir, &filename) {
            return read_snippet_file(&path);
        }
    }

    // 2. Shared conductor prompts (worktree, then repo)
    if let Some(path) = find_snippet_path(&bases, Path::new(".conductor/prompts"), &filename) {
        return read_snippet_file(&path);
    }

    Err(ConductorError::Workflow(format!(
        "Prompt snippet '{name}' not found. Searched:\n\
         {}  .conductor/prompts/{filename}",
        if let Some(wf) = workflow_name {
            format!("  .conductor/workflows/{wf}/prompts/{filename}\n")
        } else {
            String::new()
        }
    )))
}

/// Resolve a snippet from an explicit path relative to the repo root.
fn load_snippet_by_path(repo_path: &str, rel_path: &str) -> Result<String> {
    if Path::new(rel_path).is_absolute() {
        return Err(ConductorError::Workflow(format!(
            "Explicit prompt snippet path '{rel_path}' must be relative, not absolute"
        )));
    }

    let repo_root = PathBuf::from(repo_path);
    let joined = repo_root.join(rel_path);

    let canonical = joined.canonicalize().map_err(|_| {
        ConductorError::Workflow(format!(
            "Prompt snippet file not found: '{rel_path}' (resolved relative to repo root '{repo_path}')"
        ))
    })?;

    let canonical_repo = repo_root.canonicalize().map_err(|e| {
        ConductorError::Workflow(format!(
            "Failed to canonicalize repo root '{repo_path}': {e}"
        ))
    })?;

    if !canonical.starts_with(&canonical_repo) {
        return Err(ConductorError::Workflow(format!(
            "Prompt snippet path '{rel_path}' escapes the repository root — path traversal is not allowed"
        )));
    }

    read_snippet_file(&canonical)
}

/// Read a snippet file and return its trimmed content.
fn read_snippet_file(path: &Path) -> Result<String> {
    let content = fs::read_to_string(path).map_err(|e| {
        ConductorError::Workflow(format!(
            "Failed to read prompt snippet file {}: {e}",
            path.display()
        ))
    })?;
    Ok(content.trim().to_string())
}

/// Check whether all referenced prompt snippets exist (for validation).
///
/// Returns a list of snippet names that could not be found.
pub fn find_missing_snippets(
    worktree_path: &str,
    repo_path: &str,
    snippet_names: &[String],
    workflow_name: Option<&str>,
) -> Vec<String> {
    snippet_names
        .iter()
        .filter(|name| {
            let snippet_ref = PromptSnippetRef::from_str_value(name);
            load_prompt_snippet(worktree_path, repo_path, &snippet_ref, workflow_name).is_err()
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_snippet_ref_from_str_name() {
        let r = PromptSnippetRef::from_str_value("review-diff-scope");
        assert_eq!(r, PromptSnippetRef::Name("review-diff-scope".to_string()));
    }

    #[test]
    fn test_snippet_ref_from_str_path() {
        let r = PromptSnippetRef::from_str_value(".conductor/prompts/custom.md");
        assert_eq!(
            r,
            PromptSnippetRef::Path(".conductor/prompts/custom.md".to_string())
        );
    }

    #[test]
    fn test_load_snippet_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join(".conductor/prompts");
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(prompts_dir.join("review-diff-scope.md"), "Get the PR diff.").unwrap();

        let result = load_snippet_by_name(
            dir.path().to_str().unwrap(),
            "/nonexistent",
            "review-diff-scope",
            None,
        );
        assert_eq!(result.unwrap(), "Get the PR diff.");
    }

    #[test]
    fn test_load_snippet_workflow_local_override() {
        let dir = tempfile::tempdir().unwrap();

        // Shared version
        let shared = dir.path().join(".conductor/prompts");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("scope.md"), "shared scope").unwrap();

        // Workflow-local override
        let wf_local = dir.path().join(".conductor/workflows/my-wf/prompts");
        fs::create_dir_all(&wf_local).unwrap();
        fs::write(wf_local.join("scope.md"), "workflow-local scope").unwrap();

        let result = load_snippet_by_name(
            dir.path().to_str().unwrap(),
            "/nonexistent",
            "scope",
            Some("my-wf"),
        );
        assert_eq!(result.unwrap(), "workflow-local scope");
    }

    #[test]
    fn test_load_snippet_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_snippet_by_name(
            dir.path().to_str().unwrap(),
            "/nonexistent",
            "missing",
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_load_and_concat_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join(".conductor/prompts");
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(prompts_dir.join("a.md"), "snippet A").unwrap();
        fs::write(prompts_dir.join("b.md"), "snippet B").unwrap();

        let result = load_and_concat_snippets(
            dir.path().to_str().unwrap(),
            "/nonexistent",
            &["a".to_string(), "b".to_string()],
            None,
        );
        assert_eq!(result.unwrap(), "snippet A\n\nsnippet B");
    }

    #[test]
    fn test_load_and_concat_empty() {
        let result = load_and_concat_snippets("/a", "/b", &[], None);
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_find_missing_snippets() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join(".conductor/prompts");
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(prompts_dir.join("exists.md"), "I exist").unwrap();

        let missing = find_missing_snippets(
            dir.path().to_str().unwrap(),
            "/nonexistent",
            &["exists".to_string(), "nope".to_string()],
            None,
        );
        assert_eq!(missing, vec!["nope".to_string()]);
    }

    #[test]
    fn test_validate_name_rejects_traversal() {
        assert!(validate_name_segment("../escape", "test").is_err());
        assert!(validate_name_segment("a/b", "test").is_err());
        assert!(validate_name_segment("valid-name", "test").is_ok());
    }

    #[test]
    fn test_load_snippet_by_path_valid() {
        let dir = tempfile::tempdir().unwrap();
        let snippets_dir = dir.path().join("docs");
        fs::create_dir_all(&snippets_dir).unwrap();
        fs::write(snippets_dir.join("rules.md"), "explicit path content").unwrap();

        let result = load_snippet_by_path(dir.path().to_str().unwrap(), "docs/rules.md");
        assert_eq!(result.unwrap(), "explicit path content");
    }

    #[test]
    fn test_load_snippet_by_path_rejects_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("rules.md").to_str().unwrap().to_string();
        let result = load_snippet_by_path(dir.path().to_str().unwrap(), &abs);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("must be relative"));
    }

    #[test]
    fn test_load_snippet_by_path_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_snippet_by_path(dir.path().to_str().unwrap(), "../../etc/passwd");
        assert!(result.is_err());
        // Either "not found" (canonicalize fails) or "escapes the repository root"
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found") || msg.contains("escapes the repository root"));
    }

    #[test]
    fn test_load_snippet_by_path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_snippet_by_path(dir.path().to_str().unwrap(), "nonexistent/file.md");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_and_concat_snippets_missing_one_errors() {
        let dir = tempfile::tempdir().unwrap();
        let prompts_dir = dir.path().join(".conductor/prompts");
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(prompts_dir.join("exists.md"), "I exist").unwrap();

        let result = load_and_concat_snippets(
            dir.path().to_str().unwrap(),
            "/nonexistent",
            &["exists".to_string(), "missing".to_string()],
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_load_snippet_by_name_falls_back_to_repo_path() {
        let worktree_dir = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();

        // Only place the snippet in repo_path, not worktree_path
        let prompts_dir = repo_dir.path().join(".conductor/prompts");
        fs::create_dir_all(&prompts_dir).unwrap();
        fs::write(prompts_dir.join("shared.md"), "repo-level content").unwrap();

        let result = load_snippet_by_name(
            worktree_dir.path().to_str().unwrap(),
            repo_dir.path().to_str().unwrap(),
            "shared",
            None,
        );
        assert_eq!(result.unwrap(), "repo-level content");
    }

    #[test]
    fn test_validate_name_rejects_backslash_and_null() {
        assert!(validate_name_segment("a\\b", "test").is_err());
        assert!(validate_name_segment("a\0b", "test").is_err());
    }
}
