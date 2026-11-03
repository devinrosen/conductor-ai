//! `session.idle`: an interactive session finished a turn and has stayed idle for
//! `[notifications.session_idle] delay_secs`.
//!
//! The hook only writes state (`status = 'idle'`, `idle_since`). The TUI and web pollers
//! call [`SessionIdleNotifier::poll`], which finds sessions idle long enough and fires
//! once per idle period, deduped on `(run_id, idle_since)` in `notification_log` so the
//! TUI and web running together still notify once.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use runkon_notify::{DedupStore, Event, HookRunner, Severity};
use rusqlite::{params, Connection, OptionalExtension};

use crate::config::{hooks_as_runkon, HookConfig, NotificationConfig};
use crate::error::Result;
use crate::session::hook::{stop_turn_details, PLACEHOLDER_PROMPT, RUNTIME_INTERACTIVE};

use super::{is_recent, notification_sink_configured, try_claim_notification, NOTIFY_MAX_AGE_SECS};

/// `notification_log.event_type` for hook delivery of `session.idle`.
pub const SESSION_IDLE_EVENT_TYPE: &str = "session_idle";

const PREVIEW_CHARS: usize = 100;

/// An interactive session that has been idle for at least the configured delay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleSession {
    pub run_id: String,
    pub session_id: Option<String>,
    /// RFC 3339 time of the `Stop` that began this idle period.
    pub idle_since: String,
    pub worktree_id: Option<String>,
    pub worktree_slug: Option<String>,
    pub repo_slug: String,
    pub branch: String,
    /// The turn's prompt; empty for the "(interactive session)" placeholder.
    pub prompt: String,
    pub last_message: Option<String>,
    /// Background task count reported by `Stop`; `None` when the field was absent.
    pub background_tasks: Option<i64>,
    /// Prompt submit to `Stop`, when the prompt span is known.
    pub turn_duration_ms: Option<i64>,
}

/// Dedup key for one idle period of one run.
pub fn session_idle_dedup_key(run_id: &str, idle_since: &str) -> String {
    format!("{run_id}|{idle_since}")
}

/// Single-line, length-capped preview of `s`, ending in `…` when cut.
///
/// Shared by the hook event and the web push payload so both show the same text.
pub fn preview(s: &str) -> String {
    let single: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let single = single.trim();
    if single.chars().count() > PREVIEW_CHARS {
        let cut: String = single.chars().take(PREVIEW_CHARS).collect();
        format!("{cut}\u{2026}")
    } else {
        single.to_string()
    }
}

/// Notification body: `"<label>: <last>"`, or whichever of the two is non-empty.
pub fn session_idle_body(label: &str, last: &str) -> String {
    match (label.is_empty(), last.is_empty()) {
        (_, true) => label.to_string(),
        (true, false) => last.to_string(),
        (false, false) => format!("{label}: {last}"),
    }
}

/// An idle period that has passed the delay, identified but not yet loaded.
///
/// Cheap to produce (`agent_runs` only), so the pollers can list candidates every tick,
/// skip the ones already handled, and load details only for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleCandidate {
    pub run_id: String,
    pub idle_since: String,
}

impl IdleCandidate {
    pub fn dedup_key(&self) -> String {
        session_idle_dedup_key(&self.run_id, &self.idle_since)
    }
}

/// Interactive runs that are `idle` with `idle_since` at least `delay_secs` before `now`.
///
/// The delay is compared in Rust: hook timestamps are RFC 3339 with an explicit offset,
/// and string comparison in SQL is fragile across formats. Rows with an unparseable
/// `idle_since` are skipped (never fail open, or they would fire on every tick).
pub fn due_idle_candidates(
    conn: &Connection,
    delay_secs: u64,
    now: DateTime<Utc>,
) -> Result<Vec<IdleCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT id, idle_since FROM agent_runs \
         WHERE runtime = ?1 AND status = 'idle' AND idle_since IS NOT NULL \
         ORDER BY idle_since, id",
    )?;
    let rows = stmt.query_map(params![RUNTIME_INTERACTIVE], |row| {
        Ok(IdleCandidate {
            run_id: row.get(0)?,
            idle_since: row.get(1)?,
        })
    })?;
    let mut due = Vec::new();
    for row in rows {
        let c = row?;
        let idle_at = match DateTime::parse_from_rfc3339(&c.idle_since) {
            Ok(t) => t.with_timezone(&Utc),
            Err(e) => {
                tracing::warn!(run_id = %c.run_id, idle_since = %c.idle_since, "unparseable idle_since: {e}");
                continue;
            }
        };
        if now.signed_duration_since(idle_at).num_seconds() >= delay_secs as i64 {
            due.push(c);
        }
    }
    Ok(due)
}

