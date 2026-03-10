//! Orchestrator: automatically spawn and manage child agent runs from a parent run's plan steps.
//!
//! The orchestrator takes a parent run with plan steps and, for each step, spawns
//! a child agent run in a tmux window. It polls the database for child completion,
//! updates plan step status, and aggregates results back to the parent.

use std::process::Command;
use std::time::Duration;

use rusqlite::Connection;

use crate::agent::{AgentManager, AgentRun, AgentRunStatus, StepStatus};
use crate::agent_runtime;
use crate::config::Config;
use crate::error::Result;
use crate::worktree::WorktreeManager;

/// Configuration for the orchestrator.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// How often to poll the DB for child run completion (default: 5s).
    pub poll_interval: Duration,
    /// Maximum time to wait for a single child run before marking it as timed out (default: 30min).
    pub child_timeout: Duration,
    /// Whether to stop orchestration on the first child failure (default: false).
    pub fail_fast: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            child_timeout: Duration::from_secs(30 * 60),
            fail_fast: false,
        }
    }
}

/// Result of a single child run within the orchestration.
#[derive(Debug, Clone)]
pub struct ChildRunResult {
    pub step_index: usize,
    pub step_description: String,
    pub run_id: String,
    pub status: AgentRunStatus,
    pub result_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// Summary of the full orchestration.
#[derive(Debug, Clone)]
pub struct OrchestrationResult {
    pub parent_run_id: String,
    pub child_results: Vec<ChildRunResult>,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub all_succeeded: bool,
}

/// Run the orchestration loop for a parent agent run.
///
/// This function:
/// 1. Reads the parent run's plan steps from the database
/// 2. For each pending step, spawns a child agent run in a tmux window
/// 3. Polls the DB for child completion
/// 4. Updates the plan step status based on the child's result
/// 5. Aggregates all results and updates the parent run
///
pub fn orchestrate_run(
    conn: &Connection,
    config: &Config,
    parent_run_id: &str,
    worktree_path: &str,
    model: Option<&str>,
    orch_config: &OrchestratorConfig,
) -> Result<OrchestrationResult> {
    let mgr = AgentManager::new(conn);

    // Fetch the parent run and verify it exists
    let parent_run = mgr.get_run(parent_run_id)?.ok_or_else(|| {
        crate::error::ConductorError::Agent(format!("Parent run not found: {parent_run_id}"))
    })?;

    // Orchestration requires a registered worktree for tmux window naming.
    // Ephemeral PR runs (worktree_id == None) are not supported by the orchestrator.
    let worktree_id = parent_run.worktree_id.as_deref().ok_or_else(|| {
        crate::error::ConductorError::Agent(
            "Orchestration is not supported for ephemeral PR runs (no registered worktree)."
                .to_string(),
        )
    })?;
    let wt_mgr = WorktreeManager::new(conn, config);
    let worktree = wt_mgr.get_by_id(worktree_id)?;

    let steps = mgr.get_run_steps(parent_run_id)?;
    if steps.is_empty() {
        return Err(crate::error::ConductorError::Agent(
            "Parent run has no plan steps to orchestrate".to_string(),
        ));
    }

    let mut child_results = Vec::new();
    let mut all_succeeded = true;

    for (i, step) in steps.iter().enumerate() {
        // Skip already-completed steps (e.g. from a resumed orchestration)
        if step.status == StepStatus::Completed {
            eprintln!(
                "[orchestrator] Step {}/{}: already completed — {}",
                i + 1,
                steps.len(),
                step.description
            );
            continue;
        }

        eprintln!(
            "[orchestrator] Step {}/{}: {}",
            i + 1,
            steps.len(),
            step.description
        );

        // Mark step as in_progress
        if let Some(ref step_id) = step.id {
            let _ = mgr.update_step_status(step_id, StepStatus::InProgress);
        }

        // Build prompt for the child agent from the step description and parent context
        let child_prompt = build_child_prompt(&parent_run, &step.description, i, steps.len());

        // Create child run record
        let child_window = format!("{}-step-{}", worktree.slug, i + 1);
        let child_run = mgr.create_child_run(
            Some(worktree_id),
            &child_prompt,
            Some(&child_window),
            model,
            parent_run_id,
        )?;

        eprintln!(
            "[orchestrator] Spawning child run {} in tmux window '{}'",
            child_run.id, child_window
        );

        // Spawn the child agent in a tmux window
        let spawn_result = agent_runtime::spawn_child_tmux(
            &child_run.id,
            worktree_path,
            &child_prompt,
            model,
            &child_window,
        );

        if let Err(e) = spawn_result {
            eprintln!("[orchestrator] Failed to spawn child: {e}");
            let _ = mgr.update_run_failed(&child_run.id, &format!("spawn failed: {e}"));
            if let Some(ref step_id) = step.id {
                let _ = mgr.update_step_status(step_id, StepStatus::Failed);
            }
            all_succeeded = false;

            child_results.push(ChildRunResult {
                step_index: i,
                step_description: step.description.clone(),
                run_id: child_run.id,
                status: AgentRunStatus::Failed,
                result_text: Some(format!("spawn failed: {e}")),
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
            });

            if orch_config.fail_fast {
                eprintln!("[orchestrator] fail_fast enabled — stopping orchestration");
                break;
            }
            continue;
        }

        // Poll for child completion
        let result = agent_runtime::poll_child_completion(
            conn,
            &child_run.id,
            orch_config.poll_interval,
            orch_config.child_timeout,
            None,
        );

        match result {
            Ok(completed_run) => {
                let succeeded = completed_run.status == AgentRunStatus::Completed;
                if succeeded {
                    eprintln!(
                        "[orchestrator] Step {}/{} completed: cost=${:.4}, {} turns",
                        i + 1,
                        steps.len(),
                        completed_run.cost_usd.unwrap_or(0.0),
                        completed_run.num_turns.unwrap_or(0)
                    );
                    if let Some(ref step_id) = step.id {
                        let _ = mgr.update_step_status(step_id, StepStatus::Completed);
                    }
                } else {
                    eprintln!(
                        "[orchestrator] Step {}/{} failed: {}",
                        i + 1,
                        steps.len(),
                        completed_run
                            .result_text
                            .as_deref()
                            .unwrap_or("unknown error")
                    );
                    if let Some(ref step_id) = step.id {
                        let _ = mgr.update_step_status(step_id, StepStatus::Failed);
                    }
                    all_succeeded = false;
                }

                child_results.push(ChildRunResult {
                    step_index: i,
                    step_description: step.description.clone(),
                    run_id: completed_run.id.clone(),
                    status: completed_run.status.clone(),
                    result_text: completed_run.result_text.clone(),
                    cost_usd: completed_run.cost_usd,
                    num_turns: completed_run.num_turns,
                    duration_ms: completed_run.duration_ms,
                });

                if !succeeded && orch_config.fail_fast {
                    eprintln!("[orchestrator] fail_fast enabled — stopping orchestration");
                    break;
                }
            }
            Err(e) => {
                eprintln!("[orchestrator] Step {}/{} error: {e}", i + 1, steps.len());
                if let Some(ref step_id) = step.id {
                    let _ = mgr.update_step_status(step_id, StepStatus::Failed);
                }
                // Cancel the child run if it timed out
                let _ = mgr.update_run_cancelled(&child_run.id);
                // Try to kill the tmux window
                let _ = Command::new("tmux")
                    .args(["kill-window", "-t", &format!(":{child_window}")])
                    .output();

                all_succeeded = false;

                child_results.push(ChildRunResult {
                    step_index: i,
                    step_description: step.description.clone(),
                    run_id: child_run.id,
                    status: AgentRunStatus::Failed,
                    result_text: Some(e.to_string()),
                    cost_usd: None,
                    num_turns: None,
                    duration_ms: None,
                });

                if orch_config.fail_fast {
                    eprintln!("[orchestrator] fail_fast enabled — stopping orchestration");
                    break;
                }
            }
        }
    }

    // Aggregate totals
    let total_cost: f64 = child_results.iter().filter_map(|r| r.cost_usd).sum();
    let total_turns: i64 = child_results.iter().filter_map(|r| r.num_turns).sum();
    let total_duration_ms: i64 = child_results.iter().filter_map(|r| r.duration_ms).sum();

    let result = OrchestrationResult {
        parent_run_id: parent_run_id.to_string(),
        child_results,
        total_cost,
        total_turns,
        total_duration_ms,
        all_succeeded,
    };

    // Build a summary of the orchestration as the parent's result text
    let summary = build_orchestration_summary(&result);

    // Update the parent run status
    if all_succeeded {
        mgr.update_run_completed(
            parent_run_id,
            None, // no session_id for orchestrator
            Some(&summary),
            Some(total_cost),
            Some(total_turns),
            Some(total_duration_ms),
        )?;
        mgr.mark_plan_done(parent_run_id)?;
        eprintln!("[orchestrator] All steps completed successfully");
    } else {
        mgr.update_run_failed(parent_run_id, &summary)?;
        eprintln!("[orchestrator] Orchestration finished with failures");
    }

    eprintln!(
        "[orchestrator] Total: ${:.4}, {} turns, {:.1}s",
        total_cost,
        total_turns,
        total_duration_ms as f64 / 1000.0
    );

    Ok(result)
}

/// Build a prompt for a child agent based on the parent's context and step description.
fn build_child_prompt(
    parent_run: &AgentRun,
    step_description: &str,
    step_index: usize,
    total_steps: usize,
) -> String {
    format!(
        "You are executing step {step_num} of {total_steps} in a multi-step plan.\n\n\
         ## Original Task\n\
         {parent_prompt}\n\n\
         ## Your Assignment (Step {step_num}/{total_steps})\n\
         {step_description}\n\n\
         Focus only on this step. Do not attempt to complete other steps.",
        step_num = step_index + 1,
        parent_prompt = parent_run.prompt,
    )
}

/// Build a human-readable summary of the orchestration result.
fn build_orchestration_summary(result: &OrchestrationResult) -> String {
    let mut lines = Vec::new();

    let total = result.child_results.len();
    let succeeded = result
        .child_results
        .iter()
        .filter(|r| r.status == AgentRunStatus::Completed)
        .count();
    let failed = total - succeeded;

    lines.push(format!(
        "Orchestration: {succeeded}/{total} steps completed{}",
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        }
    ));

