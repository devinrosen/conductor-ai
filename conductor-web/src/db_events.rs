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
use conductor_core::notify::{
    is_recent, load_idle_session_or_skip, preview, try_claim_notification, IdleCandidate,
    IdleSession, IdleVisit, SeenIdlePeriods, NOTIFY_MAX_AGE_SECS,
};
use rusqlite::Connection;

use crate::events::ConductorEvent;

pub const WEB_FEEDBACK_EVENT_TYPE: &str = "web_event.feedback_requested";
pub const WEB_GATE_EVENT_TYPE: &str = "web_event.gate_waiting";
pub const WEB_SESSION_IDLE_EVENT_TYPE: &str = "web_event.session_idle";

#[derive(Default)]
pub struct DbEventDetector {
    seen_feedback: HashSet<String>,
    seen_gates: HashSet<String>,
    seen_session_idle: SeenIdlePeriods,
}

/// Claim each unseen item once and push an event for those that are newly claimed and recent.
///
/// `created_at` returns `None` when the item has no timestamp; such items fail open (treated as
/// recent). Afterwards `seen` is trimmed to the ids still present in `items`.
#[allow(clippy::too_many_arguments)]
fn collect_new<T>(
    conn: &Connection,
    seen: &mut HashSet<String>,
    items: &[T],
    event_type: &str,
    now: DateTime<Utc>,
    max_age: Duration,
    id_of: impl Fn(&T) -> &str,
    created_at: impl Fn(&T) -> Option<&str>,
    mut build: impl FnMut(&T) -> ConductorEvent,
    events: &mut Vec<ConductorEvent>,
) {
    for item in items {
        let id = id_of(item);
        if seen.contains(id) {
            continue;
        }
        match try_claim_notification(conn, id, event_type) {
            Ok(claimed) => {
                seen.insert(id.to_string());
                let recent = created_at(item).is_none_or(|t| is_recent(t, now, max_age));
                if claimed && recent {
                    events.push(build(item));
                }
            }
            Err(e) => {
                tracing::warn!(id = %id, event_type, "notification claim failed: {e}");
            }
        }
    }
    let current: HashSet<&str> = items.iter().map(&id_of).collect();
    seen.retain(|id| current.contains(id.as_str()));
}

impl DbEventDetector {
    /// Run one poll tick and return the events the caller should emit on the bus.
    pub fn poll(&mut self, conn: &Connection, now: DateTime<Utc>) -> Result<Vec<ConductorEvent>> {
        let max_age = Duration::seconds(NOTIFY_MAX_AGE_SECS);
        let mut events = Vec::new();

        let mgr = AgentManager::new(conn);
        let pending = mgr.list_all_pending_feedback_requests()?;
        collect_new(
            conn,
            &mut self.seen_feedback,
            &pending,
            WEB_FEEDBACK_EVENT_TYPE,
            now,
            max_age,
            |r| r.id.as_str(),
            |r| Some(r.created_at.as_str()),
            |req| {
                let worktree_id = match mgr.get_run(&req.run_id) {
                    Ok(run) => run.and_then(|r| r.worktree_id).unwrap_or_default(),
                    Err(e) => {
                        tracing::warn!(
                            run_id = %req.run_id,
                            feedback_id = %req.id,
                            "failed to resolve worktree for feedback request: {e}"
                        );
                        String::new()
                    }
                };
                ConductorEvent::FeedbackRequested {
                    run_id: req.run_id.clone(),
                    worktree_id,
                    feedback_id: req.id.clone(),
                }
            },
            &mut events,
        );

        let gates = conductor_core::workflow::list_all_waiting_gate_steps(conn)?;
        collect_new(
            conn,
            &mut self.seen_gates,
            &gates,
            WEB_GATE_EVENT_TYPE,
            now,
            max_age,
            |(s, _, _)| s.id.as_str(),
            |(s, _, _)| s.started_at.as_deref(),
            |(step, _, _)| ConductorEvent::WorkflowGateWaiting {
                run_id: step.workflow_run_id.clone(),
                step_id: step.id.clone(),
            },
            &mut events,
        );

        Ok(events)
    }

    /// Emit [`ConductorEvent::SessionIdle`] for interactive sessions idle at least
    /// `delay_secs`, once per idle period. A test convenience over
    /// [`Self::poll_session_idle_candidates`]; the web loop lists candidates itself so the
    /// hook notifier can share the query.
    #[cfg(test)]
    pub fn poll_session_idle(
        &mut self,
        conn: &Connection,
        now: DateTime<Utc>,
        delay_secs: u64,
    ) -> Result<Vec<ConductorEvent>> {
        let candidates = conductor_core::notify::due_idle_candidates(conn, delay_secs, now)?;
        self.poll_session_idle_candidates(conn, now, &candidates)
    }

