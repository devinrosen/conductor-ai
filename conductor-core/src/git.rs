use std::process::Command;

use crate::error::{ConductorError, Result, SubprocessFailure};

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
///
/// Constructs a `SubprocessFailure` with structured data (exit code, stderr, stdout)
/// so callers can programmatically classify and handle errors.
fn run_command(
    cmd: &mut Command,
    make_err: fn(SubprocessFailure) -> ConductorError,
) -> Result<std::process::Output> {
    let program = cmd.get_program().to_string_lossy().to_string();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
    let cmd_str = format!("`{program} {}`", args.join(" "));
    let output = cmd.output().map_err(|e| {
        make_err(SubprocessFailure::from_message(
            &cmd_str,
            format!("failed to spawn {cmd_str}: {e}"),
        ))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(make_err(SubprocessFailure {
            command: cmd_str,
            exit_code: output.status.code(),
            stderr,
            stdout,
        }));
    }
    Ok(output)
}

/// Run a git command with bounded retry for transient failures.
///
/// `build_cmd` is called on each attempt to produce a fresh `Command`
/// (since `Command::output()` consumes the command's state).
///
/// Part of: bounded-retry-with-escalation@1.0.0
#[allow(dead_code)]
pub(crate) fn check_output_with_retry<F>(
    config: &crate::retry::RetryConfig,
    build_cmd: F,
) -> Result<std::process::Output>
where
    F: Fn() -> Command,
{
    check_with_retry(config, build_cmd, ConductorError::Git)
}

/// Run a gh CLI command with bounded retry for transient failures.
///
/// Part of: bounded-retry-with-escalation@1.0.0
#[allow(dead_code)]
pub(crate) fn check_gh_output_with_retry<F>(
    config: &crate::retry::RetryConfig,
    build_cmd: F,
) -> Result<std::process::Output>
where
    F: Fn() -> Command,
{
    check_with_retry(config, build_cmd, ConductorError::GhCli)
}

#[allow(dead_code)]
fn check_with_retry<F>(
    config: &crate::retry::RetryConfig,
    build_cmd: F,
    make_err: fn(SubprocessFailure) -> ConductorError,
) -> Result<std::process::Output>
where
    F: Fn() -> Command,
{
    let outcome = crate::retry::retry_with_backoff(
        config,
        || run_command(&mut build_cmd(), make_err),
        |err: &ConductorError| match err {
            ConductorError::Git(f) | ConductorError::GhCli(f) => crate::retry::is_transient(f),
            _ => false,
        },
        || false, // No cancellation support at this level; callers run in background threads
    );
    match outcome {
        crate::retry::RetryOutcome::Success { value, .. } => Ok(value),
        crate::retry::RetryOutcome::Exhausted { last_error, .. } => Err(last_error),
        crate::retry::RetryOutcome::Cancelled => {
            unreachable!("cancellation predicate is || false")
        }
    }
}

/// Check if a local branch exists in the given repo.
///
/// Runs `git branch --list <branch>` and returns `true` if the output is non-empty.
/// This is a fast, local-only operation (no network).
///
/// Returns `Err` if the git subprocess fails to run (e.g. git not installed,
/// invalid repo path) so callers can distinguish "branch absent" from
/// "unable to check".
pub(crate) fn local_branch_exists(repo_path: &str, branch: &str) -> Result<bool> {
    let cmd_str = format!("`git branch --list -- {branch}`");
    let output = git_in(repo_path)
        .args(["branch", "--list", "--", branch])
        .output()
        .map_err(|e| {
            ConductorError::Git(SubprocessFailure::from_message(
                &cmd_str,
                format!("failed to spawn {cmd_str}: {e}"),
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(ConductorError::Git(SubprocessFailure {
            command: cmd_str,
            exit_code: output.status.code(),
            stderr,
            stdout,
        }));
    }
    Ok(!output.stdout.is_empty())
}

/// Check if `branch` has been merged into `default_branch` using local refs
/// (`git branch --merged`). Fast but may be stale if the remote has advanced.
pub(crate) fn is_branch_merged_local(repo_path: &str, branch: &str, default_branch: &str) -> bool {
    let output = git_in(repo_path)
        .args(["branch", &format!("--merged={default_branch}")])
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
            matches!(&err, ConductorError::Git(f) if f.stderr.contains("oops")),
            "expected Git variant with stderr, got: {err:?}"
        );
    }

    #[test]
    fn check_gh_output_nonzero_exit_returns_ghcli_error() {
        let err =
            check_gh_output(Command::new("sh").args(["-c", "echo bad >&2; exit 1"])).unwrap_err();
        assert!(
            matches!(&err, ConductorError::GhCli(f) if f.stderr.contains("bad")),
            "expected GhCli variant with stderr, got: {err:?}"
        );
    }

    #[test]
    fn check_gh_output_empty_stderr_includes_exit_status() {
        let err = check_gh_output(Command::new("sh").args(["-c", "exit 42"])).unwrap_err();
        assert!(
            matches!(&err, ConductorError::GhCli(f) if f.exit_code == Some(42)),
            "expected GhCli variant with exit code 42, got: {err:?}"
        );
    }

    #[test]
    fn check_gh_output_spawn_failure_returns_ghcli_error() {
        let err = check_gh_output(&mut Command::new("__nonexistent_binary_xyz__")).unwrap_err();
        assert!(
            matches!(&err, ConductorError::GhCli(f) if f.stderr.contains("failed to spawn")),
            "expected GhCli variant for spawn failure, got: {err:?}"
        );
    }

    /// Create a throwaway git repo with one commit so branch refs exist.
    fn init_temp_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().to_str().unwrap();
        Command::new("git")
            .args(["init", "-b", "main"])
            .arg(p)
            .output()
            .unwrap();
        // Configure a dummy author so commit works even without global gitconfig
        for (k, v) in [("user.name", "test"), ("user.email", "t@t")] {
            Command::new("git")
                .args(["-C", p, "config", k, v])
                .output()
                .unwrap();
        }
        let out = Command::new("git")
            .args(["-C", p, "commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
        assert!(out.status.success(), "git commit failed in temp repo");
        tmp
    }

    #[test]
    fn test_local_branch_exists_true() {
        let tmp = init_temp_repo();
        assert!(local_branch_exists(tmp.path().to_str().unwrap(), "main").unwrap());
    }

    #[test]
    fn test_local_branch_exists_false() {
        let tmp = init_temp_repo();
        assert!(
            !local_branch_exists(tmp.path().to_str().unwrap(), "nonexistent-branch-xyz-12345")
                .unwrap()
        );
    }

    #[test]
    fn test_local_branch_exists_bad_path() {
        // Bad path should return an error, not silently return false
        assert!(local_branch_exists("/nonexistent/repo/path", "main").is_err());
    }

    #[test]
    fn check_output_spawn_failure_returns_git_error() {
        let err = check_output(&mut Command::new("__nonexistent_binary_xyz__")).unwrap_err();
        assert!(
            matches!(&err, ConductorError::Git(f) if f.stderr.contains("failed to spawn")),
            "expected Git variant for spawn failure, got: {err:?}"
        );
    }

    /// Regression test for #1335: branch names that look like flags must not be
    /// interpreted as git options.  The `--merged=<ref>` form (with `=`) used in
    /// `is_branch_merged_local` prevents git from treating the default_branch
    /// value as a separate flag.
    #[test]
    fn is_branch_merged_local_flag_like_default_branch_does_not_inject() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        // Initialise a tiny repo with one commit so `git branch --merged` works.
        Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(repo)
            .output()
            .unwrap();

        // A default_branch value that looks like a flag should NOT cause git to
        // interpret it as an option (the old code used a positional arg which
        // would fail here).  With the `--merged=<val>` form, git simply reports
        // "not a valid object name" on stderr and exits non-zero, so the
        // function returns false rather than crashing or deleting something.
        let result = is_branch_merged_local(repo.to_str().unwrap(), "main", "--delete");
        assert!(!result, "flag-like default_branch must not cause injection");
    }

    /// Verify the happy path: a branch merged into the default branch is detected.
    #[test]
    fn is_branch_merged_local_returns_true_for_merged_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Create and merge a feature branch.
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "feat"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["merge", "feature"])
            .current_dir(repo)
            .output()
            .unwrap();

        assert!(is_branch_merged_local(
            repo.to_str().unwrap(),
            "feature",
            "main"
        ));
    }
}