    for r in &result.child_results {
        let status_marker = if r.status == AgentRunStatus::Completed {
            "ok"
        } else {
            "FAIL"
        };
        lines.push(format!(
            "  [{status_marker}] Step {}: {}",
            r.step_index + 1,
            r.step_description
        ));
    }

    lines.push(format!(
        "Total: ${:.4}, {} turns, {:.1}s",
        result.total_cost,
        result.total_turns,
        result.total_duration_ms as f64 / 1000.0
    ));

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    fn make_parent_run(prompt: &str) -> AgentRun {
        AgentRun {
            id: "parent-1".to_string(),
            worktree_id: Some("w1".to_string()),
            claude_session_id: None,
            prompt: prompt.to_string(),
            status: AgentRunStatus::Running,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: None,
            parent_run_id: None,
        }
    }

    #[test]
    fn test_build_child_prompt() {
        let parent = make_parent_run("Implement user authentication");

        let prompt = build_child_prompt(&parent, "Add login form UI", 0, 3);
        assert!(prompt.contains("step 1 of 3"));
        assert!(prompt.contains("Implement user authentication"));
        assert!(prompt.contains("Add login form UI"));
        assert!(prompt.contains("Focus only on this step"));
    }

    #[test]
    fn test_build_child_prompt_last_step() {
        let parent = make_parent_run("Build API");

        let prompt = build_child_prompt(&parent, "Deploy to staging", 4, 5);
        assert!(prompt.contains("step 5 of 5"));
        assert!(prompt.contains("Build API"));
        assert!(prompt.contains("Deploy to staging"));
    }

