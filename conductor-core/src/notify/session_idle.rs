//! `session.idle`: an interactive session finished a turn and has stayed idle for
//! `[notifications.session_idle] delay_secs`.
//!
//! The hook only writes state (`status = 'idle'`, `idle_since`). The TUI and web pollers
//! call [`SessionIdleNotifier::poll`], which finds sessions idle long enough and fires
//! once per idle period, deduped on `(run_id, idle_since)` in `notification_log` so the
//! TUI and web running together still notify once.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

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

/// How long a period whose details failed to load is left alone before it is retried.
pub const IDLE_LOAD_RETRY_SECS: u64 = 300;

/// What a poller decided about one idle period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleVisit {
    /// Fired, claimed, or intentionally dropped: never visit this period again.
    Handled,
    /// Not ready (e.g. the run left idle since it was listed): visit again next tick.
    Retry,
    /// Failed to load: visit again only after [`IDLE_LOAD_RETRY_SECS`], so a permanently
    /// unreadable row is neither re-queried nor re-logged every tick.
    Backoff,
}

/// Idle periods a poller has already handled, so a steady poll neither reloads details
/// nor re-claims `notification_log` rows every tick.
///
/// Shared by the hook notifier here and the web push detector so both follow one rule:
/// a period is remembered only once `visit` reports it handled, failed loads back off,
/// and the sets are trimmed to the periods still listed so they cannot grow without bound.
#[derive(Debug, Default)]
pub struct SeenIdlePeriods {
    handled: HashSet<String>,
    retry_at: HashMap<String, Instant>,
}

impl SeenIdlePeriods {
    /// Call `visit(candidate, dedup_key)` for each candidate not yet handled and not
    /// backing off, and record its [`IdleVisit`] outcome.
    pub fn visit_unseen(
        &mut self,
        candidates: &[IdleCandidate],
        visit: impl FnMut(&IdleCandidate, &str) -> IdleVisit,
    ) {
        self.visit_unseen_at(candidates, Instant::now(), visit)
    }

    fn visit_unseen_at(
        &mut self,
        candidates: &[IdleCandidate],
        now: Instant,
        mut visit: impl FnMut(&IdleCandidate, &str) -> IdleVisit,
    ) {
        let mut current = HashSet::with_capacity(candidates.len());
        for c in candidates {
            let key = c.dedup_key();
            let skip =
                self.handled.contains(&key) || self.retry_at.get(&key).is_some_and(|t| now < *t);
            if !skip {
                match visit(c, &key) {
                    IdleVisit::Handled => {
                        self.retry_at.remove(&key);
                        self.handled.insert(key.clone());
                    }
                    IdleVisit::Retry => {
                        self.retry_at.remove(&key);
                    }
                    IdleVisit::Backoff => {
                        self.retry_at.insert(
                            key.clone(),
                            now + std::time::Duration::from_secs(IDLE_LOAD_RETRY_SECS),
                        );
                    }
                }
            }
            current.insert(key);
        }
        self.handled.retain(|k| current.contains(k));
        self.retry_at.retain(|k, _| current.contains(k));
    }

    pub fn len(&self) -> usize {
        self.handled.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handled.is_empty()
    }
}

/// Load a candidate's details for a poller, or say how to treat the period instead.
///
/// `Err(Retry)` when the run left idle since listing (the next tick sees its new state);
/// `Err(Backoff)` after logging a load failure. Shared so every poller skips identically.
pub fn load_idle_session_or_skip(
    conn: &Connection,
    c: &IdleCandidate,
) -> std::result::Result<IdleSession, IdleVisit> {
    match load_idle_session(conn, c) {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(IdleVisit::Retry),
        Err(e) => {
            tracing::warn!(
                run_id = %c.run_id, idle_since = %c.idle_since,
                "session idle load failed, retrying in {IDLE_LOAD_RETRY_SECS}s: {e}"
            );
            Err(IdleVisit::Backoff)
        }
    }
}

