use std::path::Path;
use std::process::Command;

use crate::error::{ConductorError, Result};
use crate::git::{check_gh_output, check_output, git_in};

/// Resolve the base branch for a repo using a priority order:
/// 1. The configured default branch (from DB) if it exists locally
/// 2. `git symbolic-ref refs/remotes/origin/HEAD` (remote default)
/// 3. Fall back to `main`, then `master`
/// 4. Final fallback: return the configured default regardless
pub(super) fn resolve_base_branch(repo_path: &str, configured_default: &str) -> String {
    if branch_exists(repo_path, configured_default) {
        return configured_default.to_string();
    }

    if let Some(branch) = detect_remote_head(repo_path) {
        return branch;
    }

    for name in &["main", "master"] {
        if branch_exists(repo_path, name) {
            return name.to_string();
        }
    }

    configured_default.to_string()
}

/// Check if a local branch exists.
pub(super) fn branch_exists(repo_path: &str, branch: &str) -> bool {
    git_in(repo_path)
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Detect the default branch from the remote's HEAD ref.
pub(super) fn detect_remote_head(repo_path: &str) -> Option<String> {
    let output = git_in(repo_path)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let refname = String::from_utf8_lossy(&output.stdout).trim().to_string();
    refname
        .strip_prefix("refs/remotes/origin/")
        .map(|s| s.to_string())
}

/// Ensure the base branch is up to date with the remote before creating a worktree.
///
/// Returns a list of non-fatal warnings (fetch failure, diverged branch, etc.).
/// Returns `Err` only for hard failures like a dirty working tree.
pub(super) fn ensure_base_up_to_date(repo_path: &str, base_branch: &str) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // 1. Check for uncommitted changes in the repo working tree
    let output = git_in(repo_path).args(["status", "--porcelain"]).output()?;
    if output.status.success() && !output.stdout.is_empty() {
        return Err(ConductorError::Git(
            "uncommitted changes on base branch, please commit or stash first".to_string(),
        ));
    }

    // 2. Fetch from remote (soft failure — warn and allow local-only creation)
    let fetch = git_in(repo_path).args(["fetch", "origin"]).output();
    match fetch {
        Ok(o) if o.status.success() => {}
        _ => {
            warnings.push(
                "could not fetch from origin; creating worktree from local state".to_string(),
            );
            return Ok(warnings);
        }
    }

    // 3. Check if the remote tracking branch exists
    let remote_ref = format!("refs/remotes/origin/{base_branch}");
    let has_remote = git_in(repo_path)
        .args(["rev-parse", "--verify", &remote_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_remote {
        return Ok(warnings);
    }

    // 4. Determine which branch is currently checked out
    let current_branch = git_in(repo_path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // 5. Fast-forward the base branch
    let origin_ref = format!("origin/{base_branch}");
    if current_branch == base_branch {
        // Base is already checked out — merge directly
        let merge = git_in(repo_path)
            .args(["merge", "--ff-only", &origin_ref])
            .output();
        if !merge.map(|o| o.status.success()).unwrap_or(false) {
            warnings.push(format!(
                "base branch '{}' has diverged from origin; consider `git pull --rebase`",
                base_branch
            ));
        }
    } else {
        // Need to checkout base branch first (handles detached HEAD too)
        let checkout = git_in(repo_path)
            .args(["switch", "--", base_branch])
            .output();
        match checkout {
            Ok(o) if o.status.success() => {
                let merge = git_in(repo_path)
                    .args(["merge", "--ff-only", &origin_ref])
                    .output();
                if !merge.map(|o| o.status.success()).unwrap_or(false) {
                    warnings.push(format!(
                        "base branch '{}' has diverged from origin; consider `git pull --rebase`",
                        base_branch
                    ));
                }
            }
            _ => {
                warnings.push(format!(
                    "could not checkout '{}'; creating worktree from local state",
                    base_branch
                ));
            }
        }
    }

    Ok(warnings)
}

/// Remove the git worktree directory and delete the associated branch.
/// Both operations are best-effort: failures are logged but not propagated because the
/// worktree or branch may already be gone (e.g. manually removed).
pub(super) fn remove_git_artifacts(repo_path: &str, worktree_path: &str, branch: &str) {
    match git_in(repo_path)
        .args(["worktree", "remove", worktree_path, "--force"])
        .output()
    {
        Ok(o) if !o.status.success() => {
            tracing::warn!(
                repo = repo_path,
                worktree = worktree_path,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "git worktree remove failed"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = repo_path,
                worktree = worktree_path,
                error = %e,
                "failed to run git worktree remove"
            );
        }
        Ok(_) => {}
    }

    match git_in(repo_path)
        .args(["branch", "-D", "--", branch])
        .output()
    {
        Ok(o) if !o.status.success() => {
            tracing::warn!(
                repo = repo_path,
                branch = branch,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "git branch -D failed"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = repo_path,
                branch = branch,
                error = %e,
                "failed to run git branch -D"
            );
        }
        Ok(_) => {}
    }
}

/// Delete a remote branch via `git push origin --delete <branch>`.
/// Best-effort: failures are logged but not propagated.
pub(super) fn delete_remote_branch(repo_path: &str, branch: &str) {
    match git_in(repo_path)
        .args(["push", "origin", "--delete", "--", branch])
        .output()
    {
        Ok(o) if !o.status.success() => {
            tracing::warn!(
                repo = repo_path,
                branch = branch,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "git push origin --delete failed (remote branch may already be gone)"
            );
        }
        Err(e) => {
            tracing::warn!(
                repo = repo_path,
                branch = branch,
                error = %e,
                "failed to run git push origin --delete"
            );
        }
        Ok(_) => {
            tracing::info!(repo = repo_path, branch = branch, "deleted remote branch");
        }
    }
}

/// Clone a remote repository into `local_path`.
/// Uses `git clone -- <remote_url> <local_path>` so that a `remote_url`
/// starting with `-` cannot be misinterpreted as a flag.
pub(super) fn clone_repo(remote_url: &str, local_path: &str) -> Result<()> {
    check_output(Command::new("git").args(["clone", "--", remote_url, local_path]))?;
    Ok(())
}

/// Parse the raw output of the `gh pr view` jq expression used in
/// `fetch_pr_branch`.  The expected format is:
///
/// ```text
/// <head_branch>|<base_branch>|<head_owner>/<head_repo>|<true|false>
/// ```
///
/// The fourth field is the value of `isCrossRepository` (true for fork PRs).
///
/// Returns `(head_branch, base_branch, head_repo, is_fork)`.
pub(super) fn parse_pr_view_output(raw: &str) -> Result<(String, String, String, bool)> {
    let raw = raw.trim();
    let parts: Vec<&str> = raw.splitn(4, '|').collect();
    if parts.len() < 4 {
        return Err(ConductorError::GhCli(format!(
            "unexpected gh pr view output: {raw}"
        )));
    }
    let head_branch = parts[0].to_string();
    let base_branch = parts[1].to_string();
    let head_repo = parts[2].to_string();
    let is_fork = parts[3].trim() == "true";
    Ok((head_branch, base_branch, head_repo, is_fork))
}

/// Fetch a PR's branch from the appropriate remote and return `(head_branch,
/// base_branch)`.
///
/// For same-repo PRs the branch is fetched from `origin`.  For fork PRs the
/// fork remote is added temporarily (or reused if it already exists) and the
/// branch is fetched from there.
pub(super) fn fetch_pr_branch(repo_path: &str, pr_number: u32) -> Result<(String, String)> {
    // 1. Resolve the PR's head branch name, base branch, and repository info
    let output = check_gh_output(
        Command::new("gh")
            .args([
                "pr",
                "view",
                &pr_number.to_string(),
                "--json",
                "headRefName,baseRefName,headRepository,isCrossRepository",
                "--jq",
                ".headRefName + \"|\" + .baseRefName + \"|\" + .headRepository.owner.login + \"/\" + .headRepository.name + \"|\" + (.isCrossRepository | tostring)",
            ])
            .current_dir(repo_path),
    )?;

    let raw = String::from_utf8_lossy(&output.stdout);
    let (head_branch, base_branch, head_repo, is_fork) = parse_pr_view_output(&raw)?;

    if is_fork {
        // For fork PRs: add the fork as a named remote (using the owner login)
        // and fetch from it.
        let fork_owner = head_repo.split('/').next().unwrap_or(&head_repo);

        validate_remote_name(fork_owner)?;

        // Look up the fork's clone URL via gh api
        let url_output = check_gh_output(
            Command::new("gh")
                .args(["api", &format!("repos/{head_repo}"), "--jq", ".clone_url"])
                .current_dir(repo_path),
        )?;

        let fork_url = String::from_utf8_lossy(&url_output.stdout)
            .trim()
            .to_string();

        // Add the remote if it doesn't already exist (ignore failure if it does)
        let _ = git_in(repo_path)
            .args(["remote", "add", fork_owner, &fork_url])
            .output();

        // Fetch the branch from the fork remote
        check_output(git_in(repo_path).args([
            "fetch",
            fork_owner,
            &format!("{head_branch}:{head_branch}"),
        ]))?;
    } else {
        // Same-repo PR: fetch from origin
        check_output(git_in(repo_path).args([
            "fetch",
            "origin",
            &format!("{head_branch}:{head_branch}"),
        ]))?;
    }

    Ok((head_branch, base_branch))
}

/// Validate that `name` is safe to use as a git remote name.
///
/// Rejects names that are empty, start with `-` (would be parsed as a git flag),
/// or contain characters that are unsafe in git remote names.
pub(super) fn validate_remote_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConductorError::InvalidInput(
            "fork owner name is empty".to_string(),
        ));
    }
    if name.starts_with('-') {
        return Err(ConductorError::InvalidInput(format!(
            "fork owner name {name:?} starts with '-' and would be interpreted as a git flag"
        )));
    }
    let unsafe_chars: &[char] = &[' ', '\t', '\n', '\\', ':', '?', '*', '[', '^', '~', '\0'];
    if let Some(c) = name.chars().find(|c| unsafe_chars.contains(c)) {
        return Err(ConductorError::InvalidInput(format!(
            "fork owner name {name:?} contains unsafe character {c:?}"
        )));
    }
    Ok(())
}

/// Detect package manager and install dependencies if applicable.
pub(super) fn install_deps(worktree_path: &Path) {
    if worktree_path.join("package.json").exists() {
        // Detect lockfile to choose the right package manager
        let pm = if worktree_path.join("bun.lockb").exists()
            || worktree_path.join("bun.lock").exists()
        {
            "bun"
        } else if worktree_path.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if worktree_path.join("yarn.lock").exists() {
            "yarn"
        } else {
            "npm"
        };
        let _ = Command::new(pm)
            .arg("install")
            .current_dir(worktree_path)
            .output();
    }
}