    #[test]
    fn test_build_child_prompt_single_step() {
        let parent = make_parent_run("Fix the typo");

        let prompt = build_child_prompt(&parent, "Fix the typo in README", 0, 1);
        assert!(prompt.contains("step 1 of 1"));
    }

    #[test]
    fn test_orchestration_summary() {
        let result = OrchestrationResult {
            parent_run_id: "p1".to_string(),
            child_results: vec![
                ChildRunResult {
                    step_index: 0,
                    step_description: "Read the code".to_string(),
                    run_id: "c1".to_string(),
                    status: AgentRunStatus::Completed,
                    result_text: None,
                    cost_usd: Some(0.05),
                    num_turns: Some(3),
                    duration_ms: Some(5000),
                },
                ChildRunResult {
                    step_index: 1,
                    step_description: "Write tests".to_string(),
                    run_id: "c2".to_string(),
                    status: AgentRunStatus::Failed,
                    result_text: Some("timeout".to_string()),
                    cost_usd: Some(0.10),
                    num_turns: Some(5),
                    duration_ms: Some(10000),
                },
            ],
            total_cost: 0.15,
            total_turns: 8,
            total_duration_ms: 15000,
            all_succeeded: false,
        };

        let summary = build_orchestration_summary(&result);
        assert!(summary.contains("1/2 steps completed"));
        assert!(summary.contains("1 failed"));
        assert!(summary.contains("[ok] Step 1"));
        assert!(summary.contains("[FAIL] Step 2"));
        assert!(summary.contains("$0.1500"));
    }

