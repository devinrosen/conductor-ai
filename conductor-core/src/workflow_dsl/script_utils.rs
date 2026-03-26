//! Path resolution utilities for workflow script steps.
//!
//! This module is the single source of truth for the script lookup algorithm
//! used at both runtime (workflow executor) and validation time.

use crate::text_util::path_is_within_dir;

/// Returns the ordered list of `(search_root, candidate_path)` pairs for a
/// script name.
///
/// For absolute paths the root is the same as the candidate (no boundary check
/// applies).  For relative paths the order is:
/// 1. `working_dir/{run}`
/// 2. `working_dir/.conductor/scripts/{run}`
/// 3. `repo_path/{run}`
/// 4. `repo_path/.conductor/scripts/{run}`
/// 5. `skills_dir/{run}` (if set)
pub(crate) fn script_search_paths(
    run: &str,
    working_dir: &str,
    repo_path: &str,
    skills_dir: Option<&std::path::Path>,
) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
    let p = std::path::Path::new(run);
    if p.is_absolute() {
        return vec![(p.to_path_buf(), p.to_path_buf())];
    }
    let wd = std::path::Path::new(working_dir);
    let rp = std::path::Path::new(repo_path);
    let mut pairs = vec![
        (wd.to_path_buf(), wd.join(run)),
        (wd.to_path_buf(), wd.join(".conductor/scripts").join(run)),
        (rp.to_path_buf(), rp.join(run)),
        (rp.to_path_buf(), rp.join(".conductor/scripts").join(run)),
    ];
    if let Some(skills) = skills_dir {
        pairs.push((skills.to_path_buf(), skills.join(run)));
    }
    pairs
}

