use std::path::PathBuf;

/// Expand a leading `~` in a path string to the user's home directory.
///
/// Returns `Err` with a descriptive message if `~` is present but the home
/// directory cannot be determined. Paths that do not start with `~` are
/// returned unchanged as a `PathBuf`.
pub fn expand_tilde(path: &str) -> Result<PathBuf, String> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = dirs::home_dir()
            .ok_or_else(|| "cannot expand '~': home directory is unavailable".to_string())?;
        Ok(home.join(rest))
    } else if path == "~" {
        let home = dirs::home_dir()
            .ok_or_else(|| "cannot expand '~': home directory is unavailable".to_string())?;
        Ok(home)
    } else {
        Ok(PathBuf::from(path))
    }
}

/// Resolve a `.conductor/<subdir>` directory, preferring `worktree_path` over `repo_path`.
///
/// Returns `Some(path)` for the first existing directory found, or `None` if neither exists.
pub fn resolve_conductor_subdir(
    worktree_path: &str,
    repo_path: &str,
    subdir: &str,
) -> Option<PathBuf> {
    if !worktree_path.is_empty() {
        let worktree_dir = PathBuf::from(worktree_path).join(".conductor").join(subdir);
        if worktree_dir.is_dir() {
            return Some(worktree_dir);
        }
    }
    let repo_dir = PathBuf::from(repo_path).join(".conductor").join(subdir);
    if repo_dir.is_dir() {
        return Some(repo_dir);
    }
    None
}

/// Resolve a `.conductor/<subdir>` directory for a specific file, preferring `worktree_path`
/// over `repo_path`, but only committing to a candidate when the specific file exists there.
///
/// Unlike `resolve_conductor_subdir` (which stops at the first *directory* that exists),
/// this function gates on file existence so that a worktree that has the directory but not
/// the specific file falls through to the repo root.
///
/// Returns `Some(dir)` — the directory containing the file — or `None` if the file
/// is absent from both locations.
pub fn resolve_conductor_subdir_for_file(
    worktree_path: &str,
    repo_path: &str,
    subdir: &str,
    filename: &str,
) -> Option<PathBuf> {
    if !worktree_path.is_empty() {
        let dir = PathBuf::from(worktree_path).join(".conductor").join(subdir);
        if dir.join(filename).is_file() {
            return Some(dir);
        }
    }
    let dir = PathBuf::from(repo_path).join(".conductor").join(subdir);
    if dir.join(filename).is_file() {
        return Some(dir);
    }
    None
}

/// Truncate a string at a char boundary no greater than `max_bytes`.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backwards from max_bytes to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate `s` to at most `max` bytes (on a char boundary) and append `suffix` when truncated.
pub fn cap_with_suffix(s: &str, max: usize, suffix: &str) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let truncated = truncate_str(s, max);
        let mut out = String::with_capacity(truncated.len() + suffix.len());
        out.push_str(truncated);
        out.push_str(suffix);
        out
    }
}

