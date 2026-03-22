//! Verification-gated commits: gate agent commits on passing checks.
//!
//! When a workflow step has `can_commit = true` and `verify_before_commit = true`,
//! the gate runs configurable check commands (e.g. `cargo test`, `cargo clippy`)
//! after agent execution. If any check fails, the step is marked as Failed.
//!
//! Part of: verification-gated-commit-protocol@1.1.0

use std::process::Command;

use crate::git::git_in;

/// Configuration for the commit verification gate.
#[derive(Debug, Clone)]
pub struct CommitGateConfig {
    /// Shell commands to run; all must exit 0 for the gate to pass.
    pub checks: Vec<String>,
    /// Whether the gate is active.
    pub enabled: bool,
    /// Timeout per check command. Default: 5 minutes.
    pub timeout: std::time::Duration,
}

impl Default for CommitGateConfig {
    fn default() -> Self {
        Self {
            checks: vec![],
            enabled: false,
            timeout: std::time::Duration::from_secs(300),
        }
    }
}

/// Result of evaluating the commit gate.
#[derive(Debug)]
pub enum GateDecision {
    /// All checks passed.
    Accept,
    /// A check failed.
    Reject {
        failed_check: String,
        stderr: String,
        exit_code: Option<i32>,
    },
}

/// Run all commit gate checks in the given working directory.
///
/// Returns `Accept` if all checks pass (exit 0), or `Reject` with the first
/// failing check's details.
pub fn evaluate_commit_gate(
    working_dir: &str,
    config: &CommitGateConfig,
) -> crate::error::Result<GateDecision> {
    if !config.enabled || config.checks.is_empty() {
        return Ok(GateDecision::Accept);
    }

    for check in &config.checks {
        tracing::info!(check = %check, "running commit gate check");

        // Spawn and collect output with a timeout.
        // We use a separate thread for wait_with_output() to avoid pipe buffer
        // deadlocks (the OS pipe buffer is ~64KB; if a check writes more than
        // that to stdout+stderr, the child blocks on write while we block on
        // wait, creating a deadlock). The thread reads pipes to completion
        // before returning the exit status.
        let timeout = config.timeout;
        let check_clone = check.clone();
        let wd = working_dir.to_string();
        let handle = std::thread::spawn(move || {
            Command::new("sh")
                .args(["-c", &check_clone])
                .current_dir(&wd)
                .output()
        });

        // Wait for the thread with timeout
        let deadline = std::time::Instant::now() + timeout;
        let output = loop {
            if handle.is_finished() {
                match handle.join() {
                    Ok(Ok(output)) => break output,
                    Ok(Err(e)) => {
                        return Err(crate::error::ConductorError::Workflow(format!(
                            "failed to spawn commit gate check `{check}`: {e}"
                        )));
                    }
                    Err(_) => {
                        return Err(crate::error::ConductorError::Workflow(format!(
                            "commit gate check `{check}` thread panicked"
                        )));
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                // Thread is still running — we can't kill from here, but the
                // process will be cleaned up when the thread is dropped.
                return Ok(GateDecision::Reject {
                    failed_check: check.clone(),
                    stderr: format!("timed out after {timeout:?}"),
                    exit_code: None,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            tracing::warn!(
                check = %check,
                exit_code = ?output.status.code(),
                "commit gate check failed"
            );
            return Ok(GateDecision::Reject {
                failed_check: check.clone(),
                stderr,
                exit_code: output.status.code(),
            });
        }
    }

    Ok(GateDecision::Accept)
}

/// Detect commits made by an agent by comparing HEAD before and after execution.
///
/// Returns the list of new commit SHAs (oldest first).
pub fn detect_agent_commits(
    working_dir: &str,
    before_sha: &str,
) -> crate::error::Result<Vec<String>> {
    let output = match crate::git::check_output(git_in(working_dir).args([
        "log",
        "--format=%H",
        &format!("{before_sha}..HEAD"),
    ])) {
        Ok(o) => o,
        Err(_) => {
            // Non-zero exit (e.g. invalid range) — treat as no commits
            return Ok(vec![]);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let shas: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .rev() // oldest first
        .map(String::from)
        .collect();

    Ok(shas)
}

/// Capture the current HEAD SHA for later comparison.
pub fn capture_head_sha(working_dir: &str) -> Option<String> {
    crate::git::check_output(git_in(working_dir).args(["rev-parse", "HEAD"]))
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_temp_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().to_str().unwrap();
        Command::new("git")
            .args(["init", "-b", "main"])
            .arg(p)
            .output()
            .unwrap();
        for (k, v) in [("user.name", "test"), ("user.email", "t@t")] {
            Command::new("git")
                .args(["-C", p, "config", k, v])
                .output()
                .unwrap();
        }
        Command::new("git")
            .args(["-C", p, "commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
        tmp
    }

    #[test]
    fn gate_accept_when_check_passes() {
        let tmp = init_temp_repo();
        let config = CommitGateConfig {
            checks: vec!["true".to_string()],
            enabled: true,
            ..Default::default()
        };
        let result = evaluate_commit_gate(tmp.path().to_str().unwrap(), &config).unwrap();
        assert!(matches!(result, GateDecision::Accept));
    }

    #[test]
    fn gate_reject_when_check_fails() {
        let tmp = init_temp_repo();
        let config = CommitGateConfig {
            checks: vec!["false".to_string()],
            enabled: true,
            ..Default::default()
        };
        let result = evaluate_commit_gate(tmp.path().to_str().unwrap(), &config).unwrap();
        assert!(matches!(result, GateDecision::Reject { .. }));
    }

    #[test]
    fn gate_accept_when_disabled() {
        let config = CommitGateConfig {
            checks: vec!["false".to_string()],
            enabled: false,
            ..Default::default()
        };
        let result = evaluate_commit_gate("/tmp", &config).unwrap();
        assert!(matches!(result, GateDecision::Accept));
    }

    #[test]
    fn gate_accept_when_no_checks() {
        let config = CommitGateConfig {
            checks: vec![],
            enabled: true,
            ..Default::default()
        };
        let result = evaluate_commit_gate("/tmp", &config).unwrap();
        assert!(matches!(result, GateDecision::Accept));
    }

    #[test]
    fn gate_stops_at_first_failure() {
        let tmp = init_temp_repo();
        let config = CommitGateConfig {
            checks: vec![
                "true".to_string(),
                "echo fail >&2; exit 1".to_string(),
                "true".to_string(), // should not be reached
            ],
            enabled: true,
            ..Default::default()
        };
        let result = evaluate_commit_gate(tmp.path().to_str().unwrap(), &config).unwrap();
        match result {
            GateDecision::Reject {
                failed_check,
                stderr,
                ..
            } => {
                assert!(failed_check.contains("exit 1"));
                assert!(stderr.contains("fail"));
            }
            _ => panic!("expected reject"),
        }
    }

    #[test]
    fn detect_commits_with_known_history() {
        let tmp = init_temp_repo();
        let p = tmp.path().to_str().unwrap();

        let before = capture_head_sha(p).unwrap();

        // Make two commits
        Command::new("git")
            .args(["-C", p, "commit", "--allow-empty", "-m", "commit-1"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", p, "commit", "--allow-empty", "-m", "commit-2"])
            .output()
            .unwrap();

        let commits = detect_agent_commits(p, &before).unwrap();
        assert_eq!(commits.len(), 2);
    }

    #[test]
    fn detect_commits_no_new_commits() {
        let tmp = init_temp_repo();
        let p = tmp.path().to_str().unwrap();
        let before = capture_head_sha(p).unwrap();
        let commits = detect_agent_commits(p, &before).unwrap();
        assert!(commits.is_empty());
    }

    #[test]
    fn capture_head_sha_returns_sha() {
        let tmp = init_temp_repo();
        let sha = capture_head_sha(tmp.path().to_str().unwrap());
        assert!(sha.is_some());
        assert_eq!(sha.unwrap().len(), 40);
    }
}
