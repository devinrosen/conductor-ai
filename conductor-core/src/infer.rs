/// Infer the base branch from a ticket's raw_json.
///
/// Returns `Some((branch_name, milestone_title))` if the ticket has a milestone and a
/// `release/<milestone_title>` branch exists on the remote origin of `repo_path`.
/// Returns `None` if the ticket has no milestone, if the remote branch does not exist,
/// or if any step fails.
pub fn infer_base_branch(raw_json: &str, repo_path: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(raw_json).ok()?;
    let title = value["milestone"]["title"].as_str()?;

    // Validate that the milestone title contains only safe characters before
    // constructing a branch name and passing it to a subprocess argument.
    // Allowed: alphanumeric, dot, hyphen, underscore.
    if !title
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return None;
    }

    let branch = format!("release/{title}");

    // Verify the branch exists on the remote via `git ls-remote --heads origin <branch>`.
    let output = std::process::Command::new("git")
        .args(["ls-remote", "--heads", "origin", &branch])
        .current_dir(repo_path)
        .output()
        .ok()?;

    if output.stdout.is_empty() {
        None
    } else {
        Some((branch, title.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build raw_json with a milestone title.
    fn with_milestone(title: &str) -> String {
        serde_json::json!({
            "number": 1,
            "title": "some ticket",
            "milestone": { "title": title, "number": 1 }
        })
        .to_string()
    }

    /// Helper: build raw_json with no milestone field.
    fn without_milestone() -> String {
        serde_json::json!({
            "number": 1,
            "title": "some ticket"
        })
        .to_string()
    }

    #[test]
    fn test_no_milestone_returns_none() {
        // No git call needed — returns None before reaching ls-remote.
        // Use a non-existent path to ensure no git call succeeds accidentally.
        let result = infer_base_branch(&without_milestone(), "/nonexistent/path");
        assert!(result.is_none());
    }

    #[test]
    fn test_malformed_json_returns_none() {
        let result = infer_base_branch("not json at all", "/nonexistent/path");
        assert!(result.is_none());
    }

    #[test]
    fn test_milestone_null_returns_none() {
        let raw = serde_json::json!({
            "number": 1,
            "title": "some ticket",
            "milestone": null
        })
        .to_string();
        let result = infer_base_branch(&raw, "/nonexistent/path");
        assert!(result.is_none());
    }

    #[test]
    fn test_milestone_branch_absent_on_remote_returns_none() {
        // Use a real git repo path but a milestone title that won't match any remote branch.
        // Find the workspace root so git ls-remote works (but branch won't exist).
        let repo_path =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
        let raw = with_milestone("99.99.99-nonexistent");
        // git ls-remote may fail (no remote) or return empty — either way, None.
        let result = infer_base_branch(&raw, repo_path.to_str().unwrap_or("/tmp"));
        assert!(result.is_none());
    }

    #[test]
    fn test_milestone_title_with_unsafe_chars_returns_none() {
        // Titles containing path-traversal or shell-special characters must be rejected
        // before reaching the subprocess call.
        let traversal = with_milestone("../etc/passwd");
        let result = infer_base_branch(&traversal, "/tmp");
        assert!(result.is_none());

        let newline = with_milestone("1.0\nmalicious");
        let result = infer_base_branch(&newline, "/tmp");
        assert!(result.is_none());

        let space = with_milestone("1.0 rc1");
        let result = infer_base_branch(&space, "/tmp");
        assert!(result.is_none());
    }

    #[test]
    fn test_milestone_branch_exists_on_remote_returns_some() {
        // Set up a minimal local bare repo acting as "origin" so git ls-remote works
        // without any network access.
        let tmp = std::env::temp_dir();
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let remote_path = tmp.join(format!("conductor-test-remote-{id}"));
        let clone_path = tmp.join(format!("conductor-test-clone-{id}"));

        // Clean up on any previous failed run.
        let _ = std::fs::remove_dir_all(&remote_path);
        let _ = std::fs::remove_dir_all(&clone_path);

        // Init bare remote.
        let ok = std::process::Command::new("git")
            .args(["init", "--bare", remote_path.to_str().unwrap()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            // git not available or failed — skip gracefully.
            let _ = std::fs::remove_dir_all(&remote_path);
            return;
        }

        // Init a working clone, configure identity, create an initial commit, then push
        // a `release/1.2.3` branch to the bare remote.
        let run = |args: &[&str], dir: &std::path::Path| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };

        std::fs::create_dir_all(&clone_path).unwrap();
        run(&["init"], &clone_path);
        run(
            &["remote", "add", "origin", remote_path.to_str().unwrap()],
            &clone_path,
        );
        // Create an empty commit so we can push a branch.
        run(&["commit", "--allow-empty", "-m", "init"], &clone_path);
        run(&["push", "origin", "HEAD:release/1.2.3"], &clone_path);

        let raw = with_milestone("1.2.3");
        let result = infer_base_branch(&raw, clone_path.to_str().unwrap());

        // Clean up before asserting so temp dirs are always removed.
        let _ = std::fs::remove_dir_all(&remote_path);
        let _ = std::fs::remove_dir_all(&clone_path);

        assert_eq!(
            result,
            Some(("release/1.2.3".to_string(), "1.2.3".to_string()))
        );
    }
}
