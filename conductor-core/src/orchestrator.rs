//! Orchestrator: automatically spawn and manage child agent runs from a parent run's plan steps.
//!
//! The orchestrator takes a parent run with plan steps and, for each step, spawns
//! a headless child agent subprocess. It drains the subprocess stdout stream for
//! completion, updates plan step status, and aggregates results back to the parent.

use std::time::Duration;

use rusqlite::Connection;

use crate::agent::{AgentManager, AgentRun, AgentRunStatus, StepStatus};
use crate::agent_runtime;
use crate::config::Config;
use crate::error::Result;

/// Configuration for the orchestrator.
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Maximum time to wait for a single child run before marking it as timed out (default: 30min).
    pub child_timeout: Duration,
    /// Whether to stop orchestration on the first child failure (default: false).
    pub fail_fast: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
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
/// 2. For each pending step, spawns a headless child agent subprocess
/// 3. Drains the subprocess stdout stream for completion
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

    // Orchestration requires a registered worktree.
    // Ephemeral PR runs (worktree_id == None) are not supported by the orchestrator.
    let worktree_id = parent_run.worktree_id.as_deref().ok_or_else(|| {
        crate::error::ConductorError::Agent(
            "Orchestration is not supported for ephemeral PR runs (no registered worktree)."
                .to_string(),
        )
    })?;

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

        // Create child run record (no tmux window — headless subprocess)
        let child_run = mgr.create_child_run(
            Some(worktree_id),
            &child_prompt,
            None,
            model,
            parent_run_id,
            None,
        )?;

        eprintln!(
            "[orchestrator] Spawning headless child run {}",
            child_run.id
        );

        // Spawn the child agent as a headless subprocess
        let (handle, prompt_file) = match agent_runtime::try_spawn_headless_run(
            &child_run.id,
            worktree_path,
            &child_prompt,
            None,
            model,
            None,
            Some(&config.general.agent_permission_mode),
            &[],
        ) {
            Ok(pair) => pair,
            Err(e) => {
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
        };

        let pid = handle.pid;
        if let Err(e) = mgr.update_run_subprocess_pid(&child_run.id, pid) {
            tracing::warn!("Failed to persist subprocess pid: {e}");
        }

        // Spawn drain thread — opens its own DB connection (Connection is not Send)
        let run_id_clone = child_run.id.clone();
        let log_path = crate::config::agent_log_path(&child_run.id);
        let (tx, rx) = std::sync::mpsc::channel::<agent_runtime::DrainOutcome>();
        std::thread::spawn(move || {
            let conn = match crate::db::open_database(&crate::config::db_path()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("drain thread: failed to open DB: {e}");
                    let _ = std::fs::remove_file(&prompt_file);
                    let _ = tx.send(agent_runtime::DrainOutcome::NoResult);
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);
            let outcome = agent_runtime::drain_stream_json(
                handle.stdout,
                &run_id_clone,
                &log_path,
                &mgr,
                |_| {},
            );
            let _ = std::fs::remove_file(&prompt_file);
            drop(handle.stderr);
            let mut child = handle.child;
            let _ = child.wait();
            let _ = tx.send(outcome);
        });

        // Wait for drain thread with periodic timeout checks
        let start = std::time::Instant::now();
        let drain_outcome = loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(outcome) => break outcome,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if start.elapsed() > orch_config.child_timeout {
                        eprintln!(
                            "[orchestrator] Step {}/{} timed out, cancelling",
                            i + 1,
                            steps.len()
                        );
                        // Mark cancelled BEFORE sending SIGTERM (RFC 016 Q2)
                        let _ = mgr.update_run_cancelled(&child_run.id);
                        agent_runtime::cancel_subprocess(pid);
                        let _ = rx.recv_timeout(Duration::from_secs(6));
                        break agent_runtime::DrainOutcome::NoResult;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::warn!(
                        "[orchestrator] Step {}/{}: drain thread disconnected unexpectedly",
                        i + 1,
                        steps.len()
                    );
                    break agent_runtime::DrainOutcome::NoResult;
                }
            }
        };

        match drain_outcome {
            agent_runtime::DrainOutcome::Completed => {
                // Re-read run from DB for final status and metrics
                let completed_run = match mgr.get_run(&child_run.id) {
                    Ok(Some(r)) => r,
                    Ok(None) | Err(_) => {
                        eprintln!(
                            "[orchestrator] Step {}/{} DB read error after drain",
                            i + 1,
                            steps.len()
                        );
                        if let Some(ref step_id) = step.id {
                            let _ = mgr.update_step_status(step_id, StepStatus::Failed);
                        }
                        all_succeeded = false;
                        child_results.push(ChildRunResult {
                            step_index: i,
                            step_description: step.description.clone(),
                            run_id: child_run.id,
                            status: AgentRunStatus::Failed,
                            result_text: Some("DB read error after drain".to_string()),
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
                };

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
            agent_runtime::DrainOutcome::NoResult => {
                eprintln!(
                    "[orchestrator] Step {}/{}: no result from headless process",
                    i + 1,
                    steps.len()
                );
                // Ensure the run is marked failed if not already cancelled by timeout path
                let _ = mgr
                    .update_run_failed_if_running(&child_run.id, "no result from headless process");
                if let Some(ref step_id) = step.id {
                    let _ = mgr.update_step_status(step_id, StepStatus::Failed);
                }
                all_succeeded = false;

                child_results.push(ChildRunResult {
                    step_index: i,
                    step_description: step.description.clone(),
                    run_id: child_run.id,
                    status: AgentRunStatus::Failed,
                    result_text: Some("no result from headless process".to_string()),
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
            None,
            None,
            None,
            None,
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
            repo_id: None,
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
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
            conversation_id: None,
            subprocess_pid: None,
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
        assert_eq!(config.child_timeout, Duration::from_secs(30 * 60));
        assert!(!config.fail_fast);
    }

    #[test]
    fn test_orchestrator_config_custom() {
        let config = OrchestratorConfig {
            child_timeout: Duration::from_secs(60),
            fail_fast: true,
        };
        assert_eq!(config.child_timeout, Duration::from_secs(60));
        assert!(config.fail_fast);
    }

    #[test]
    fn test_poll_child_completion_already_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();
        mgr.update_run_completed(
            &run.id,
            None,
            Some("done"),
            Some(0.05),
            Some(3),
            Some(5000),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let result = agent_runtime::poll_child_completion(
            &conn,
            &run.id,
            Duration::from_millis(10),
            Duration::from_secs(1),
            None,
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