    #[test]
    fn test_orchestration_summary_all_succeeded() {
        let result = OrchestrationResult {
            parent_run_id: "p1".to_string(),
            child_results: vec![
                ChildRunResult {
                    step_index: 0,
                    step_description: "Step A".to_string(),
                    run_id: "c1".to_string(),
                    status: AgentRunStatus::Completed,
                    result_text: None,
                    cost_usd: Some(0.01),
                    num_turns: Some(2),
                    duration_ms: Some(1000),
                },
                ChildRunResult {
                    step_index: 1,
                    step_description: "Step B".to_string(),
                    run_id: "c2".to_string(),
                    status: AgentRunStatus::Completed,
                    result_text: None,
                    cost_usd: Some(0.02),
                    num_turns: Some(3),
                    duration_ms: Some(2000),
                },
            ],
            total_cost: 0.03,
            total_turns: 5,
            total_duration_ms: 3000,
            all_succeeded: true,
        };

        let summary = build_orchestration_summary(&result);
        assert!(summary.contains("2/2 steps completed"));
        // Should NOT contain failure count
        assert!(!summary.contains("failed"));
        assert!(summary.contains("[ok] Step 1"));
        assert!(summary.contains("[ok] Step 2"));
    }

    #[test]
    fn test_orchestration_summary_empty_results() {
        let result = OrchestrationResult {
            parent_run_id: "p1".to_string(),
            child_results: vec![],
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            all_succeeded: true,
        };

        let summary = build_orchestration_summary(&result);
        assert!(summary.contains("0/0 steps completed"));
        assert!(summary.contains("$0.0000"));
    }

    #[test]
    fn test_orchestrator_config_defaults() {
        let config = OrchestratorConfig::default();
        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert_eq!(config.child_timeout, Duration::from_secs(30 * 60));
        assert!(!config.fail_fast);
    }

    #[test]
    fn test_orchestrator_config_custom() {
        let config = OrchestratorConfig {
            poll_interval: Duration::from_secs(1),
            child_timeout: Duration::from_secs(60),
            fail_fast: true,
        };
        assert_eq!(config.poll_interval, Duration::from_secs(1));
        assert_eq!(config.child_timeout, Duration::from_secs(60));
        assert!(config.fail_fast);
    }