/// Load the full [`IdleSession`] for a candidate. `None` when the run has left `idle`
/// (or started a new idle period) since the candidate was listed.
pub fn load_idle_session(conn: &Connection, c: &IdleCandidate) -> Result<Option<IdleSession>> {
    let row = conn
        .query_row(
            "SELECT ar.session_id, ar.worktree_id, w.slug, \
                    COALESCE(r.slug, ''), COALESCE(w.branch, ''), ar.prompt, ar.result_text \
             FROM agent_runs ar \
             LEFT JOIN worktrees w ON w.id = ar.worktree_id \
             LEFT JOIN repos r ON r.id = COALESCE(ar.repo_id, w.repo_id) \
             WHERE ar.id = ?1 AND ar.status = 'idle' AND ar.idle_since = ?2",
            params![c.run_id, c.idle_since],
            |row| {
                Ok(IdleSession {
                    run_id: c.run_id.clone(),
                    session_id: row.get(0)?,
                    idle_since: c.idle_since.clone(),
                    worktree_id: row.get(1)?,
                    worktree_slug: row.get(2)?,
                    repo_slug: row.get(3)?,
                    branch: row.get(4)?,
                    prompt: row.get(5)?,
                    last_message: row.get(6)?,
                    background_tasks: None,
                    turn_duration_ms: None,
                })
            },
        )
        .optional()?;
    let Some(mut s) = row else {
        return Ok(None);
    };
    let (background_tasks, prompt_started) = stop_turn_details(conn, &c.run_id, &c.idle_since)?;
    s.background_tasks = background_tasks;
    if s.prompt == PLACEHOLDER_PROMPT {
        s.prompt.clear();
    }
    s.turn_duration_ms = prompt_started
        .and_then(|p| DateTime::parse_from_rfc3339(&p).ok())
        .zip(DateTime::parse_from_rfc3339(&c.idle_since).ok())
        .map(|(p, idle_at)| idle_at.signed_duration_since(p).num_milliseconds().max(0));
    Ok(Some(s))
}

/// Due idle sessions with details loaded. Convenience over [`due_idle_candidates`] and
/// [`load_idle_session`]; the pollers use those directly to skip handled periods.
pub fn list_due_idle_sessions(
    conn: &Connection,
    delay_secs: u64,
    now: DateTime<Utc>,
) -> Result<Vec<IdleSession>> {
    let mut out = Vec::new();
    for c in due_idle_candidates(conn, delay_secs, now)? {
        if let Some(s) = load_idle_session(conn, &c)? {
            out.push(s);
        }
    }
    Ok(out)
}

/// Build the `session.idle` event.
///
/// The body is `"<worktree or repo slug>: <last message preview>"`, or just the label
/// when the stop carried no message.
pub(crate) fn session_idle_event(s: &IdleSession) -> Event {
    let label = s
        .worktree_slug
        .as_deref()
        .filter(|l| !l.is_empty())
        .or(Some(s.repo_slug.as_str()).filter(|l| !l.is_empty()))
        .unwrap_or("");
    let last = s.last_message.as_deref().map(preview).unwrap_or_default();
    let body = session_idle_body(label, &last);
    Event {
        kind: "session.idle".into(),
        title: "Conductor \u{2014} Session idle".into(),
        body,
        severity: Severity::Info,
        fields: [
            ("run_id".into(), s.run_id.clone()),
            (
                "session_id".into(),
                s.session_id.clone().unwrap_or_default(),
            ),
            ("repo_slug".into(), s.repo_slug.clone()),
            ("branch".into(), s.branch.clone()),
            (
                "worktree_slug".into(),
                s.worktree_slug.clone().unwrap_or_default(),
            ),
            ("prompt_preview".into(), preview(&s.prompt)),
            ("last_message_preview".into(), last),
            (
                "background_tasks".into(),
                s.background_tasks.unwrap_or(0).to_string(),
            ),
            (
                "turn_duration_ms".into(),
                s.turn_duration_ms
                    .map(|m| m.to_string())
                    .unwrap_or_default(),
            ),
            ("idle_since".into(), s.idle_since.clone()),
            ("timestamp".into(), Utc::now().to_rfc3339()),
        ]
        .into_iter()
        .collect(),
    }
}

