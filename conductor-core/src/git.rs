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
    let program = cmd.get_program().to_string_lossy().to_string();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
    let output = cmd.output().map_err(|e| {
        ConductorError::Git(format!(
            "failed to spawn `{program} {}`: {e}",
            args.join(" ")
        ))
    })?;
    if !output.status.success() {
        return Err(ConductorError::Git(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(output)
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
