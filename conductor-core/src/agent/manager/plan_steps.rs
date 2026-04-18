use std::collections::HashMap;

use chrono::Utc;
use rusqlite::named_params;

use crate::db::query_collect;
use crate::error::Result;

use super::super::db::{row_to_plan_step, AGENT_RUN_STEPS_SELECT};
use super::super::status::StepStatus;
use super::super::types::{AgentRun, PlanStep};
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Store the two-phase plan for a run. Replaces any existing plan steps.
    /// Inserts individual records into `agent_run_steps`.
    pub fn update_run_plan(&self, run_id: &str, steps: &[PlanStep]) -> Result<()> {
        // Delete any existing steps for this run.
        self.conn.execute(
            "DELETE FROM agent_run_steps WHERE run_id = :run_id",
            named_params! { ":run_id": run_id },
        )?;

        for (i, step) in steps.iter().enumerate() {
            let step_id = crate::new_id();
            let status = if step.done {
                StepStatus::Completed
            } else {
                StepStatus::Pending
            };
            self.conn.execute(
                "INSERT INTO agent_run_steps (id, run_id, position, description, status) \
                 VALUES (:id, :run_id, :position, :description, :status)",
                named_params! {
                    ":id": step_id,
                    ":run_id": run_id,
                    ":position": i as i64,
                    ":description": step.description,
                    ":status": status,
                },
            )?;
        }

        Ok(())
    }

    /// Mark all steps in the plan as completed (called on successful run completion).
    pub fn mark_plan_done(&self, run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE agent_run_steps SET status = 'completed', completed_at = :completed_at \
             WHERE run_id = :run_id AND status != 'completed'",
            named_params! { ":completed_at": now, ":run_id": run_id },
        )?;
        Ok(())
    }

    /// Update the status of a single plan step.
    pub fn update_step_status(&self, step_id: &str, status: StepStatus) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        match status {
            StepStatus::InProgress => {
                self.conn.execute(
                    "UPDATE agent_run_steps SET status = :status, started_at = :started_at WHERE id = :id",
                    named_params! { ":status": status, ":started_at": now, ":id": step_id },
                )?;
            }
            StepStatus::Completed | StepStatus::Failed => {
                self.conn.execute(
                    "UPDATE agent_run_steps SET status = :status, completed_at = :completed_at WHERE id = :id",
                    named_params! { ":status": status, ":completed_at": now, ":id": step_id },
                )?;
            }
            _ => {
                self.conn.execute(
                    "UPDATE agent_run_steps SET status = :status WHERE id = :id",
                    named_params! { ":status": status, ":id": step_id },
                )?;
            }
        }
        Ok(())
    }

    /// Get all plan steps for a run, ordered by position.
    pub fn get_run_steps(&self, run_id: &str) -> Result<Vec<PlanStep>> {
        query_collect(
            self.conn,
            &format!("{AGENT_RUN_STEPS_SELECT} WHERE run_id = :run_id ORDER BY position ASC"),
            named_params! { ":run_id": run_id },
            row_to_plan_step,
        )
    }

    /// Populate the `plan` field on a slice of runs from the steps table.
    pub(super) fn populate_plans(&self, runs: &mut [AgentRun]) -> Result<()> {
        if runs.is_empty() {
            return Ok(());
        }

        // Build a set of run IDs and fetch all steps at once.
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        let placeholders = crate::db::sql_placeholders(ids.len());
        let sql = format!(
            "{AGENT_RUN_STEPS_SELECT} WHERE run_id IN ({placeholders}) ORDER BY position ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&ids), |row| {
            let run_id: String = row.get("run_id")?;
            let step = row_to_plan_step(row)?;
            Ok((run_id, step))
        })?;

        let mut steps_map: HashMap<String, Vec<PlanStep>> = HashMap::new();
        for row in rows {
            let (run_id, step) = row?;
            steps_map.entry(run_id).or_default().push(step);
        }

        for run in runs.iter_mut() {
            if let Some(steps) = steps_map.remove(&run.id) {
                run.plan = if steps.is_empty() { None } else { Some(steps) };
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::StepStatus;
    use crate::agent::types::PlanStep;

    #[test]
    fn test_update_run_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        assert!(run.plan.is_none());

        let steps = vec![
            PlanStep {
                description: "Investigate the issue".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Write a fix".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        let plan = fetched.plan.unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].description, "Investigate the issue");
        assert!(!plan[0].done);
        assert_eq!(plan[0].status, StepStatus::Pending);
        assert!(plan[0].id.is_some());
        assert_eq!(plan[0].position, Some(0));
        assert_eq!(plan[1].description, "Write a fix");
        assert!(!plan[1].done);
        assert_eq!(plan[1].position, Some(1));
    }

    #[test]
    fn test_mark_plan_done() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![
            PlanStep {
                description: "Step one".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Step two".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.mark_plan_done(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        let plan = fetched.plan.unwrap();
        assert!(plan[0].done);
        assert_eq!(plan[0].status, StepStatus::Completed);
        assert!(plan[0].completed_at.is_some());
        assert!(plan[1].done);
        assert_eq!(plan[1].status, StepStatus::Completed);
        assert!(plan[1].completed_at.is_some());
    }

    #[test]
    fn test_mark_plan_done_no_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        // Should not error when no plan exists
        mgr.mark_plan_done(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.plan.is_none());
    }

    #[test]
    fn test_plan_roundtrip_in_latest_runs() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![PlanStep {
            description: "Do the thing".to_string(),
            done: true,
            status: StepStatus::Completed,
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let map = mgr.latest_runs_by_worktree().unwrap();
        let latest = map.get("w1").unwrap();
        let plan = latest.plan.as_ref().unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].description, "Do the thing");
        assert!(plan[0].done);
    }

    #[test]
    fn test_update_step_status() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![
            PlanStep {
                description: "Step one".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Step two".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        // Get the step IDs
        let stored = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(stored.len(), 2);

        // Mark first step in_progress
        let step_id = stored[0].id.as_ref().unwrap();
        mgr.update_step_status(step_id, StepStatus::InProgress)
            .unwrap();
        let updated = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(updated[0].status, StepStatus::InProgress);
        assert!(updated[0].started_at.is_some());
        assert!(!updated[0].done);

        // Mark first step completed
        mgr.update_step_status(step_id, StepStatus::Completed)
            .unwrap();
        let updated = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(updated[0].status, StepStatus::Completed);
        assert!(updated[0].completed_at.is_some());
        assert!(updated[0].done);
        // Second step still pending
        assert_eq!(updated[1].status, StepStatus::Pending);
        assert!(!updated[1].done);
    }

    #[test]
    fn test_update_step_status_failed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![PlanStep {
            description: "Step one".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let stored = mgr.get_run_steps(&run.id).unwrap();
        let step_id = stored[0].id.as_ref().unwrap();
        mgr.update_step_status(step_id, StepStatus::Failed).unwrap();

        let updated = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(updated[0].status, StepStatus::Failed);
        assert!(updated[0].completed_at.is_some());
        assert!(!updated[0].done);
    }

    #[test]
    fn test_get_run_steps_ordering() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![
            PlanStep {
                description: "First".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Second".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Third".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();

        let stored = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].description, "First");
        assert_eq!(stored[0].position, Some(0));
        assert_eq!(stored[1].description, "Second");
        assert_eq!(stored[1].position, Some(1));
        assert_eq!(stored[2].description, "Third");
        assert_eq!(stored[2].position, Some(2));
    }

    #[test]
    fn test_update_run_plan_replaces_existing() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps1 = vec![PlanStep {
            description: "Old step".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps1).unwrap();

        let steps2 = vec![
            PlanStep {
                description: "New step one".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "New step two".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps2).unwrap();

        let stored = mgr.get_run_steps(&run.id).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].description, "New step one");
        assert_eq!(stored[1].description, "New step two");
    }

    #[test]
    fn test_needs_resume_failed_with_incomplete_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![
            PlanStep {
                description: "Investigate".to_string(),
                done: true,
                ..Default::default()
            },
            PlanStep {
                description: "Write fix".to_string(),
                ..Default::default()
            },
            PlanStep {
                description: "Write tests".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_session_id(&run.id, "sess-abc").unwrap();
        mgr.update_run_failed(&run.id, "Context exhausted").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.needs_resume());
        assert!(fetched.has_incomplete_plan_steps());
        assert_eq!(fetched.incomplete_plan_steps().len(), 2);
        assert_eq!(fetched.claude_session_id.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn test_needs_resume_cancelled_with_incomplete_plan() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![
            PlanStep {
                description: "Step 1".to_string(),
                done: true,
                ..Default::default()
            },
            PlanStep {
                description: "Step 2".to_string(),
                ..Default::default()
            },
        ];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_session_id(&run.id, "sess-xyz").unwrap();
        mgr.update_run_cancelled(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(fetched.needs_resume());
        assert_eq!(fetched.incomplete_plan_steps().len(), 1);
    }

    #[test]
    fn test_no_needs_resume_completed_run() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![PlanStep {
            description: "Step 1".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_completed(
            &run.id,
            Some("sess-123"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        mgr.mark_plan_done(&run.id).unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(!fetched.needs_resume());
    }

    #[test]
    fn test_no_needs_resume_no_session_id() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![PlanStep {
            description: "Step 1".to_string(),
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        // Fail without ever getting a session_id (e.g. spawn failure)
        mgr.update_run_failed(&run.id, "Failed to spawn").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(!fetched.needs_resume()); // No session_id means can't resume
    }

    #[test]
    fn test_no_needs_resume_all_steps_done() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "Fix the bug", None, None)
            .unwrap();
        let steps = vec![PlanStep {
            description: "Step 1".to_string(),
            done: true,
            ..Default::default()
        }];
        mgr.update_run_plan(&run.id, &steps).unwrap();
        mgr.update_run_session_id(&run.id, "sess-123").unwrap();
        mgr.update_run_failed(&run.id, "Some error").unwrap();

        let fetched = mgr.get_run(&run.id).unwrap().unwrap();
        assert!(!fetched.needs_resume()); // All steps done, nothing to resume
    }

    #[test]
    fn test_build_resume_prompt() {
        use crate::agent::status::AgentRunStatus;
        use crate::agent::types::AgentRun;

        let run = AgentRun {
            id: "test".to_string(),
            worktree_id: Some("w1".to_string()),
            repo_id: None,
            claude_session_id: Some("sess-abc".to_string()),
            prompt: "Fix the bug".to_string(),
            status: AgentRunStatus::Failed,
            result_text: None,
            cost_usd: None,
            num_turns: None,
            duration_ms: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: None,
            tmux_window: None,
            log_file: None,
            model: None,
            plan: Some(vec![
                PlanStep {
                    description: "Investigate".to_string(),
                    done: true,
                    ..Default::default()
                },
                PlanStep {
                    description: "Write fix".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    description: "Write tests".to_string(),
                    ..Default::default()
                },
            ]),
            parent_run_id: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            bot_name: None,
            conversation_id: None,
            subprocess_pid: None,
        };

        let prompt = run.build_resume_prompt();
        assert!(prompt.contains("Continue where you left off"));
        assert!(prompt.contains("1. Write fix"));
        assert!(prompt.contains("2. Write tests"));
        assert!(!prompt.contains("Investigate")); // Done step should not appear
    }
}