    /// Emit [`ConductorEvent::SessionIdle`] for this tick's already-listed candidates.
    /// Call only when `[notifications.session_idle] web_push` is on. Details are loaded
    /// only for periods not yet handled. A candidate that fails to load is logged and
    /// skipped so one unreadable row cannot block the others.
    pub fn poll_session_idle_candidates(
        &mut self,
        conn: &Connection,
        now: DateTime<Utc>,
        candidates: &[IdleCandidate],
    ) -> Result<Vec<ConductorEvent>> {
        let max_age = Duration::seconds(NOTIFY_MAX_AGE_SECS);
        let mut keyed: Vec<(String, IdleSession)> = Vec::new();
        self.seen_session_idle.visit_unseen(candidates, |c, key| {
            match load_idle_session_or_skip(conn, c) {
                Ok(s) => {
                    keyed.push((key.to_string(), s));
                    IdleVisit::Handled
                }
                Err(v) => v,
            }
        });
        // `keyed` holds only unhandled periods, so a throwaway seen set is enough here;
        // `collect_new` claims each in `notification_log` and drops the stale ones.
        let mut events = Vec::new();
        collect_new(
            conn,
            &mut HashSet::new(),
            &keyed,
            WEB_SESSION_IDLE_EVENT_TYPE,
            now,
            max_age,
            |(k, _)| k.as_str(),
            |(_, s)| Some(s.idle_since.as_str()),
            |(_, s)| ConductorEvent::SessionIdle {
                run_id: s.run_id.clone(),
                worktree_id: s.worktree_id.clone().unwrap_or_default(),
                worktree_slug: s.worktree_slug.clone().unwrap_or_default(),
                last_message_preview: s.last_message.as_deref().map(preview).unwrap_or_default(),
                background_tasks: s.background_tasks.unwrap_or(0),
            },
            &mut events,
        );
        Ok(events)
    }
}

#[cfg(test)]
impl DbEventDetector {
    fn seen_session_idle_len(&self) -> usize {
        self.seen_session_idle.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::seeded_state_with_agent_run;
    use conductor_core::notify::{due_idle_candidates, idle_candidates_for_tick};
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
        (run.id.clone(), step_id)
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

    const IDLE_AT: &str = "2026-11-02T10:00:00+00:00";

    fn add_idle_session(conn: &Connection, id: &str, idle_since: &str) {
        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at, runtime, idle_since, result_text) \
             VALUES (?1, 'w1', 'p', 'idle', '2026-11-02T09:00:00+00:00', 'interactive', ?2, 'all done')",
            rusqlite::params![id, idle_since],
        )
        .unwrap();
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn session_idle_emits_once_after_delay() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let mut d = DbEventDetector::default();
        let early = at(IDLE_AT) + Duration::seconds(10);
        assert!(d.poll_session_idle(&conn, early, 30).unwrap().is_empty());
        let due = at(IDLE_AT) + Duration::seconds(30);
        let evs = d.poll_session_idle(&conn, due, 30).unwrap();
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            ConductorEvent::SessionIdle {
                run_id,
                worktree_id,
                last_message_preview,
                background_tasks,
                ..
            } => {
                assert_eq!(run_id, "s1");
                assert_eq!(worktree_id, "w1");
                assert_eq!(last_message_preview, "all done");
                assert_eq!(*background_tasks, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(d.poll_session_idle(&conn, due, 30).unwrap().is_empty());
        // A fresh detector (restart) finds the claim in notification_log.
        let mut fresh = DbEventDetector::default();
        assert!(fresh.poll_session_idle(&conn, due, 30).unwrap().is_empty());
    }

    #[test]
    fn session_idle_preview_matches_core_truncation() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let long = "word ".repeat(60);
        conn.execute("UPDATE agent_runs SET result_text = ?1", [&long])
            .unwrap();
        let mut d = DbEventDetector::default();
        let evs = d
            .poll_session_idle(&conn, at(IDLE_AT) + Duration::seconds(60), 0)
            .unwrap();
        match &evs[0] {
            ConductorEvent::SessionIdle {
                last_message_preview,
                ..
            } => assert_eq!(last_message_preview, &preview(&long)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn session_idle_new_period_emits_again() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let mut d = DbEventDetector::default();
        let now = at(IDLE_AT) + Duration::seconds(60);
        assert_eq!(d.poll_session_idle(&conn, now, 0).unwrap().len(), 1);
        let later = "2026-11-02T10:05:00+00:00";
        conn.execute(
            "UPDATE agent_runs SET idle_since = ?1 WHERE id = 's1'",
            [later],
        )
        .unwrap();
        let now2 = at(later) + Duration::seconds(1);
        assert_eq!(d.poll_session_idle(&conn, now2, 0).unwrap().len(), 1);
    }

    #[test]
    fn plain_poll_never_emits_session_idle() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let mut d = DbEventDetector::default();
        let evs = d.poll(&conn, at(IDLE_AT) + Duration::seconds(600)).unwrap();
        assert!(evs
            .iter()
            .all(|e| !matches!(e, ConductorEvent::SessionIdle { .. })));
    }

    #[test]
    fn session_idle_handled_period_skipped_without_reload_and_pruned() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let now = at(IDLE_AT) + Duration::seconds(60);
        let cands = due_idle_candidates(&conn, 0, now).unwrap();
        let mut d = DbEventDetector::default();
        assert_eq!(
            d.poll_session_idle_candidates(&conn, now, &cands)
                .unwrap()
                .len(),
            1
        );
        // Same candidates again: skipped from memory.
        assert!(d
            .poll_session_idle_candidates(&conn, now, &cands)
            .unwrap()
            .is_empty());
        // Candidate leaves the list: pruned from the seen set.
        assert!(d
            .poll_session_idle_candidates(&conn, now, &[])
            .unwrap()
            .is_empty());
        assert_eq!(d.seen_session_idle_len(), 0);
    }

