use crate::db::query_collect;
use crate::error::Result;

use super::super::db::{row_to_agent_run, AGENT_RUN_SELECT};
use super::super::log_parsing::try_recover_from_log;
use super::super::tmux::list_live_tmux_windows;
use super::AgentManager;

impl<'a> AgentManager<'a> {
    /// Reap orphaned agent runs whose tmux windows have disappeared.
    ///
    /// Queries all runs with an active status (`running` or `waiting_for_feedback`),
    /// checks whether their tmux window still exists, and for any orphans:
    /// 1. Attempts log-file recovery via `try_recover_from_log()` (the agent may
    ///    have completed but the handler didn't fire).
    /// 2. If no result is found in the log, marks the run as `failed`.
    ///
    /// Returns the number of orphaned runs that were reaped.
    pub fn reap_orphaned_runs(&self) -> Result<usize> {
        let active_runs = query_collect(
            self.conn,
            &format!("{AGENT_RUN_SELECT} WHERE status IN ('running', 'waiting_for_feedback')"),
            [],
            row_to_agent_run,
        )?;

        // Fetch all live tmux window names once (avoids N+1 subprocess spawns).
        let live_windows = list_live_tmux_windows();

        let mut reaped = 0;
        for run in &active_runs {
            if let Some(ref name) = run.tmux_window {
                if live_windows.contains(name.as_str()) {
                    continue;
                }
            }
            // Window is gone — try to recover result from log file
            if try_recover_from_log(self, &run.id).is_some() {
                reaped += 1;
                continue;
            }
            // No result in log — mark as failed
            self.update_run_failed(
                &run.id,
                "tmux session lost — agent may have completed but result was not captured",
            )?;
            reaped += 1;
        }
        Ok(reaped)
    }
}

#[cfg(test)]
mod tests {
    use super::super::setup_db;
    use super::super::AgentManager;
    use crate::agent::status::AgentRunStatus;

    #[test]
    fn test_reap_orphaned_runs_no_tmux_window() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "test prompt", None, None)
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
        assert!(updated
            .result_text
            .as_deref()
            .unwrap()
            .contains("tmux session lost"));
        assert!(updated.ended_at.is_some());
    }

    #[test]
    fn test_reap_orphaned_runs_nonexistent_tmux_window() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(
                Some("w1"),
                "test prompt",
                Some("nonexistent-window-xyz-999"),
                None,
            )
            .unwrap();
        assert_eq!(run.status, AgentRunStatus::Running);

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 1);

        let updated = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(updated.status, AgentRunStatus::Failed);
    }

    #[test]
    fn test_reap_orphaned_runs_skips_completed() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let run = mgr
            .create_run(Some("w1"), "test prompt", None, None)
            .unwrap();
        mgr.update_run_completed(
            &run.id,
            None,
            Some("Done"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 0);
    }

    #[test]
    fn test_reap_orphaned_runs_multiple() {
        let conn = setup_db();
        let mgr = AgentManager::new(&conn);

        let r1 = mgr.create_run(Some("w1"), "prompt 1", None, None).unwrap();
        let r2 = mgr.create_run(Some("w1"), "prompt 2", None, None).unwrap();
        let r3 = mgr.create_run(Some("w1"), "prompt 3", None, None).unwrap();
        mgr.update_run_completed(
            &r3.id,
            None,
            Some("Done"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let reaped = mgr.reap_orphaned_runs().unwrap();
        assert_eq!(reaped, 2);

        assert_eq!(
            mgr.get_run(&r1.id).unwrap().unwrap().status,
            AgentRunStatus::Failed
        );
        assert_eq!(
            mgr.get_run(&r2.id).unwrap().unwrap().status,
            AgentRunStatus::Failed
        );
        assert_eq!(
            mgr.get_run(&r3.id).unwrap().unwrap().status,
            AgentRunStatus::Completed
        );
    }
}