/// The tick's shared idle candidates, listed once for the hook notifier and the Web Push
/// detector (web) or the notifier alone (TUI). Empty, without a query, when neither a
/// notification sink nor `web_push` is on.
pub fn idle_candidates_for_tick(
    conn: &Connection,
    config: &NotificationConfig,
    hooks: &[HookConfig],
    now: DateTime<Utc>,
) -> Vec<IdleCandidate> {
    if !(notification_sink_configured(config, hooks) || config.session_idle.web_push) {
        return Vec::new();
    }
    due_idle_candidates(conn, config.session_idle.delay_secs, now).unwrap_or_else(|e| {
        tracing::warn!("session idle lookup failed: {e}");
        Vec::new()
    })
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

/// Due idle sessions with details loaded. A test convenience over
/// [`due_idle_candidates`] and [`load_idle_session`]; the pollers use those directly to
/// skip handled periods.
#[cfg(test)]
fn list_due_idle_sessions(
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
    seen: SeenIdlePeriods,
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
        let max_age = chrono::Duration::seconds(NOTIFY_MAX_AGE_SECS);
        let runner = HookRunner::new(&hooks_as_runkon(hooks)).with_dedup_store(dedup_store);

        let mut fired = 0;
        self.seen.visit_unseen(candidates, |c, key| {
            if !is_recent(&c.idle_since, now, max_age) {
                if let Err(e) = try_claim_notification(conn, key, SESSION_IDLE_EVENT_TYPE) {
                    tracing::warn!("session idle stale claim failed: {e}");
                }
                return IdleVisit::Handled;
            }
            let session = match load_idle_session_or_skip(conn, c) {
                Ok(s) => s,
                Err(v) => return v,
            };
            runner.fire_with_dedup(&session_idle_event(&session), key, SESSION_IDLE_EVENT_TYPE);
            fired += 1;
            IdleVisit::Handled
        });
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

    fn candidate(conn: &Connection) -> IdleCandidate {
        let c = due_idle_candidates(conn, 0, secs(T_STOP, 1)).unwrap();
        assert_eq!(c.len(), 1);
        c.into_iter().next().unwrap()
    }

    #[test]
    fn dedup_key_combines_run_and_idle_since() {
        let c = IdleCandidate {
            run_id: "r".into(),
            idle_since: "t".into(),
        };
        assert_eq!(c.dedup_key(), session_idle_dedup_key("r", "t"));
        assert_eq!(c.dedup_key(), "r|t");
    }

    #[test]
    fn load_idle_session_returns_fields() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({"background_tasks": [1]}));
        let c = candidate(&conn);
        let s = load_idle_session(&conn, &c).unwrap().unwrap();
        assert_eq!(s.run_id, c.run_id);
        assert_eq!(s.idle_since, T_STOP);
        assert_eq!(s.session_id.as_deref(), Some("s1"));
        assert_eq!(s.worktree_slug.as_deref(), Some("feat-test"));
        assert_eq!(s.repo_slug, "test-repo");
        assert_eq!(s.prompt, "do it");
        assert_eq!(s.last_message.as_deref(), Some("all done"));
        assert_eq!(s.background_tasks, Some(1));
        assert_eq!(s.turn_duration_ms, Some(20_000));
    }

    #[test]
    fn load_idle_session_none_after_run_leaves_idle() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        apply(
            &conn,
            "UserPromptSubmit",
            json!({"prompt": "more"}),
            "2026-11-02T10:00:30+00:00",
        );
        assert_eq!(load_idle_session(&conn, &c).unwrap(), None);
    }

    #[test]
    fn load_idle_session_none_when_idle_since_changed() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        turn(
            &conn,
            "2026-11-02T10:01:00+00:00",
            "2026-11-02T10:02:00+00:00",
            json!({}),
        );
        // Idle again, but in a new period: the stale candidate must not load.
        assert_eq!(load_idle_session(&conn, &c).unwrap(), None);
    }

    #[test]
    fn load_idle_session_clears_placeholder_prompt() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        conn.execute("UPDATE agent_runs SET prompt = ?1", [PLACEHOLDER_PROMPT])
            .unwrap();
        let s = load_idle_session(&conn, &candidate(&conn))
            .unwrap()
            .unwrap();
        assert_eq!(s.prompt, "");
    }

    #[test]
    fn load_idle_session_without_prompt_event_has_no_duration() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        conn.execute("DELETE FROM agent_run_events WHERE kind = 'prompt'", [])
            .unwrap();
        let s = load_idle_session(&conn, &candidate(&conn))
            .unwrap()
            .unwrap();
        assert_eq!(s.turn_duration_ms, None);
    }

    #[test]
    fn load_idle_session_or_skip_ok_for_valid_idle_run() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        let s = load_idle_session_or_skip(&conn, &c).unwrap();
        assert_eq!(s.run_id, c.run_id);
        assert_eq!(s.idle_since, T_STOP);
    }

    #[test]
    fn load_idle_session_or_skip_retries_when_run_left_idle() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        apply(
            &conn,
            "UserPromptSubmit",
            json!({"prompt": "more"}),
            "2026-11-02T10:00:30+00:00",
        );
        assert_eq!(
            load_idle_session_or_skip(&conn, &c).unwrap_err(),
            IdleVisit::Retry
        );
    }

    #[test]
    fn load_idle_session_or_skip_backs_off_on_load_failure() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        conn.execute("DROP TABLE agent_run_events", []).unwrap();
        assert_eq!(
            load_idle_session_or_skip(&conn, &c).unwrap_err(),
            IdleVisit::Backoff
        );
    }

    #[test]
    fn due_idle_candidates_respects_delay_and_skips_unparseable() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        assert!(due_idle_candidates(&conn, 30, secs(T_STOP, 29))
            .unwrap()
            .is_empty());
        assert_eq!(
            due_idle_candidates(&conn, 30, secs(T_STOP, 30))
                .unwrap()
                .len(),
            1
        );
        conn.execute("UPDATE agent_runs SET idle_since = 'garbage'", [])
            .unwrap();
        assert!(due_idle_candidates(&conn, 0, secs(T_STOP, 600))
            .unwrap()
            .is_empty());
    }

    fn cand(run: &str, since: &str) -> IdleCandidate {
        IdleCandidate {
            run_id: run.into(),
            idle_since: since.into(),
        }
    }

    #[test]
    fn seen_periods_skip_handled_retry_unhandled_and_prune() {
        let mut seen = SeenIdlePeriods::default();
        let a = cand("a", "1");
        let b = cand("b", "1");
        let mut visited = Vec::new();
        seen.visit_unseen(&[a.clone(), b.clone()], |c, _| {
            visited.push(c.run_id.clone());
            // b is not handled, so it is retried
            if c.run_id == "a" {
                IdleVisit::Handled
            } else {
                IdleVisit::Retry
            }
        });
        assert_eq!(visited, ["a", "b"]);
        assert_eq!(seen.len(), 1);
        visited.clear();
        seen.visit_unseen(&[a.clone(), b.clone()], |c, _| {
            visited.push(c.run_id.clone());
            IdleVisit::Handled
        });
        assert_eq!(visited, ["b"]);
        assert_eq!(seen.len(), 2);
        // `a` leaves the list: pruned, and a new period for the same run is visited.
        visited.clear();
        seen.visit_unseen(&[cand("b", "1"), cand("a", "2")], |c, _| {
            visited.push(c.dedup_key());
            IdleVisit::Handled
        });
        assert_eq!(visited, ["a|2"]);
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn seen_periods_back_off_failed_loads_then_retry() {
        let mut seen = SeenIdlePeriods::default();
        let a = [cand("a", "1")];
        let t0 = Instant::now();
        let mut visits = 0;
        seen.visit_unseen_at(&a, t0, |_, _| {
            visits += 1;
            IdleVisit::Backoff
        });
        seen.visit_unseen_at(&a, t0 + std::time::Duration::from_secs(10), |_, _| {
            visits += 1;
            IdleVisit::Backoff
        });
        assert_eq!(visits, 1, "backing off: not revisited");
        let later = t0 + std::time::Duration::from_secs(IDLE_LOAD_RETRY_SECS + 1);
        seen.visit_unseen_at(&a, later, |_, _| {
            visits += 1;
            IdleVisit::Handled
        });
        assert_eq!(visits, 2);
        assert_eq!(seen.len(), 1);
        // Leaving the list prunes the backoff entry.
        seen.visit_unseen_at(&[], later, |_, _| IdleVisit::Handled);
        assert!(seen.is_empty() && seen.retry_at.is_empty());
    }

    fn poll_cands(
        n: &mut SessionIdleNotifier,
        conn: &Connection,
        store: &Arc<RecordingStore>,
        cands: &[IdleCandidate],
        now: DateTime<Utc>,
    ) -> usize {
        n.poll_candidates(conn, &enabled(), &[], store.clone(), now, cands)
    }

    #[test]
    fn poll_candidates_left_idle_is_not_marked_seen() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        // The run leaves idle between listing and loading.
        apply(
            &conn,
            "UserPromptSubmit",
            json!({"prompt": "more"}),
            "2026-11-02T10:00:30+00:00",
        );
        let now = secs(T_STOP, 31);
        assert_eq!(
            poll_cands(&mut n, &conn, &store, std::slice::from_ref(&c), now),
            0
        );
        assert_eq!(n.seen.len(), 0);
        // It returns to the same idle_since (e.g. the listing was merely stale): it can fire.
        conn.execute(
            "UPDATE agent_runs SET status = 'idle', idle_since = ?1",
            [T_STOP],
        )
        .unwrap();
        assert_eq!(poll_cands(&mut n, &conn, &store, &[c], now), 1);
    }

    #[test]
    fn poll_candidates_does_not_reload_handled_period() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let now = secs(T_STOP, 31);
        assert_eq!(
            poll_cands(&mut n, &conn, &store, std::slice::from_ref(&c), now),
            1
        );
        // Break the detail query: a handled period is skipped before it is ever loaded.
        conn.execute("DROP TABLE agent_run_events", []).unwrap();
        assert_eq!(poll_cands(&mut n, &conn, &store, &[c], now), 0);
        assert_eq!(store.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn poll_candidates_stale_period_is_claimed_and_not_fired() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let later = secs(T_STOP, NOTIFY_MAX_AGE_SECS + 60);
        assert_eq!(
            poll_cands(&mut n, &conn, &store, std::slice::from_ref(&c), later),
            0
        );
        assert!(store.0.lock().unwrap().is_empty());
        assert!(!try_claim_notification(&conn, &c.dedup_key(), SESSION_IDLE_EVENT_TYPE).unwrap());
        assert_eq!(n.seen.len(), 1);
    }

    #[test]
    fn poll_candidates_prunes_when_candidate_leaves_list() {
        let conn = setup_db();
        turn(&conn, T0, T_STOP, json!({}));
        let c = candidate(&conn);
        let store = Arc::new(RecordingStore::default());
        let mut n = SessionIdleNotifier::new();
        let now = secs(T_STOP, 31);
        assert_eq!(poll_cands(&mut n, &conn, &store, &[c], now), 1);
        assert_eq!(n.seen.len(), 1);
        assert_eq!(poll_cands(&mut n, &conn, &store, &[], now), 0);
        assert_eq!(n.seen.len(), 0);
    }
}
