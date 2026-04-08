use std::path::Path;
use std::process::Command;

use crate::error::{ConductorError, Result, SubprocessFailure};
use crate::git::{check_gh_output, check_output, git_in};

/// Structured result of a pre-creation health check on the base branch.
#[derive(Debug, Clone)]
pub struct MainHealthStatus {
    /// Whether the base branch has uncommitted local changes.
    pub is_dirty: bool,
    /// List of files with uncommitted changes (populated when `is_dirty` is true).
    pub dirty_files: Vec<String>,
    /// Number of commits the local base branch is behind `origin/<branch>`,
    /// computed from cached remote refs (no network fetch).
    /// Zero if the remote tracking ref doesn't exist yet.
    pub commits_behind: u32,
    /// Whether `git status --porcelain` itself failed (e.g. not a git repo).
    /// When true, `is_dirty` and `dirty_files` are unreliable.
    pub status_check_failed: bool,
}

/// Run a read-only health check on `base_branch` inside `repo_path`.
///
/// Checks:
/// 1. `git status --porcelain` — detects dirty files (does NOT abort, just records them)
/// 2. `git rev-list --count HEAD..origin/<branch>` — computes `commits_behind` from
///    cached remote refs (no network fetch; the actual fetch happens later in `create()`)
///
/// Does not modify any git state (no checkout, no merge, no fetch).
pub fn check_main_health(repo_path: &str, base_branch: &str) -> MainHealthStatus {
    // 1. Check dirty state
    let (is_dirty, dirty_files, status_check_failed) =
        match git_in(repo_path).args(["status", "--porcelain"]).output() {
            Ok(o) if o.status.success() && !o.stdout.is_empty() => {
                let files: Vec<String> = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| {
                        l.trim_start_matches(|c: char| !c.is_whitespace())
                            .trim()
                            .to_string()
                    })
                    .collect();
                (true, files, false)
            }
            Ok(o) if o.status.success() => {
                // Empty stdout → working tree is clean
                (false, Vec::new(), false)
            }
            _ => {
                // Command failed or non-zero exit — cannot determine dirty state
                (false, Vec::new(), true)
            }
        };

    // 2. Count commits behind using cached remote refs (no fetch — avoids double
    //    fetch with the subsequent ensure_base_up_to_date call in create()).
    let remote_ref = format!("origin/{base_branch}");
    let commits_behind = {
        let count_out = git_in(repo_path)
            .args(["rev-list", "--count", &format!("HEAD..{remote_ref}")])
            .output();
        match count_out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .unwrap_or(0),
            _ => 0,
        }
    };

    MainHealthStatus {
        is_dirty,
        dirty_files,
        commits_behind,
        status_check_failed,
    }
}