/// Resolve a script name to an existing path using the standard search order:
/// `working_dir` → `repo_path` → `skills_dir`.
///
/// For relative paths, every candidate is verified to stay within its search
/// root after canonicalization (defense-in-depth against path traversal and
/// symlink escapes).
///
/// Absolute paths are permitted as-is because they originate from workflow
/// authors who already control file-system layout; imposing a boundary check
/// would break legitimate use-cases (e.g. `/usr/local/bin/jq`) without
/// meaningful security benefit.
///
/// Returns `None` if no candidate path exists on the filesystem.
pub fn resolve_script_path(
    run: &str,
    working_dir: &str,
    repo_path: &str,
    skills_dir: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let pairs = script_search_paths(run, working_dir, repo_path, skills_dir);
    let is_absolute = std::path::Path::new(run).is_absolute();

    for (root, candidate) in &pairs {
        if candidate.exists() {
            // Absolute paths are trusted — see doc-comment above.
            if is_absolute {
                return Some(candidate.clone());
            }
            // Reject path traversal attempts.
            if run.contains("..") {
                continue;
            }
            // For paths under .conductor/ (the standard script location),
            // allow symlinks that point outside the search root. This is
            // intentional: .conductor/scripts/ entries are commonly symlinked
            // to external sources (e.g., fsm-engine/scripts/). We skip
            // canonicalization to avoid rejecting these valid symlinks.
            // Check the candidate path (which includes the search prefix),
            // not `run` (which may be a bare filename resolved via .conductor/scripts/).
            let relative = candidate.strip_prefix(root).unwrap_or(candidate.as_path());
            if relative.starts_with(".conductor") {
                return Some(candidate.clone());
            }
            // For other relative paths (bare filenames in working_dir, repo,
            // or skills), apply the strict canonicalize-based containment
            // check to block symlink escapes.
            if path_is_within_dir(root, candidate) {
                return Some(candidate.clone());
            }
        }
    }
    None
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
                let pairs =
                    script_search_paths(run, &working_dir, &repo_path, skills_dir.as_deref());
                let mut searched: Vec<String> =
                    pairs.iter().map(|(_, c)| c.display().to_string()).collect();
                // When no explicit skills_dir is configured, still hint the
                // default location so users know where to place skill scripts.
                if skills_dir.is_none() {
                    searched.push(format!("~/.claude/skills/{run}"));
                }
                searched.join(", ")
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
            err.contains("/tmp/wd/.conductor/scripts/missing.sh"),
            "should include working_dir .conductor/scripts path, got: {err}"
        );
        assert!(
            err.contains("/tmp/repo/missing.sh"),
            "should include repo_path path, got: {err}"
        );
        assert!(
            err.contains("/tmp/repo/.conductor/scripts/missing.sh"),
            "should include repo_path .conductor/scripts path, got: {err}"
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
        let pairs = script_search_paths("/abs/path/script.sh", "/wd", "/repo", None);
        let candidates: Vec<_> = pairs.iter().map(|(_, c)| c.clone()).collect();
        assert_eq!(
            candidates,
            vec![std::path::PathBuf::from("/abs/path/script.sh")]
        );
    }

    #[test]
    fn test_script_search_paths_relative_no_skills() {
        let pairs = script_search_paths("run.sh", "/wd", "/repo", None);
        let candidates: Vec<_> = pairs.iter().map(|(_, c)| c.clone()).collect();
        assert_eq!(
            candidates,
            vec![
                std::path::PathBuf::from("/wd/run.sh"),
                std::path::PathBuf::from("/wd/.conductor/scripts/run.sh"),
                std::path::PathBuf::from("/repo/run.sh"),
                std::path::PathBuf::from("/repo/.conductor/scripts/run.sh"),
            ]
        );
    }

    #[test]
    fn test_script_search_paths_relative_with_skills() {
        let skills = std::path::Path::new("/home/user/.claude/skills");
        let pairs = script_search_paths("my-skill.sh", "/wd", "/repo", Some(skills));
        let candidates: Vec<_> = pairs.iter().map(|(_, c)| c.clone()).collect();
        assert_eq!(
            candidates,
            vec![
                std::path::PathBuf::from("/wd/my-skill.sh"),
                std::path::PathBuf::from("/wd/.conductor/scripts/my-skill.sh"),
                std::path::PathBuf::from("/repo/my-skill.sh"),
                std::path::PathBuf::from("/repo/.conductor/scripts/my-skill.sh"),
                std::path::PathBuf::from("/home/user/.claude/skills/my-skill.sh"),
            ]
        );
    }

    #[test]
    fn test_script_search_paths_ordering() {
        let skills = std::path::Path::new("/skills");
        let pairs = script_search_paths("script.sh", "/working", "/repository", Some(skills));
        assert_eq!(pairs[0].1, std::path::PathBuf::from("/working/script.sh"));
        assert_eq!(
            pairs[1].1,
            std::path::PathBuf::from("/working/.conductor/scripts/script.sh")
        );
        assert_eq!(
            pairs[2].1,
            std::path::PathBuf::from("/repository/script.sh")
        );
        assert_eq!(
            pairs[3].1,
            std::path::PathBuf::from("/repository/.conductor/scripts/script.sh")
        );
        assert_eq!(pairs[4].1, std::path::PathBuf::from("/skills/script.sh"));
    }

    #[test]
    fn test_script_search_paths_roots_match_candidates() {
        let skills = std::path::Path::new("/skills");
        let pairs = script_search_paths("script.sh", "/working", "/repository", Some(skills));
        assert_eq!(pairs[0].0, std::path::PathBuf::from("/working"));
        assert_eq!(pairs[1].0, std::path::PathBuf::from("/working"));
        assert_eq!(pairs[2].0, std::path::PathBuf::from("/repository"));
        assert_eq!(pairs[3].0, std::path::PathBuf::from("/repository"));
        assert_eq!(pairs[4].0, std::path::PathBuf::from("/skills"));
    }

    #[test]
    fn test_resolve_script_path_rejects_traversal() {
        // Create a directory structure where ../../etc/passwd style traversal
        // would escape the working_dir boundary.
        let root = tempfile::tempdir().unwrap();
        let working = root.path().join("project").join("subdir");
        std::fs::create_dir_all(&working).unwrap();
        // Place a file two levels above working_dir (i.e. at root).
        let target = root.path().join("secret.txt");
        std::fs::write(&target, "secret").unwrap();

        let result = resolve_script_path(
            "../../secret.txt",
            working.to_str().unwrap(),
            "/nonexistent",
            None,
        );
        assert_eq!(result, None, "path traversal via ../../ must be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_script_path_rejects_symlink_escape() {
        // A symlink inside working_dir that points outside it must be rejected.
        let root = tempfile::tempdir().unwrap();
        let working = root.path().join("project");
        std::fs::create_dir_all(&working).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("evil.sh");
        std::fs::write(&target, "#!/bin/sh\necho pwned").unwrap();
        std::os::unix::fs::symlink(&target, working.join("evil.sh")).unwrap();

        let result =
            resolve_script_path("evil.sh", working.to_str().unwrap(), "/nonexistent", None);
        assert_eq!(
            result, None,
            "symlink escaping the working directory must be rejected"
        );
    }

    #[test]
    fn test_resolve_script_path_in_conductor_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let scripts_dir = dir.path().join(".conductor").join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let script = scripts_dir.join("doc-context-assembler.sh");
        std::fs::write(&script, "#!/bin/sh\necho docs").unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let result =
            resolve_script_path("doc-context-assembler.sh", "/nonexistent", repo_path, None);
        assert!(
            result.is_some(),
            "bare script name should resolve via .conductor/scripts/"
        );
        assert_eq!(result.unwrap(), script);
    }

    #[test]
    fn test_script_search_paths_no_filesystem_access() {
        // Paths are returned even when files do not exist — pure construction
        let pairs = script_search_paths("nonexistent.sh", "/no/such/dir", "/also/missing", None);
        assert_eq!(pairs.len(), 4);
        assert!(pairs[0].1.ends_with("nonexistent.sh"));
        assert!(pairs[1].1.ends_with("nonexistent.sh"));
    }
}
