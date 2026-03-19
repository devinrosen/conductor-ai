use std::process::Command;

use crate::error::{ConductorError, Result};

/// Return a `Command` for `git` rooted at `dir`.
pub(crate) fn git_in(dir: impl AsRef<std::path::Path>) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd
}

/// Run `cmd`, returning its `Output` on success or a `ConductorError::Git` on non-zero exit.
pub(crate) fn check_output(cmd: &mut Command) -> Result<std::process::Output> {
    run_command(cmd, ConductorError::Git)
}

/// Run `cmd`, returning its `Output` on success or a `ConductorError::GhCli` on non-zero exit.
pub(crate) fn check_gh_output(cmd: &mut Command) -> Result<std::process::Output> {
    run_command(cmd, ConductorError::GhCli)
}

/// Shared implementation: run a command and map failures using the given error constructor.
fn run_command(
    cmd: &mut Command,
    make_err: fn(String) -> ConductorError,
) -> Result<std::process::Output> {
    let program = cmd.get_program().to_string_lossy().to_string();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
    let cmd_str = format!("`{program} {}`", args.join(" "));
    let output = cmd
        .output()
        .map_err(|e| make_err(format!("failed to spawn {cmd_str}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("{cmd_str} exited with {}", output.status)
        } else {
            format!("{cmd_str} failed: {stderr}")
        };
        return Err(make_err(detail));
    }
    Ok(output)
}

/// Check if a local branch exists in the given repo.
///
/// Runs `git branch --list <branch>` and returns `true` if the output is non-empty.
/// This is a fast, local-only operation (no network).
pub(crate) fn local_branch_exists(repo_path: &str, branch: &str) -> bool {
    git_in(repo_path)
        .args(["branch", "--list", "--", branch])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Check if `branch` has been merged into `default_branch` using local refs
/// (`git branch --merged`). Fast but may be stale if the remote has advanced.
pub(crate) fn is_branch_merged_local(repo_path: &str, branch: &str, default_branch: &str) -> bool {
    let output = git_in(repo_path)
        .args(["branch", "--merged", default_branch])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout
                .lines()
                .any(|line| line.trim().trim_start_matches("* ") == branch)
        }
        _ => false,
    }
}

/// Check if `branch` has been merged into `base` by fetching from origin and
/// using `git merge-base --is-ancestor` on remote refs. More accurate than
/// the local variant but requires network access.
pub(crate) fn is_branch_merged_remote(repo_path: &str, branch: &str, base: &str) -> bool {
    // Fetch latest remote state (best-effort)
    let _ = git_in(repo_path)
        .args(["fetch", "origin", "--", base, branch])
        .output();

    // Check if the branch is an ancestor of the base
    git_in(repo_path)
        .args([
            "merge-base",
            "--is-ancestor",
            &format!("origin/{branch}"),
            &format!("origin/{base}"),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ConductorError;

    #[test]
    fn check_output_success() {
        let output = check_output(Command::new("echo").arg("hello")).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn check_output_nonzero_exit_returns_git_error() {
        let err =
            check_output(Command::new("sh").args(["-c", "echo oops >&2; exit 1"])).unwrap_err();
        assert!(
            matches!(&err, ConductorError::Git(msg) if msg.contains("oops")),
            "expected Git variant with stderr, got: {err:?}"
        );
    }

    #[test]
    fn check_gh_output_nonzero_exit_returns_ghcli_error() {
        let err =
            check_gh_output(Command::new("sh").args(["-c", "echo bad >&2; exit 1"])).unwrap_err();
        assert!(
            matches!(&err, ConductorError::GhCli(msg) if msg.contains("bad")),
            "expected GhCli variant with stderr, got: {err:?}"
        );
    }

    #[test]
    fn check_gh_output_empty_stderr_includes_exit_status() {
        let err = check_gh_output(Command::new("sh").args(["-c", "exit 42"])).unwrap_err();
        assert!(
            matches!(&err, ConductorError::GhCli(msg) if msg.contains("exited with")),
            "expected GhCli variant with exit status, got: {err:?}"
        );
    }

    #[test]
    fn check_gh_output_spawn_failure_returns_ghcli_error() {
        let err = check_gh_output(&mut Command::new("__nonexistent_binary_xyz__")).unwrap_err();
        assert!(
            matches!(&err, ConductorError::GhCli(msg) if msg.contains("failed to spawn")),
            "expected GhCli variant for spawn failure, got: {err:?}"
        );
    }

    #[test]
    fn test_local_branch_exists_true() {
        // "main" or "master" exists in the current repo
        let repo = env!("CARGO_MANIFEST_DIR");
        let parent = std::path::Path::new(repo).parent().unwrap();
        // The workspace root is a git repo; it should have a "main" branch
        assert!(
            local_branch_exists(parent.to_str().unwrap(), "main")
                || local_branch_exists(parent.to_str().unwrap(), "master"),
            "expected main or master to exist"
        );
    }

    #[test]
    fn test_local_branch_exists_false() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let parent = std::path::Path::new(repo).parent().unwrap();
        assert!(!local_branch_exists(
            parent.to_str().unwrap(),
            "nonexistent-branch-xyz-12345"
        ));
    }

    #[test]
    fn test_local_branch_exists_bad_path() {
        assert!(!local_branch_exists("/nonexistent/repo/path", "main"));
    }

    #[test]
    fn check_output_spawn_failure_returns_git_error() {
        let err = check_output(&mut Command::new("__nonexistent_binary_xyz__")).unwrap_err();
        assert!(
            matches!(&err, ConductorError::Git(msg) if msg.contains("failed to spawn")),
            "expected Git variant for spawn failure, got: {err:?}"
        );
    }
}