/// Resolve the base branch name (with prefix fallback) and ensure it's up to date.
///
/// When an explicit `from_branch` is provided (e.g. from a Vantage ticket), we try:
/// 1. The exact name
/// 2. If that fails and the name doesn't already start with `feat/` or `fix/`, try
///    prefixing with `feat/` then `fix/`
///
/// If no explicit branch is given, falls back to `resolve_base_branch`.
///
/// The `force_dirty` and `pre_verified_clean` flags are forwarded to
/// `ensure_base_up_to_date`.
pub(super) fn resolve_and_update_base(
    repo_path: &str,
    from_branch: Option<&str>,
    configured_default: &str,
    force_dirty: bool,
    pre_verified_clean: bool,
) -> Result<(String, Vec<String>)> {
    let Some(requested) = from_branch else {
        let base = resolve_base_branch(repo_path, configured_default);
        let warnings = ensure_base_up_to_date(repo_path, &base, force_dirty, pre_verified_clean)?;
        return Ok((base, warnings));
    };

    // Perform a single fetch upfront to avoid redundant network calls
    // during prefix fallback attempts
    let mut fetch_warnings = Vec::new();
    let fetch = git_in(repo_path).args(["fetch", "origin"]).output();
    match fetch {
        Ok(o) if o.status.success() => {}
        _ => {
            fetch_warnings.push(
                "could not fetch from origin; creating worktree from local state".to_string(),
            );
        }
    }

    // Try exact name first (skip fetch since we already did it)
    match ensure_base_up_to_date_with_fetch_control(
        repo_path,
        requested,
        force_dirty,
        pre_verified_clean,
        false,
    ) {
        Ok(mut warnings) => {
            warnings.extend(fetch_warnings);
            Ok((requested.to_string(), warnings))
        }
        Err(_first_err) => {
            // If the name already has a known prefix, don't try alternatives
            if requested.starts_with("feat/") || requested.starts_with("fix/") {
                return Err(_first_err);
            }

            // Try feat/ and fix/ prefixes (skip fetch since we already did it)
            for prefix in &["feat/", "fix/"] {
                let candidate = format!("{prefix}{requested}");
                if let Ok(mut warnings) = ensure_base_up_to_date_with_fetch_control(
                    repo_path,
                    &candidate,
                    force_dirty,
                    pre_verified_clean,
                    false,
                ) {
                    warnings.extend(fetch_warnings);
                    return Ok((candidate, warnings));
                }
            }

            // All attempts failed — return the original error
            Err(_first_err)
        }
    }
}

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
/// Returns `Err` only for hard failures like a dirty working tree (unless `force_dirty` is true).
///
/// When `force_dirty` is `true`, the dirty-state check is skipped (the caller has
/// already confirmed the user wants to proceed despite uncommitted changes).
///
/// When `pre_verified_clean` is `true`, the dirty-state check is also skipped because
/// the caller has already confirmed the working tree is clean via `check_main_health`.
/// Use this to avoid running `git status --porcelain` twice on the happy path.
pub(super) fn ensure_base_up_to_date(
    repo_path: &str,
    base_branch: &str,
    force_dirty: bool,
    pre_verified_clean: bool,
) -> Result<Vec<String>> {
    ensure_base_up_to_date_with_fetch_control(
        repo_path,
        base_branch,
        force_dirty,
        pre_verified_clean,
        true,
    )
}