    #[test]
    fn session_idle_run_that_left_idle_is_dropped_then_can_emit() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let now = at(IDLE_AT) + Duration::seconds(60);
        let cands = due_idle_candidates(&conn, 0, now).unwrap();
        conn.execute(
            "UPDATE agent_runs SET status = 'running' WHERE id = 's1'",
            [],
        )
        .unwrap();
        let mut d = DbEventDetector::default();
        assert!(d
            .poll_session_idle_candidates(&conn, now, &cands)
            .unwrap()
            .is_empty());
        assert_eq!(d.seen_session_idle_len(), 0);
        conn.execute("UPDATE agent_runs SET status = 'idle' WHERE id = 's1'", [])
            .unwrap();
        assert_eq!(
            d.poll_session_idle_candidates(&conn, now, &cands)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn session_idle_unreadable_candidate_does_not_block_others() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "bad", IDLE_AT);
        add_idle_session(&conn, "good", "2026-11-02T10:00:01+00:00");
        // A non-integer background_tasks makes the bad run's detail load fail.
        conn.execute(
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, metadata) \
             VALUES ('e1', 'bad', 'result', 'x', ?1, '{\"background_tasks\": \"many\"}')",
            [IDLE_AT],
        )
        .unwrap();
        let now = at(IDLE_AT) + Duration::seconds(60);
        let cands = due_idle_candidates(&conn, 0, now).unwrap();
        let mut d = DbEventDetector::default();
        let evs = d.poll_session_idle_candidates(&conn, now, &cands).unwrap();
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], ConductorEvent::SessionIdle { run_id, .. } if run_id == "good"));
    }

    fn cfg_with(web_push: bool, hooks_enabled: bool) -> conductor_core::config::Config {
        let mut cfg = conductor_core::config::Config::default();
        cfg.notifications.enabled = hooks_enabled;
        cfg.notifications.session_idle.web_push = web_push;
        cfg.notifications.session_idle.delay_secs = 0;
        cfg
    }

    #[test]
    fn idle_candidates_for_tick_gating() {
        let (_tmp, conn) = open();
        add_idle_session(&conn, "s1", IDLE_AT);
        let now = at(IDLE_AT) + Duration::seconds(60);
        let tick = |cfg: &conductor_core::config::Config| {
            idle_candidates_for_tick(&conn, &cfg.notifications, &cfg.notify.hooks, now)
        };
        assert!(tick(&cfg_with(false, false)).is_empty());
        for cfg in [
            cfg_with(true, false),
            cfg_with(false, true),
            cfg_with(true, true),
        ] {
            let c = tick(&cfg);
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].run_id, "s1");
        }
    }
}
