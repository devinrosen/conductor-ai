/// Infer the base branch from a ticket's raw_json.
///
/// Returns `Some((branch_name, milestone_title))` if the ticket has a milestone and a
/// `release/<milestone_title>` branch exists on the remote origin of `repo_path`.
/// Returns `None` if the ticket has no milestone, if the remote branch does not exist,
/// or if any step fails.
pub fn infer_base_branch(raw_json: &str, repo_path: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(raw_json).ok()?;
    let title = value["milestone"]["title"].as_str()?;
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
        let repo_path = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
        let raw = with_milestone("99.99.99-nonexistent");
        // git ls-remote may fail (no remote) or return empty — either way, None.
        let result = infer_base_branch(&raw, repo_path.to_str().unwrap_or("/tmp"));
        assert!(result.is_none());
    }
}