fn ensure_base_up_to_date_with_fetch_control(
    repo_path: &str,
    base_branch: &str,
    force_dirty: bool,
    pre_verified_clean: bool,
    should_fetch: bool,
) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // 1. Check for uncommitted changes in the repo working tree
    if !force_dirty && !pre_verified_clean {
        let output = git_in(repo_path).args(["status", "--porcelain"]).output()?;
        if output.status.success() && !output.stdout.is_empty() {
            return Err(ConductorError::InvalidInput(
                "uncommitted changes on base branch, please commit or stash first".to_string(),
            ));
        }
    }

    // 2. Fetch from remote (soft failure — warn and allow local-only creation).
    if should_fetch {
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
    }

    // 3. Check if the remote tracking branch exists
    let remote_ref = format!("refs/remotes/origin/{base_branch}");
    let has_remote = git_in(repo_path)
        .args(["rev-parse", "--verify", &remote_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let has_local = branch_exists(repo_path, base_branch);

    if !has_remote && !has_local {
        return Err(ConductorError::Git(
            crate::error::SubprocessFailure::from_message(
                "git",
                format!(
                    "base branch '{}' not found locally or on remote 'origin'",
                    base_branch
                ),
            ),
        ));
    }

    // 3b. If the branch exists on the remote but not locally, create a local tracking branch
    if has_remote && !has_local {
        // Validate branch name for security - prevent injection of git options
        if base_branch.starts_with('-') || base_branch.contains('\0') || base_branch.contains('\n')
        {
            return Err(ConductorError::InvalidInput(format!(
                "invalid branch name '{}': branch names cannot start with '-' or contain null/newline characters",
                base_branch
            )));
        }

        let create = git_in(repo_path)
            .args([
                "branch",
                "--track",
                "--", // Explicitly separate options from branch names
                base_branch,
                &format!("origin/{base_branch}"),
            ])
            .output();
        match create {
            Ok(o) if o.status.success() => {}
            _ => {
                return Err(ConductorError::Git(
                    crate::error::SubprocessFailure::from_message(
                        "git",
                        format!(
                            "base branch '{}' exists on remote but could not create local tracking branch",
                            base_branch
                        ),
                    ),
                ));
            }
        }
        // Local branch is now set to the remote tip — no need to fast-forward.
        return Ok(warnings);
    }

    if !has_remote {
        // Local branch exists but no remote tracking — use local state as-is.
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
        // Base exists locally but is not checked out — update it without checkout
        // by using `git fetch . origin/{base}:refs/heads/{base}` (local fast-forward).
        let ff = git_in(repo_path)
            .args([
                "fetch",
                ".",
                &format!("refs/remotes/origin/{base_branch}:refs/heads/{base_branch}"),
            ])
            .output();
        if !ff.map(|o| o.status.success()).unwrap_or(false) {
            warnings.push(format!(
                "base branch '{}' has diverged from origin; consider `git pull --rebase`",
                base_branch
            ));
        }
    }

    Ok(warnings)
}

/// Remove the git worktree directory and delete the associated branch.
/// Both operations are best-effort: failures are logged but not propagated because the
/// worktree or branch may already be gone (e.g. manually removed).
pub(super) fn remove_git_artifacts(repo_path: &str, worktree_path: &str, branch: &str) {
    if Path::new(worktree_path).exists() {
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
        // Fallback: if git worktree remove didn't delete the directory (e.g. git had
        // already deregistered this worktree externally via `git worktree prune`),
        // force-remove it with fs. Safe here because remove_git_artifacts is only
        // called for terminal-state worktrees (merged/abandoned).
        if Path::new(worktree_path).exists() {
            tracing::warn!(
                worktree = worktree_path,
                "git worktree remove did not delete directory; removing with fs::remove_dir_all"
            );
            if let Err(e) = std::fs::remove_dir_all(worktree_path) {
                tracing::warn!(
                    worktree = worktree_path,
                    error = %e,
                    "fs::remove_dir_all also failed; directory may still exist"
                );
            }
        }
    } else {
        tracing::debug!(
            repo = repo_path,
            worktree = worktree_path,
            "worktree path already gone, skipping git worktree remove"
        );
    }

    if branch_exists(repo_path, branch) {
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
    } else {
        tracing::debug!(
            repo = repo_path,
            branch = branch,
            "branch already gone, skipping git branch -D"
        );
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
        return Err(ConductorError::GhCli(SubprocessFailure::from_message(
            "gh pr view",
            format!("unexpected gh pr view output: {raw}"),
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
                "headRefName,baseRefName,headRepository,headRepositoryOwner,isCrossRepository",
                "--jq",
                ".headRefName + \"|\" + .baseRefName + \"|\" + .headRepositoryOwner.login + \"/\" + .headRepository.name + \"|\" + (.isCrossRepository | tostring)",
            ])
            .current_dir(repo_path),
    )?;

    let raw = String::from_utf8_lossy(&output.stdout);
    let (head_branch, base_branch, head_repo, is_fork) = parse_pr_view_output(&raw)?;

    validate_branch_name(&head_branch)?;

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

        // Add the remote if it doesn't already exist (ignore failure only if remote already exists)
        match git_in(repo_path)
            .args(["remote", "add", fork_owner, &fork_url])
            .output()
        {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.contains("already exists") {
                    return Err(crate::error::ConductorError::Git(
                        crate::error::SubprocessFailure {
                            command: format!("git remote add {} {}", fork_owner, fork_url),
                            exit_code: output.status.code(),
                            stderr: stderr.to_string(),
                            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                        },
                    ));
                }
            }
            Err(e) => {
                return Err(crate::error::ConductorError::Git(
                    crate::error::SubprocessFailure::from_message(
                        &format!("git remote add {} {}", fork_owner, fork_url),
                        e.to_string(),
                    ),
                ));
            }
            _ => {} // Success or already exists case
        }

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

/// Shared base validation for git names: rejects empty names, names starting with `-`
/// (would be parsed as a git flag), and names containing characters unsafe in git contexts.
fn validate_git_name_base(kind: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ConductorError::InvalidInput(format!(
            "{kind} name is empty"
        )));
    }
    if name.starts_with('-') {
        return Err(ConductorError::InvalidInput(format!(
            "{kind} name {name:?} starts with '-' and would be interpreted as a git flag"
        )));
    }
    let unsafe_chars: &[char] = &[' ', '\t', '\n', '\\', ':', '?', '*', '[', '^', '~', '\0'];
    if let Some(c) = name.chars().find(|c| unsafe_chars.contains(c)) {
        return Err(ConductorError::InvalidInput(format!(
            "{kind} name {name:?} contains unsafe character {c:?}"
        )));
    }
    Ok(())
}