/// Fires `session.idle` for sessions that have been idle past the delay, once per idle
/// period. Holds the keys already handled so a steady poll doesn't hit `notification_log`
/// every tick.
#[derive(Debug, Default)]
pub struct SessionIdleNotifier {
    seen: HashSet<String>,
}

impl SessionIdleNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Find due idle sessions and fire `session.idle` for each new idle period.
    ///
    /// Returns the number of events dispatched. Does nothing unless a notification sink
    /// is configured. Idle periods older than [`NOTIFY_MAX_AGE_SECS`] are claimed without
    /// firing, so a first start doesn't announce stale sessions.
    pub fn poll(
        &mut self,
        conn: &Connection,
        config: &NotificationConfig,
        hooks: &[HookConfig],
        dedup_store: Arc<dyn DedupStore>,
        now: DateTime<Utc>,
    ) -> usize {
        if !notification_sink_configured(config, hooks) {
            return 0;
        }
        let candidates = match due_idle_candidates(conn, config.session_idle.delay_secs, now) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("session idle lookup failed: {e}");
                return 0;
            }
        };
        self.poll_candidates(conn, config, hooks, dedup_store, now, &candidates)
    }

    /// Like [`Self::poll`], for a caller that already listed the tick's candidates (the
    /// web loop shares one [`due_idle_candidates`] query with its Web Push detector).
    /// Details are loaded only for candidates this notifier has not handled yet.
    pub fn poll_candidates(
        &mut self,
        conn: &Connection,
        config: &NotificationConfig,
        hooks: &[HookConfig],
        dedup_store: Arc<dyn DedupStore>,
        now: DateTime<Utc>,
        candidates: &[IdleCandidate],
    ) -> usize {
        if !notification_sink_configured(config, hooks) {
            return 0;
        }
        let current: HashSet<String> = candidates.iter().map(IdleCandidate::dedup_key).collect();
        let max_age = chrono::Duration::seconds(NOTIFY_MAX_AGE_SECS);
        let runner = HookRunner::new(&hooks_as_runkon(hooks)).with_dedup_store(dedup_store);

        let mut fired = 0;
        for c in candidates {
            let key = c.dedup_key();
            if self.seen.contains(&key) {
                continue;
            }
            if !is_recent(&c.idle_since, now, max_age) {
                self.seen.insert(key.clone());
                if let Err(e) = try_claim_notification(conn, &key, SESSION_IDLE_EVENT_TYPE) {
                    tracing::warn!("session idle stale claim failed: {e}");
                }
                continue;
            }
            let session = match load_idle_session(conn, c) {
                Ok(Some(s)) => s,
                // Left idle since listed; the next tick sees its new state.
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!("session idle load failed: {e}");
                    continue;
                }
            };
            self.seen.insert(key.clone());
            runner.fire_with_dedup(&session_idle_event(&session), &key, SESSION_IDLE_EVENT_TYPE);
            fired += 1;
        }
        self.seen.retain(|k| current.contains(k));
        fired
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::{json, Value};

    use super::*;
    use crate::session::hook::{apply_hook_event, HookEnv, HookEvent};
    use crate::test_helpers::setup_db;

    const T0: &str = "2026-11-02T10:00:00+00:00";
    const T_STOP: &str = "2026-11-02T10:00:20+00:00";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn hook(name: &str, extra: Value, when: &str) -> HookEvent {
        let mut v = json!({
            "hook_event_name": name,
            "session_id": "s1",
            "cwd": "/tmp/ws/feat-test/src",
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        let env = HookEnv {
            attended: true,
            claude_pid: Some(std::process::id()),
            tmux_pane: None,
        };
        HookEvent::from_payload(&v, env, when.to_string())
    }

    fn apply(conn: &Connection, name: &str, extra: Value, when: &str) {
        apply_hook_event(conn, &hook(name, extra, when)).unwrap();
    }

    fn turn(conn: &Connection, start: &str, stop: &str, extra: Value) {
        apply(
            conn,
            "UserPromptSubmit",
            json!({"prompt": "do it", "prompt_id": "p"}),
            start,
        );
        let mut e = json!({"last_assistant_message": "all done", "prompt_id": "p"});
        for (k, v) in extra.as_object().unwrap() {
            e[k] = v.clone();
        }
        apply(conn, "Stop", e, stop);
    }

    fn secs(base: &str, n: i64) -> DateTime<Utc> {
        at(base) + chrono::Duration::seconds(n)
    }

    #[derive(Default)]
    struct RecordingStore(Mutex<Vec<(String, String)>>);
    impl DedupStore for RecordingStore {
        fn try_claim(&self, entity_id: &str, event_type: &str) -> runkon_notify::Result<bool> {
            let mut g = self.0.lock().unwrap();
            let k = (entity_id.to_string(), event_type.to_string());
            if g.contains(&k) {
                Ok(false)
            } else {
                g.push(k);
                Ok(true)
            }
        }
    }

    fn enabled() -> NotificationConfig {
        NotificationConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn due_only_after_delay() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        assert!(list_due_idle_sessions(&conn, 30, secs(T_STOP, 29))
            .unwrap()
            .is_empty());
        let due = list_due_idle_sessions(&conn, 30, secs(T_STOP, 30)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].worktree_slug.as_deref(), Some("feat-test"));
        assert_eq!(due[0].repo_slug, "test-repo");
        assert_eq!(due[0].prompt, "do it");
        assert_eq!(due[0].last_message.as_deref(), Some("all done"));
    }

    #[test]
    fn prompt_within_delay_suppresses() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        apply(
            &conn,
            "UserPromptSubmit",
            json!({"prompt": "more"}),
            "2026-11-02T10:00:30+00:00",
        );
        assert!(list_due_idle_sessions(&conn, 0, secs(T_STOP, 600))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn background_tasks_counted() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({"background_tasks": [1, 2]}));
        let due = list_due_idle_sessions(&conn, 0, secs(T_STOP, 1)).unwrap();
        assert_eq!(due[0].background_tasks, Some(2));
        assert_eq!(session_idle_event(&due[0]).fields["background_tasks"], "2");
    }

    #[test]
    fn missing_background_tasks_is_zero_in_event() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let due = list_due_idle_sessions(&conn, 0, secs(T_STOP, 1)).unwrap();
        assert_eq!(due[0].background_tasks, None);
        assert_eq!(session_idle_event(&due[0]).fields["background_tasks"], "0");
    }

    #[test]
    fn turn_duration_spans_prompt_to_stop() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let due = list_due_idle_sessions(&conn, 0, secs(T_STOP, 1)).unwrap();
        assert_eq!(due[0].turn_duration_ms, Some(20_000));
        assert_eq!(
            session_idle_event(&due[0]).fields["turn_duration_ms"],
            "20000"
        );
    }

    #[test]
    fn new_turn_gets_new_key() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let k1 = {
            let d = list_due_idle_sessions(&conn, 0, secs(T_STOP, 1)).unwrap();
            session_idle_dedup_key(&d[0].run_id, &d[0].idle_since)
        };
        let stop2 = "2026-11-02T11:00:00+00:00";
        turn(&conn, "2026-11-02T10:59:00+00:00", stop2, json!({}));
        let d = list_due_idle_sessions(&conn, 0, secs(stop2, 1)).unwrap();
        let k2 = session_idle_dedup_key(&d[0].run_id, &d[0].idle_since);
        assert_ne!(k1, k2);
        assert!(try_claim_notification(&conn, &k1, SESSION_IDLE_EVENT_TYPE).unwrap());
        assert!(try_claim_notification(&conn, &k2, SESSION_IDLE_EVENT_TYPE).unwrap());
    }

    #[test]
    fn session_start_alone_and_resume_do_not_fire() {
        let conn = setup_db();
        apply(&conn, "SessionStart", json!({"source": "startup"}), T0);
        assert!(list_due_idle_sessions(&conn, 0, secs(T0, 600))
            .unwrap()
            .is_empty());
        turn(
            &conn,
            "2026-11-02T10:01:00+00:00",
            "2026-11-02T10:02:00+00:00",
            json!({}),
        );
        apply(
            &conn,
            "SessionStart",
            json!({"source": "resume"}),
            "2026-11-02T10:03:00+00:00",
        );
        assert!(list_due_idle_sessions(&conn, 0, secs(T0, 6000))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn headless_runs_never_listed() {
        let conn = setup_db();
        crate::test_helpers::insert_test_agent_run(&conn, "ar1", "w1");
        conn.execute(
            "UPDATE agent_runs SET status = 'idle', idle_since = ?1 WHERE id = 'ar1'",
            [T_STOP],
        )
        .unwrap();
        assert!(list_due_idle_sessions(&conn, 0, secs(T_STOP, 600))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unparseable_idle_since_is_skipped() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        conn.execute("UPDATE agent_runs SET idle_since = 'garbage'", [])
            .unwrap();
        assert!(list_due_idle_sessions(&conn, 0, secs(T_STOP, 600))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn event_shape() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let due = list_due_idle_sessions(&conn, 0, secs(T_STOP, 1)).unwrap();
        let ev = session_idle_event(&due[0]);
        assert_eq!(ev.kind, "session.idle");
        assert_eq!(ev.title, "Conductor \u{2014} Session idle");
        assert_eq!(ev.body, "feat-test: all done");
        assert_eq!(ev.fields["worktree_slug"], "feat-test");
        assert_eq!(ev.fields["prompt_preview"], "do it");
    }

    #[test]
    fn preview_single_lines_and_truncates() {
        assert_eq!(preview("a\nb\n"), "a b");
        let long = "x".repeat(300);
        assert_eq!(preview(&long).chars().count(), PREVIEW_CHARS + 1);
    }

    #[test]
    fn poll_fires_once_per_period() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let cfg = enabled();
        assert_eq!(n.poll(&conn, &cfg, &[], store.clone(), secs(T_STOP, 29)), 0);
        assert_eq!(n.poll(&conn, &cfg, &[], store.clone(), secs(T_STOP, 30)), 1);
        assert_eq!(n.poll(&conn, &cfg, &[], store.clone(), secs(T_STOP, 32)), 0);
        // A second poller (TUI + web) shares the dedup store: claimed, so it does not
        // dispatch... but it still counts as an attempt; the store sees one claim.
        let mut other = SessionIdleNotifier::new();
        other.poll(&conn, &cfg, &[], store.clone(), secs(T_STOP, 33));
        assert_eq!(store.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn poll_without_sink_does_nothing() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let cfg = NotificationConfig::default();
        assert_eq!(
            n.poll(&conn, &cfg, &[], store.clone(), secs(T_STOP, 600)),
            0
        );
        assert!(store.0.lock().unwrap().is_empty());
    }

    #[test]
    fn poll_delay_zero_fires_immediately_and_new_turn_fires_again() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let mut cfg = enabled();
        cfg.session_idle.delay_secs = 0;
        assert_eq!(n.poll(&conn, &cfg, &[], store.clone(), at(T_STOP)), 1);
        let stop2 = "2026-11-02T10:05:00+00:00";
        turn(&conn, "2026-11-02T10:04:00+00:00", stop2, json!({}));
        assert_eq!(n.poll(&conn, &cfg, &[], store.clone(), at(stop2)), 1);
        assert_eq!(store.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn poll_stale_idle_is_claimed_not_fired() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let later = secs(T_STOP, NOTIFY_MAX_AGE_SECS + 60);
        assert_eq!(n.poll(&conn, &enabled(), &[], store.clone(), later), 0);
        let claimed: i64 = conn
            .query_row("SELECT COUNT(*) FROM notification_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(claimed, 1);
    }
}