    #[test]
    fn test_poll_child_completion_already_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        mgr.update_run_completed(&run.id, None, Some("done"), Some(0.05), Some(3), Some(5000))
            .unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
        );
        assert!(result.is_ok());
        let completed = result.unwrap();
        assert_eq!(completed.status, AgentRunStatus::Completed);
        assert_eq!(completed.result_text.as_deref(), Some("done"));
    }

    #[test]
    fn test_poll_child_completion_already_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        mgr.update_run_failed(&run.id, "something broke").unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
        );
        assert!(result.is_ok());
        let failed = result.unwrap();
        assert_eq!(failed.status, AgentRunStatus::Failed);
    }

    #[test]
    fn test_poll_child_completion_already_cancelled() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
        );
        assert!(result.is_ok());
        let cancelled = result.unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
    }

    #[test]
    fn test_poll_child_completion_not_found() {
        let conn = setup_db();

        let result = agent_runtime::poll_child_completion(
            &conn,
            "nonexistent-id",
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_runtime::PollError::Other(_)
        ));
    }

    #[test]
    fn test_poll_child_completion_timeout() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        // Create a run that stays in "running" status
        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_millis(50), // very short timeout
            None,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            agent_runtime::PollError::Timeout(_)
        ));
    }

    #[test]
    fn test_orchestrate_run_no_steps() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let result = orchestrate_run(
            &conn,
            &config,
            &run.id,
            "/tmp/ws/feat-test",
            None,
            &OrchestratorConfig::default(),
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("no plan steps"));
    }

    #[test]
    fn test_orchestrate_run_ephemeral_rejected() {
        let conn = setup_db();
        let config = Config::default();
        let mgr = AgentManager::new(&conn);

        // Create a run with no worktree_id (ephemeral PR run)
        let run = mgr.create_run(None, "test ephemeral", None, None).unwrap();

        let result = orchestrate_run(
            &conn,
            &config,
            &run.id,
            "/tmp/ephemeral-clone",
            None,
            &OrchestratorConfig::default(),
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("ephemeral") || err_msg.contains("no registered worktree"),
            "unexpected error message: {err_msg}"
        );
    }

    #[test]
    fn test_orchestrate_run_not_found() {
        let conn = setup_db();
        let config = Config::default();

        let result = orchestrate_run(
            &conn,
            &config,
            "nonexistent-run",
            "/tmp/ws/feat-test",
            None,
            &OrchestratorConfig::default(),
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("not found"));
    }

    #[test]
    fn test_child_run_result_fields() {
        let r = ChildRunResult {
            step_index: 2,
            step_description: "Implement feature".to_string(),
            run_id: "run-123".to_string(),
            status: AgentRunStatus::Completed,
            result_text: Some("All good".to_string()),
            cost_usd: Some(0.1234),
            num_turns: Some(10),
            duration_ms: Some(30000),
        };
        assert_eq!(r.step_index, 2);
        assert_eq!(r.status, AgentRunStatus::Completed);
        assert_eq!(r.cost_usd, Some(0.1234));
        assert_eq!(r.duration_ms, Some(30000));
    }

    #[test]
    fn test_orchestration_result_aggregation() {
        let result = OrchestrationResult {
            parent_run_id: "p1".to_string(),
            child_results: vec![
                ChildRunResult {
                    step_index: 0,
                    step_description: "A".to_string(),
                    run_id: "c1".to_string(),
                    status: AgentRunStatus::Completed,
                    result_text: None,
                    cost_usd: Some(0.10),
                    num_turns: Some(5),
                    duration_ms: Some(10000),
                },
                ChildRunResult {
                    step_index: 1,
                    step_description: "B".to_string(),
                    run_id: "c2".to_string(),
                    status: AgentRunStatus::Completed,
                    result_text: None,
                    cost_usd: Some(0.20),
                    num_turns: Some(10),
                    duration_ms: Some(20000),
                },
                ChildRunResult {
                    step_index: 2,
                    step_description: "C".to_string(),
                    run_id: "c3".to_string(),
                    status: AgentRunStatus::Failed,
                    result_text: None,
                    cost_usd: None,
                    num_turns: None,
                    duration_ms: None,
                },
            ],
            total_cost: 0.30,
            total_turns: 15,
            total_duration_ms: 30000,
            all_succeeded: false,
        };

        assert_eq!(result.child_results.len(), 3);
        assert!(!result.all_succeeded);
        assert_eq!(result.total_cost, 0.30);
        assert_eq!(result.total_turns, 15);

        let summary = build_orchestration_summary(&result);
        assert!(summary.contains("2/3 steps completed"));
        assert!(summary.contains("1 failed"));
    }
}