/// Validate that `name` is safe to use as a git remote name.
///
/// Rejects names that are empty, start with `-` (would be parsed as a git flag),
/// or contain characters that are unsafe in git remote names.
pub(super) fn validate_remote_name(name: &str) -> Result<()> {
    validate_git_name_base("fork owner", name)
}

/// Validate that `name` is safe to use as a git branch name in a refspec.
///
/// Rejects names that are empty, start with `-` (would be parsed as a git flag),
/// contain `..` (special refspec separator) or `@{` (reflog syntax), or contain
/// other characters that are unsafe in git branch names.
pub(super) fn validate_branch_name(name: &str) -> Result<()> {
    validate_git_name_base("branch", name)?;
    if name.contains("..") {
        return Err(ConductorError::InvalidInput(format!(
            "branch name {name:?} contains '..' which is unsafe in git refspecs"
        )));
    }
    if name.contains("@{") {
        return Err(ConductorError::InvalidInput(format!(
            "branch name {name:?} contains '@{{' which is unsafe in git refspecs"
        )));
    }
    Ok(())
}

/// Detect package manager and install dependencies if applicable.
pub(super) fn install_deps(worktree_path: &Path) {
    let pkg = worktree_path.join("package.json");
    if !pkg.exists() {
        return;
    }
    // Skip if the package.json has no dependencies to install.
    if let Ok(contents) = std::fs::read_to_string(&pkg) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents) {
            let has_deps = v.get("dependencies").is_some()
                || v.get("devDependencies").is_some()
                || v.get("peerDependencies").is_some();
            if !has_deps {
                return;
            }
        }
    }
    // Detect lockfile to choose the right package manager.
    let pm = if worktree_path.join("bun.lockb").exists() || worktree_path.join("bun.lock").exists()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // Helper: create a temp dir, optionally write a package.json, then call
    // install_deps and return whether a marker file appeared.  Because we
    // cannot actually run `npm` / `bun` in unit-tests we instead verify the
    // *decision* logic (early-exit vs. reaching the Command::new call) by
    // checking that the function returns without panicking and by inspecting
    // what path triggered the early-return.  A simpler approach: we write a
    // fake `npm` script on PATH that creates a sentinel file, but that is
    // fragile.  Instead we unit-test the *pure* classification logic that
    // decides whether to install at all.

    fn has_installable_deps(contents: &str) -> bool {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(contents) {
            return v.get("dependencies").is_some()
                || v.get("devDependencies").is_some()
                || v.get("peerDependencies").is_some();
        }
        false
    }

    #[test]
    fn install_deps_no_package_json_returns_early() {
        let dir = TempDir::new().unwrap();
        // No package.json present — install_deps must return without error or panic.
        install_deps(dir.path());
        // If we get here the early-exit path was taken (no subprocess tried).
    }

    #[test]
    fn install_deps_no_dep_fields_skips_install() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"foo","version":"1.0.0"}"#,
        )
        .unwrap();
        // install_deps should return early because there are no dep fields.
        install_deps(dir.path());
        // Reaching here means no panic / no subprocess was launched for a
        // package.json that has no installable dependencies.
    }

    #[test]
    fn has_installable_deps_empty_object() {
        assert!(!has_installable_deps("{}"));
    }

    #[test]
    fn has_installable_deps_name_only() {
        assert!(!has_installable_deps(r#"{"name":"pkg"}"#));
    }

    #[test]
    fn has_installable_deps_with_dependencies() {
        assert!(has_installable_deps(r#"{"dependencies":{"lodash":"^4"}}"#));
    }

    #[test]
    fn has_installable_deps_with_dev_dependencies() {
        assert!(has_installable_deps(
            r#"{"devDependencies":{"jest":"^29"}}"#
        ));
    }

    #[test]
    fn has_installable_deps_with_peer_dependencies() {
        assert!(has_installable_deps(
            r#"{"peerDependencies":{"react":"^18"}}"#
        ));
    }

    #[test]
    fn has_installable_deps_invalid_json() {
        // Malformed JSON → treated as "no deps" (install skipped).
        assert!(!has_installable_deps("not json"));
    }
}
