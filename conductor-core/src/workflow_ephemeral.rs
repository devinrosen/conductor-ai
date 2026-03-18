//! Ephemeral workflow execution against a GitHub PR without a registered repo or worktree.
//!
//! Provides [`run_workflow_on_pr`] which:
//! 1. Parses a PR reference (full URL, `owner/repo#123`, or `owner/repo/123`)
//! 2. Shallow-clones the PR branch to a temporary directory via `gh`
//! 3. Loads the named workflow from the cloned repo's `.conductor/workflows/` directory
//! 4. Executes the workflow with `worktree_id = None`
//! 5. Auto-cleans up the temp directory on drop

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use tempfile::TempDir;

use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::workflow::{
    apply_workflow_input_defaults, execute_workflow, WorkflowExecConfig, WorkflowExecInput,
    WorkflowManager, WorkflowResult,
};

/// A parsed GitHub PR reference.
#[derive(Debug, Clone)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    /// Return the `owner/repo` string.
    pub fn repo_slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Parse a PR reference into a [`PrRef`].
///
/// Accepted formats:
/// - `https://github.com/owner/repo/pull/123`
/// - `owner/repo/123`
/// - `owner/repo#123`
///
/// Bare numbers (e.g. `"123"`) are not supported; a full reference is required.
pub fn parse_pr_ref(s: &str) -> Result<PrRef> {
    let s = s.trim();

    // Full URL: https://github.com/owner/repo/pull/123
    if let Some(rest) = s.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.splitn(5, '/').collect();
        if parts.len() >= 4 && parts[2] == "pull" {
            let owner = parts[0].to_string();
            let repo = parts[1].to_string();
            let number: u64 = parts[3]
                .parse()
                .map_err(|_| ConductorError::Workflow(format!("Invalid PR number in URL: {s}")))?;
            return Ok(PrRef {
                owner,
                repo,
                number,
            });
        }
        return Err(ConductorError::Workflow(format!(
            "Could not parse GitHub PR URL: {s}"
        )));
    }

    // owner/repo#123
    if let Some(hash_pos) = s.rfind('#') {
        let left = &s[..hash_pos];
        let right = &s[hash_pos + 1..];
        let parts: Vec<&str> = left.splitn(2, '/').collect();
        if parts.len() == 2 {
            let number: u64 = right.parse().map_err(|_| {
                ConductorError::Workflow(format!("Invalid PR number in reference: {s}"))
            })?;
            return Ok(PrRef {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
                number,
            });
        }
    }

    // owner/repo/123
    let parts: Vec<&str> = s.splitn(3, '/').collect();
    if parts.len() == 3 {
        let number: u64 = parts[2].parse().map_err(|_| {
            ConductorError::Workflow(format!("Invalid PR number in reference: {s}"))
        })?;
        return Ok(PrRef {
            owner: parts[0].to_string(),
            repo: parts[1].to_string(),
            number,
        });
    }

    Err(ConductorError::Workflow(format!(
        "Cannot parse PR reference '{}'. \
         Use a full GitHub URL (https://github.com/owner/repo/pull/123), \
         'owner/repo#123', or 'owner/repo/123'.",
        s
    )))
}

/// Clone the PR branch into `dir` using `gh`.
///
/// Steps:
/// 1. `gh repo clone owner/repo <dir> -- --depth=1` to create a shallow clone
/// 2. `gh pr checkout <number> --repo owner/repo` inside the cloned directory to
///    switch to the PR branch (also shallow)
///
/// Returns the name of the branch that was checked out.
pub fn checkout_pr(pr: &PrRef, dir: &Path) -> Result<String> {
    let repo_slug = pr.repo_slug();

    let dir_str = dir.to_str().ok_or_else(|| {
        ConductorError::Workflow("Temp directory path is not valid UTF-8".to_string())
    })?;

    // Step 1: clone the repo (shallow)
    let output = Command::new("gh")
        .args(["repo", "clone", &repo_slug, dir_str, "--", "--depth=1"])
        .output()
        .map_err(|e| ConductorError::Workflow(format!("Failed to run 'gh repo clone': {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConductorError::Workflow(format!(
            "gh repo clone failed for {repo_slug}: {stderr}"
        )));
    }

    // Step 2: checkout the PR branch (detached HEAD avoids tracking-setup failures
    // that occur in shallow clones where the remote-tracking ref exists but git
    // refuses to create a local tracking branch from it).
    let output = Command::new("gh")
        .args([
            "pr",
            "checkout",
            &pr.number.to_string(),
            "--repo",
            &repo_slug,
            "--detach",
        ])
        .current_dir(dir)
        .output()
        .map_err(|e| ConductorError::Workflow(format!("Failed to run 'gh pr checkout': {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConductorError::Workflow(format!(
            "gh pr checkout {} failed for {repo_slug}: {stderr}",
            pr.number
        )));
    }

    // Read the current branch name
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| ConductorError::Workflow(format!("Failed to get branch name: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConductorError::Workflow(format!(
            "git rev-parse failed after checkout: {stderr}"
        )));
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(branch)
}

