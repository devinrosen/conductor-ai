//! Database-driven detection of feedback requests and waiting gates.
//!
//! Feedback requests and gate steps are written to SQLite by other processes (headless agents,
//! MCP tools, the workflow engine, the session hook) and never reach the in-process
//! [`EventBus`](crate::events::EventBus). The web server polls the database and emits the
//! matching bus events, which drive Web Push and SSE.
//!
//! Dedup is two-layered: an in-memory set avoids a DB write per pending item per tick, and
//! `notification_log` (under `web_event.*` keys, distinct from the TUI hook keys) makes it
//! survive restarts.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use conductor_core::agent::AgentManager;
use conductor_core::error::Result;
use conductor_core::notify::{is_recent, try_claim_notification, NOTIFY_MAX_AGE_SECS};
use rusqlite::Connection;

use crate::events::ConductorEvent;

pub const WEB_FEEDBACK_EVENT_TYPE: &str = "web_event.feedback_requested";
pub const WEB_GATE_EVENT_TYPE: &str = "web_event.gate_waiting";

#[derive(Default)]
pub struct DbEventDetector {
    seen_feedback: HashSet<String>,
    seen_gates: HashSet<String>,
}

impl DbEventDetector {
    /// Run one poll tick and return the events the caller should emit on the bus.
    pub fn poll(&mut self, conn: &Connection, now: DateTime<Utc>) -> Result<Vec<ConductorEvent>> {
        let max_age = Duration::seconds(NOTIFY_MAX_AGE_SECS);
        let mut events = Vec::new();

        let mgr = AgentManager::new(conn);
        let pending = mgr.list_all_pending_feedback_requests()?;
        for req in &pending {
            if self.seen_feedback.contains(&req.id) {
                continue;
            }
            match try_claim_notification(conn, &req.id, WEB_FEEDBACK_EVENT_TYPE) {
                Ok(claimed) => {
                    self.seen_feedback.insert(req.id.clone());
                    if claimed && is_recent(&req.created_at, now, max_age) {
                        let worktree_id = mgr
                            .get_run(&req.run_id)
                            .ok()
                            .flatten()
                            .and_then(|r| r.worktree_id)
                            .unwrap_or_default();
                        events.push(ConductorEvent::FeedbackRequested {
                            run_id: req.run_id.clone(),
                            worktree_id,
                            feedback_id: req.id.clone(),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(feedback_id = %req.id, "feedback claim failed: {e}");
                }
            }
        }
        let pending_ids: HashSet<&str> = pending.iter().map(|r| r.id.as_str()).collect();
        self.seen_feedback
            .retain(|id| pending_ids.contains(id.as_str()));

        let gates = conductor_core::workflow::list_all_waiting_gate_steps(conn)?;
        for (step, _, _) in &gates {
            if self.seen_gates.contains(&step.id) {
                continue;
            }
            match try_claim_notification(conn, &step.id, WEB_GATE_EVENT_TYPE) {
                Ok(claimed) => {
                    self.seen_gates.insert(step.id.clone());
                    // Fail open when the step has no start timestamp.
                    let recent = step
                        .started_at
                        .as_deref()
                        .is_none_or(|t| is_recent(t, now, max_age));
                    if claimed && recent {
                        events.push(ConductorEvent::WorkflowGateWaiting {
                            run_id: step.workflow_run_id.clone(),
                            step_id: step.id.clone(),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(step_id = %step.id, "gate claim failed: {e}");
                }
            }
        }
        let gate_ids: HashSet<&str> = gates.iter().map(|(s, _, _)| s.id.as_str()).collect();
        self.seen_gates.retain(|id| gate_ids.contains(id.as_str()));

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::seeded_state_with_agent_run;
    use conductor_core::workflow::{GateType, WorkflowStepStatus};

    fn open() -> (tempfile::NamedTempFile, Connection) {
        let (state, tmp) = seeded_state_with_agent_run();
        drop(state);
        let conn = conductor_core::db::open_database(tmp.path()).unwrap();
        (tmp, conn)
    }

    fn add_feedback(conn: &Connection) -> String {
        AgentManager::new(conn)
            .request_feedback("ar1", "need input", None)
            .unwrap()
            .id
    }

    fn add_gate(conn: &Connection) -> (String, String) {
        let run = conductor_core::workflow::create_workflow_run(
            conn,
            "wf",
            Some("w1"),
            "ar1",
            false,
            "manual",
            None,
        )
        .unwrap();
        let step_id =
            conductor_core::workflow::insert_step(conn, &run.id, "gate", "gate", false, 0, 0)
                .unwrap();
        conductor_core::workflow::set_step_gate_info(
            conn,
            &step_id,
            &GateType::HumanApproval,
            Some("approve?"),
            "1h",
        )
        .unwrap();
        conductor_core::workflow::update_step_status(
            conn,
            &step_id,
            WorkflowStepStatus::Waiting,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        (run.id, step_id)
    }

    #[test]
    fn feedback_emits_once() {
        let (_tmp, conn) = open();
        let id = add_feedback(&conn);
        let mut d = DbEventDetector::default();
        let evs = d.poll(&conn, Utc::now()).unwrap();
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            ConductorEvent::FeedbackRequested {
                run_id,
                worktree_id,
                feedback_id,
            } => {
                assert_eq!(run_id, "ar1");
                assert_eq!(worktree_id, "w1");
                assert_eq!(feedback_id, &id);
            }
            other => panic!("unexpected event {other:?}"),
        }
        assert!(d.poll(&conn, Utc::now()).unwrap().is_empty());
    }

    #[test]
    fn gate_emits_once() {
        let (_tmp, conn) = open();
        let (run_id, step_id) = add_gate(&conn);
        let mut d = DbEventDetector::default();
        let evs = d.poll(&conn, Utc::now()).unwrap();
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            ConductorEvent::WorkflowGateWaiting {
                run_id: r,
                step_id: s,
            } => {
                assert_eq!(r, &run_id);
                assert_eq!(s, &step_id);
            }
            other => panic!("unexpected event {other:?}"),
        }
        assert!(d.poll(&conn, Utc::now()).unwrap().is_empty());
    }

    #[test]
    fn restart_does_not_reemit() {
        let (_tmp, conn) = open();
        add_feedback(&conn);
        add_gate(&conn);
        let mut first = DbEventDetector::default();
        assert_eq!(first.poll(&conn, Utc::now()).unwrap().len(), 2);
        let mut fresh = DbEventDetector::default();
        assert!(fresh.poll(&conn, Utc::now()).unwrap().is_empty());
    }

    #[test]
    fn stale_item_is_claimed_but_not_emitted() {
        let (_tmp, conn) = open();
        let id = add_feedback(&conn);
        let mut d = DbEventDetector::default();
        let later = Utc::now() + Duration::days(2);
        assert!(d.poll(&conn, later).unwrap().is_empty());
        assert!(!try_claim_notification(&conn, &id, WEB_FEEDBACK_EVENT_TYPE).unwrap());
    }

    #[test]
    fn route_claimed_request_is_not_reemitted() {
        let (_tmp, conn) = open();
        let id = add_feedback(&conn);
        assert!(try_claim_notification(&conn, &id, WEB_FEEDBACK_EVENT_TYPE).unwrap());
        let mut d = DbEventDetector::default();
        assert!(d.poll(&conn, Utc::now()).unwrap().is_empty());
    }

    #[test]
    fn tui_hook_key_does_not_suppress_web_event() {
        let (_tmp, conn) = open();
        let id = add_feedback(&conn);
        assert!(try_claim_notification(&conn, &id, "feedback_requested").unwrap());
        let mut d = DbEventDetector::default();
        assert_eq!(d.poll(&conn, Utc::now()).unwrap().len(), 1);
    }
}