/// Split a file's content into (frontmatter_yaml, body).
///
/// Returns `None` if the content doesn't start with `---` or has no closing `---`.
pub fn parse_frontmatter(content: &str) -> Option<(&str, &str)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
    let close_pos = after_open.find("\n---")?;
    let yaml = &after_open[..close_pos];
    let rest = &after_open[close_pos + 4..]; // skip "\n---"
    let body = rest.strip_prefix('\n').unwrap_or(rest);
    Some((yaml, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_resolve_conductor_subdir_empty_worktree_path() {
        let repo_dir = TempDir::new().unwrap();
        let workflows = repo_dir.path().join(".conductor").join("workflows");
        fs::create_dir_all(&workflows).unwrap();

        // Empty worktree_path must not be resolved; repo_path should be used instead.
        let result = resolve_conductor_subdir("", repo_dir.path().to_str().unwrap(), "workflows");
        assert_eq!(result, Some(workflows));
    }

    #[test]
    fn test_resolve_conductor_subdir_nonempty_worktree_path_preferred() {
        let repo_dir = TempDir::new().unwrap();
        let wt_dir = TempDir::new().unwrap();
        let repo_workflows = repo_dir.path().join(".conductor").join("workflows");
        let wt_workflows = wt_dir.path().join(".conductor").join("workflows");
        fs::create_dir_all(&repo_workflows).unwrap();
        fs::create_dir_all(&wt_workflows).unwrap();

        // When both exist, worktree_path should be preferred.
        let result = resolve_conductor_subdir(
            wt_dir.path().to_str().unwrap(),
            repo_dir.path().to_str().unwrap(),
            "workflows",
        );
        assert_eq!(result, Some(wt_workflows));
    }

    // ── resolve_conductor_subdir_for_file ──────────────────────────────────

    #[test]
    fn test_resolve_for_file_file_in_worktree() {
        let wt = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let wt_dir = wt.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wt_dir).unwrap();
        fs::write(wt_dir.join("deploy.wf"), "content").unwrap();

        let result = resolve_conductor_subdir_for_file(
            wt.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "workflows",
            "deploy.wf",
        );
        assert_eq!(result, Some(wt_dir));
    }

    #[test]
    fn test_resolve_for_file_dir_in_worktree_file_only_in_repo() {
        // Bug case: worktree has the directory but not the specific file.
        let wt = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        // Create directory in worktree but NOT the file.
        fs::create_dir_all(wt.path().join(".conductor").join("workflows")).unwrap();
        // Create file only in repo.
        let repo_dir = repo.path().join(".conductor").join("workflows");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(repo_dir.join("deploy.wf"), "content").unwrap();

        let result = resolve_conductor_subdir_for_file(
            wt.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "workflows",
            "deploy.wf",
        );
        assert_eq!(result, Some(repo_dir));
    }

    #[test]
    fn test_resolve_for_file_both_have_file_worktree_wins() {
        let wt = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let wt_dir = wt.path().join(".conductor").join("workflows");
        let repo_dir = repo.path().join(".conductor").join("workflows");
        fs::create_dir_all(&wt_dir).unwrap();
        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(wt_dir.join("deploy.wf"), "wt content").unwrap();
        fs::write(repo_dir.join("deploy.wf"), "repo content").unwrap();

        let result = resolve_conductor_subdir_for_file(
            wt.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "workflows",
            "deploy.wf",
        );
        assert_eq!(result, Some(wt_dir));
    }

    #[test]
    fn test_resolve_for_file_absent_from_both_returns_none() {
        let wt = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let result = resolve_conductor_subdir_for_file(
            wt.path().to_str().unwrap(),
            repo.path().to_str().unwrap(),
            "workflows",
            "deploy.wf",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_for_file_empty_worktree_uses_repo() {
        let repo = TempDir::new().unwrap();
        let repo_dir = repo.path().join(".conductor").join("workflows");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(repo_dir.join("deploy.wf"), "content").unwrap();

        let result = resolve_conductor_subdir_for_file(
            "",
            repo.path().to_str().unwrap(),
            "workflows",
            "deploy.wf",
        );
        assert_eq!(result, Some(repo_dir));
    }

    #[test]
    fn test_truncate_str_multibyte() {
        assert_eq!(truncate_str("ééé", 3), "é"); // 3 < 4, backs up to 2
        assert_eq!(truncate_str("ééé", 4), "éé");

        assert_eq!(truncate_str("🦀x", 2), ""); // can't fit the crab
        assert_eq!(truncate_str("🦀x", 4), "🦀");
        assert_eq!(truncate_str("🦀x", 5), "🦀x");

        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 3), "hel");
    }

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = "---\nname: test\n---\nbody text";
        let (yaml, body) = parse_frontmatter(content).unwrap();
        assert_eq!(yaml, "name: test");
        assert_eq!(body, "body text");
    }

    #[test]
    fn test_parse_frontmatter_no_opening() {
        assert!(parse_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn test_parse_frontmatter_no_closing() {
        assert!(parse_frontmatter("---\nyaml without closing").is_none());
    }
}