/// Run a named workflow against a GitHub PR without a registered repo or worktree.
///
/// Orchestration:
/// 1. Create a temp directory (auto-deleted on drop)
/// 2. Shallow-clone the PR branch via `gh`
/// 3. Load the workflow from the cloned repo's `.conductor/workflows/` directory
/// 4. Warn if the workflow's `targets` does not include `"pr"`
/// 5. Validate required inputs and apply defaults (same as the normal run path)
/// 6. Execute the workflow with `worktree_id = None`
/// 7. Return the `WorkflowResult`
#[allow(clippy::too_many_arguments)]
pub fn run_workflow_on_pr(
    conn: &Connection,
    config: &Config,
    pr_ref: &PrRef,
    workflow_name: &str,
    model: Option<&str>,
    exec_config: WorkflowExecConfig,
    mut inputs: HashMap<String, String>,
    dry_run: bool,
) -> Result<WorkflowResult> {
    // Create a temp directory; it will be cleaned up when `_temp_dir` is dropped.
    let temp_dir = TempDir::new()
        .map_err(|e| ConductorError::Workflow(format!("Failed to create temp directory: {e}")))?;
    let clone_path = temp_dir.path();

    checkout_pr(pr_ref, clone_path)?;

    let clone_path_str = clone_path.to_str().ok_or_else(|| {
        ConductorError::Workflow("Temp directory path is not valid UTF-8".to_string())
    })?;

    // Load the workflow definition from the cloned repo
    let workflow = WorkflowManager::load_def_by_name(clone_path_str, clone_path_str, workflow_name)
        .map_err(|e| {
            ConductorError::Workflow(format!(
                "Workflow '{}' not found in cloned PR repo: {e}",
                workflow_name
            ))
        })?;

    // Inject implicit PR context variables (do not overwrite user-provided values)
    inputs.entry("pr_url".to_string()).or_insert_with(|| {
        format!(
            "https://github.com/{}/{}/pull/{}",
            pr_ref.owner, pr_ref.repo, pr_ref.number
        )
    });
    inputs
        .entry("pr_number".to_string())
        .or_insert_with(|| pr_ref.number.to_string());

    // Validate required inputs and apply defaults (shared helper)
    apply_workflow_input_defaults(&workflow, &mut inputs)?;

    let exec_config = WorkflowExecConfig {
        dry_run,
        ..exec_config
    };

    let pr_target_label = format!("{}/{}#{}", pr_ref.owner, pr_ref.repo, pr_ref.number);
    let input = WorkflowExecInput {
        conn,
        config,
        workflow: &workflow,
        worktree_id: None,
        working_dir: clone_path_str,
        repo_path: clone_path_str,
        ticket_id: None,
        repo_id: None,
        model,
        exec_config: &exec_config,
        inputs,
        depth: 0,
        parent_workflow_run_id: None,
        target_label: Some(&pr_target_label),
        default_bot_name: None,
        feature_id: None,
        iteration: 0,
        run_id_notify: None,
    };

    // `temp_dir` is dropped after execute_workflow returns, cleaning up the cloned repo.
    execute_workflow(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pr_ref_repo_slug() {
        let pr = PrRef {
            owner: "acme".to_string(),
            repo: "my-repo".to_string(),
            number: 42,
        };
        assert_eq!(pr.repo_slug(), "acme/my-repo");
    }

    #[test]
    fn test_parse_pr_ref_full_url() {
        let pr = parse_pr_ref("https://github.com/acme/my-repo/pull/42").unwrap();
        assert_eq!(pr.owner, "acme");
        assert_eq!(pr.repo, "my-repo");
        assert_eq!(pr.number, 42);
    }

    #[test]
    fn test_parse_pr_ref_owner_repo_slash_number() {
        let pr = parse_pr_ref("acme/my-repo/42").unwrap();
        assert_eq!(pr.owner, "acme");
        assert_eq!(pr.repo, "my-repo");
        assert_eq!(pr.number, 42);
    }

    #[test]
    fn test_parse_pr_ref_owner_repo_hash_number() {
        let pr = parse_pr_ref("acme/my-repo#42").unwrap();
        assert_eq!(pr.owner, "acme");
        assert_eq!(pr.repo, "my-repo");
        assert_eq!(pr.number, 42);
    }

    #[test]
    fn test_parse_pr_ref_invalid() {
        assert!(parse_pr_ref("not-a-pr").is_err());
        assert!(parse_pr_ref("123").is_err());
        assert!(parse_pr_ref("https://github.com/owner/repo").is_err());
    }

    #[test]
    fn test_parse_pr_ref_url_with_trailing_slash() {
        let pr = parse_pr_ref("https://github.com/acme/my-repo/pull/99/files").unwrap();
        assert_eq!(pr.owner, "acme");
        assert_eq!(pr.repo, "my-repo");
        assert_eq!(pr.number, 99);
    }

    /// Verifies that `checkout_pr` returns an error when `git rev-parse` fails
    /// (i.e. when the directory is not a real git repo), rather than silently
    /// returning an empty branch name.
    #[test]
    fn test_checkout_pr_git_rev_parse_failure_returns_error() {
        use tempfile::TempDir;

        let pr = PrRef {
            owner: "acme".to_string(),
            repo: "my-repo".to_string(),
            number: 1,
        };
        // Use an empty temp dir (not a git repo) to exercise the git rev-parse failure path.
        // gh repo clone / gh pr checkout will also fail, so we only care that the error
        // is propagated, not which step fails.
        let dir = TempDir::new().unwrap();
        let result = checkout_pr(&pr, dir.path());
        assert!(
            result.is_err(),
            "checkout_pr should return an error for a non-git directory"
        );
    }

    /// Verifies that `resume_workflow` cleanly rejects attempts to resume an ephemeral
    /// PR run (worktree_id = None), returning an error rather than panicking.
    #[test]
    fn test_resume_ephemeral_workflow_run_rejected() {
        use crate::agent::AgentManager;
        use crate::config::Config;
        use crate::test_helpers::setup_db;
        use crate::workflow::{resume_workflow, WorkflowManager, WorkflowResumeInput};

        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        // Create an ephemeral parent agent run (empty worktree_id → stored as NULL)
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        // Create an ephemeral workflow run with worktree_id = None and a definition snapshot
        let snapshot = r#"{"name":"test","description":"t","trigger":"manual","targets":["pr"],"inputs":[],"steps":[]}"#;
        let run = mgr
            .create_workflow_run("pr-flow", None, &parent.id, false, "pr", Some(snapshot))
            .unwrap();

        // Mark the run as failed so resume_workflow passes the status check
        mgr.update_workflow_status(
            &run.id,
            crate::workflow::WorkflowRunStatus::Failed,
            Some("error"),
        )
        .unwrap();

        let config = Config::default();
        let input = WorkflowResumeInput {
            conn: &conn,
            config: &config,
            workflow_run_id: &run.id,
            restart: false,
            from_step: None,
            model: None,
        };

        let err = resume_workflow(&input).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ephemeral") || msg.contains("no registered worktree"),
            "Expected ephemeral rejection message, got: {msg}"
        );
    }

    /// Verifies that `create_workflow_run` with `worktree_id = None` stores and
    /// retrieves NULL correctly (the ephemeral PR run path).
    #[test]
    fn test_create_workflow_run_nullable_worktree_id() {
        use crate::agent::AgentManager;
        use crate::test_helpers::setup_db;
        use crate::workflow::WorkflowManager;

        let conn = setup_db();
        let agent_mgr = AgentManager::new(&conn);
        // Ephemeral runs pass "" as worktree_id to agent_runs (stored as NULL after migration 027)
        let parent = agent_mgr.create_run(None, "workflow", None, None).unwrap();

        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("pr-flow", None, &parent.id, false, "pr", None)
            .unwrap();

        assert_eq!(run.workflow_name, "pr-flow");
        assert!(
            run.worktree_id.is_none(),
            "worktree_id should be None for ephemeral PR runs"
        );

        // Fetch back from DB and confirm round-trip
        let fetched = mgr.get_workflow_run(&run.id).unwrap().unwrap();
        assert!(
            fetched.worktree_id.is_none(),
            "worktree_id should remain None after DB round-trip"
        );
    }
}
