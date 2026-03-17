//! Path resolution utilities for workflow script steps.
//!
//! This module is the single source of truth for the script lookup algorithm
//! used at both runtime (workflow executor) and validation time.

/// Returns the ordered list of candidate paths for a script name.
///
/// For absolute paths: single-element vec with the path as-is.
/// For relative paths: `[working_dir/run, repo_path/run, skills_dir/run]`.
pub(crate) fn script_search_paths(
    run: &str,
    working_dir: &str,
    repo_path: &str,
    skills_dir: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    let p = std::path::Path::new(run);
    if p.is_absolute() {
        return vec![p.to_path_buf()];
    }
    let mut paths = vec![
        std::path::Path::new(working_dir).join(run),
        std::path::Path::new(repo_path).join(run),
    ];
    if let Some(skills) = skills_dir {
        paths.push(skills.join(run));
    }
    paths
}

/// Resolve a script name to an existing path using the standard search order:
/// `working_dir` → `repo_path` → `skills_dir`.
///
/// Returns `None` if no candidate path exists on the filesystem.
pub fn resolve_script_path(
    run: &str,
    working_dir: &str,
    repo_path: &str,
    skills_dir: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    script_search_paths(run, working_dir, repo_path, skills_dir)
        .into_iter()
        .find(|p| p.exists())
}

/// Returns the default skills directory (`$HOME/.claude/skills`), or `None`
/// if the `HOME` environment variable is not set.
pub fn default_skills_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(&h).join(".claude/skills"))
}

/// Build a resolver closure suitable for passing to `validate_script_steps`.
///
/// Returns `Ok(path)` when the script is found, or `Err(searched)` where
/// `searched` is a human-readable string of the paths that were checked.
pub fn make_script_resolver(
    working_dir: String,
    repo_path: String,
    skills_dir: Option<std::path::PathBuf>,
) -> impl Fn(&str) -> Result<std::path::PathBuf, String> {
    move |run| {
        resolve_script_path(run, &working_dir, &repo_path, skills_dir.as_deref()).ok_or_else(|| {
            let p = std::path::Path::new(run);
            if p.is_absolute() {
                run.to_string()
            } else {
                let sd = skills_dir
                    .as_ref()
                    .map(|s| s.join(run).display().to_string())
                    .unwrap_or_else(|| format!("~/.claude/skills/{run}"));
                format!("{working_dir}/{run}, {repo_path}/{run}, {sd}")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_script_path_absolute_exists() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let result = resolve_script_path(&path, "/nonexistent", "/nonexistent", None);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), tmp.path());
    }

    #[test]
    fn test_resolve_script_path_absolute_missing() {
        let result = resolve_script_path("/nonexistent/path/script.sh", "/wd", "/repo", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_script_path_relative_in_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\necho hi").unwrap();
        let working_dir = dir.path().to_str().unwrap();
        let result = resolve_script_path("run.sh", working_dir, "/nonexistent", None);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), script);
    }

    #[test]
    fn test_resolve_script_path_relative_in_repo_dir() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ci.sh");
        std::fs::write(&script, "#!/bin/sh\necho ci").unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let result = resolve_script_path("ci.sh", "/nonexistent", repo_path, None);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), script);
    }

    #[test]
    fn test_resolve_script_path_not_found() {
        let result =
            resolve_script_path("totally-missing.sh", "/nonexistent", "/nonexistent", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_script_path_in_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("my-skill.sh");
        std::fs::write(&script, "#!/bin/sh\necho skill").unwrap();
        let result = resolve_script_path(
            "my-skill.sh",
            "/nonexistent",
            "/nonexistent",
            Some(dir.path()),
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap(), script);
    }

    #[test]
    fn test_make_script_resolver_found() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        let wd = dir.path().to_str().unwrap().to_string();
        let resolver = make_script_resolver(wd, "/nonexistent".to_string(), None);
        let result = resolver("run.sh");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), script);
    }

    #[test]
    fn test_make_script_resolver_not_found_relative_no_skills() {
        let resolver = make_script_resolver("/tmp/wd".to_string(), "/tmp/repo".to_string(), None);
        let result = resolver("missing.sh");
        let err = result.unwrap_err();
        assert!(
            err.contains("/tmp/wd/missing.sh"),
            "should include working_dir path, got: {err}"
        );
        assert!(
            err.contains("/tmp/repo/missing.sh"),
            "should include repo_path path, got: {err}"
        );
        assert!(
            err.contains("~/.claude/skills/missing.sh"),
            "should include default skills hint, got: {err}"
        );
    }

    #[test]
    fn test_make_script_resolver_not_found_relative_with_skills() {
        let skills = std::path::PathBuf::from("/home/user/.claude/skills");
        let resolver =
            make_script_resolver("/tmp/wd".to_string(), "/tmp/repo".to_string(), Some(skills));
        let result = resolver("deploy.sh");
        let err = result.unwrap_err();
        assert!(
            err.contains("/home/user/.claude/skills/deploy.sh"),
            "should include explicit skills path, got: {err}"
        );
    }

    #[test]
    fn test_make_script_resolver_not_found_absolute() {
        let resolver = make_script_resolver("/tmp/wd".to_string(), "/tmp/repo".to_string(), None);
        let result = resolver("/nonexistent/absolute/script.sh");
        let err = result.unwrap_err();
        assert_eq!(
            err, "/nonexistent/absolute/script.sh",
            "absolute not-found should return the path as-is"
        );
    }

    #[test]
    fn test_script_search_paths_absolute() {
        let paths = script_search_paths("/abs/path/script.sh", "/wd", "/repo", None);
        assert_eq!(paths, vec![std::path::PathBuf::from("/abs/path/script.sh")]);
    }

    #[test]
    fn test_script_search_paths_relative_no_skills() {
        let paths = script_search_paths("run.sh", "/wd", "/repo", None);
        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("/wd/run.sh"),
                std::path::PathBuf::from("/repo/run.sh"),
            ]
        );
    }

    #[test]
    fn test_script_search_paths_relative_with_skills() {
        let skills = std::path::Path::new("/home/user/.claude/skills");
        let paths = script_search_paths("my-skill.sh", "/wd", "/repo", Some(skills));
        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("/wd/my-skill.sh"),
                std::path::PathBuf::from("/repo/my-skill.sh"),
                std::path::PathBuf::from("/home/user/.claude/skills/my-skill.sh"),
            ]
        );
    }

    #[test]
    fn test_script_search_paths_ordering() {
        let skills = std::path::Path::new("/skills");
        let paths = script_search_paths("script.sh", "/working", "/repository", Some(skills));
        assert_eq!(paths[0], std::path::PathBuf::from("/working/script.sh"));
        assert_eq!(paths[1], std::path::PathBuf::from("/repository/script.sh"));
        assert_eq!(paths[2], std::path::PathBuf::from("/skills/script.sh"));
    }

    #[test]
    fn test_script_search_paths_no_filesystem_access() {
        // Paths are returned even when files do not exist — pure construction
        let paths = script_search_paths("nonexistent.sh", "/no/such/dir", "/also/missing", None);
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("nonexistent.sh"));
        assert!(paths[1].ends_with("nonexistent.sh"));
    }
}
