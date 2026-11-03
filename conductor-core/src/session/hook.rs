//! `conductor session hook`: apply one Claude Code hook event to an interactive
//! session's `agent_runs` row (RFC 021 §2).
//!
//! The state machine lives here as plain functions so it is testable against an
//! in-memory database. [`HookEvent`] is serializable and carries its own
//! `received_at` and captured environment, and every timestamp written comes
//! from it, never from the clock. That lets the [`super::spool`] replay an event
//! later and produce identical rows and the same dedup key.
//!
//! This module writes `agent_runs`, `feedback_requests` and `agent_run_events`
//! with raw SQL on purpose rather than through `AgentManager`: the hook is a
//! fast path (no `Conductor::open()`, no config) that must run the whole
//! transition inside one IMMEDIATE transaction on a bare `&Connection`. To keep
//! the two write paths from drifting, status values and event kinds come from
//! `agent::types` / `agent::status` and are not redefined here.

use std::path::Path;

use rusqlite::{named_params, params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::status::{AgentRunStatus, FeedbackStatus};
use crate::agent::types::{
    EVENT_KIND_PROMPT, EVENT_KIND_RESULT, EVENT_KIND_SYSTEM, EVENT_KIND_TOOL,
    EVENT_KIND_TOOL_ERROR, META_KEY_ERROR_TEXT,
};
use crate::error::Result;
use crate::new_id;

pub const RUNTIME_INTERACTIVE: &str = "interactive";

/// Summaries of the `system` event a SessionStart writes. The replay dedup
/// query matches on them, so the insert and the query must share them.
const SUMMARY_SESSION_STARTED: &str = "Session started";
const SUMMARY_SESSION_RESUMED: &str = "Session resumed";
/// Placeholder for the NOT NULL `agent_runs.prompt` column until the first prompt arrives.
pub(crate) const PLACEHOLDER_PROMPT: &str = "(interactive session)";

const MAX_PROMPT_CHARS: usize = 500;
const MAX_RESULT_CHARS: usize = 2000;
const MAX_FEEDBACK_PROMPT_CHARS: usize = 200;
const MAX_ERROR_CHARS: usize = 500;
const MAX_TOOL_SUMMARY_CHARS: usize = 200;

/// Hook events this module acts on. Everything else is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    SessionStart,
    UserPromptSubmit,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    SessionEnd,
}

impl HookKind {
    pub const ALL: [HookKind; 7] = [
        HookKind::SessionStart,
        HookKind::UserPromptSubmit,
        HookKind::PermissionRequest,
        HookKind::PostToolUse,
        HookKind::PostToolUseFailure,
        HookKind::Stop,
        HookKind::SessionEnd,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HookKind::SessionStart => "SessionStart",
            HookKind::UserPromptSubmit => "UserPromptSubmit",
            HookKind::PermissionRequest => "PermissionRequest",
            HookKind::PostToolUse => "PostToolUse",
            HookKind::PostToolUseFailure => "PostToolUseFailure",
            HookKind::Stop => "Stop",
            HookKind::SessionEnd => "SessionEnd",
        }
    }
}

impl std::str::FromStr for HookKind {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        Self::ALL.into_iter().find(|k| k.as_str() == s).ok_or(())
    }
}

/// Names of the hook events the state machine handles.
pub fn handled_event_names() -> impl Iterator<Item = &'static str> {
    HookKind::ALL.into_iter().map(HookKind::as_str)
}

/// Whether `name` is a hook event the state machine handles.
pub fn is_handled_event(name: &str) -> bool {
    name.parse::<HookKind>().is_ok()
}

/// Environment captured from the hook process.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookEnv {
    /// `CLAUDE_CODE_SESSION_ATTENDED == "1"` (interactive, not `claude -p`).
    pub attended: bool,
    /// `CLAUDE_PID`, else the hook's parent pid.
    pub claude_pid: Option<u32>,
    /// `TMUX_PANE`.
    pub tmux_pane: Option<String>,
}

/// The trimmed hook payload plus the context needed to apply it later.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookEvent {
    /// RFC 3339 arrival time of the hook.
    pub received_at: String,
    pub env: HookEnv,
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Command, file path, etc. precomputed from `tool_input`.
    #[serde(default)]
    pub tool_summary: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub background_tasks: Option<i64>,
}

/// What [`apply_hook_event`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Nothing was written; the reason is for debug output and tests.
    Ignored(&'static str),
    /// The event was applied to this run.
    Applied { run_id: String },
}

/// Truncate to at most `max` characters.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl HookEvent {
    /// Build an event from a parsed hook payload. Never fails on shape: fields
    /// that are missing or of an unexpected type become `None`. Long text is
    /// truncated here so spool lines stay small.
    pub fn from_payload(v: &Value, env: HookEnv, received_at: String) -> HookEvent {
        // `model` is a string in some payloads and `{id, display_name}` in others.
        let model = match v.get("model") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(m @ Value::Object(_)) => {
                str_field(m, "id").or_else(|| str_field(m, "display_name"))
            }
            _ => None,
        };

        let tool_summary = v.get("tool_input").and_then(|input| {
            [
                "command",
                "file_path",
                "path",
                "pattern",
                "url",
                "description",
            ]
            .iter()
            .find_map(|k| str_field(input, k))
            .map(|s| truncate_chars(&s, MAX_TOOL_SUMMARY_CHARS))
        });

        let duration_ms = v.get("duration_ms").and_then(Value::as_i64).or_else(|| {
            let resp = v.get("tool_response")?;
            resp.get("duration_ms")
                .or_else(|| resp.get("durationMs"))
                .and_then(Value::as_i64)
        });

        let background_tasks = match v.get("background_tasks") {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::Array(a)) => Some(a.len() as i64),
            _ => None,
        };

        let error = match v.get("error") {
            Some(Value::String(s)) if !s.is_empty() => Some(truncate_chars(s, MAX_ERROR_CHARS)),
            _ => None,
        };

        HookEvent {
            received_at,
            env,
            hook_event_name: str_field(v, "hook_event_name"),
            session_id: str_field(v, "session_id"),
            cwd: str_field(v, "cwd"),
            transcript_path: str_field(v, "transcript_path"),
            model,
            source: str_field(v, "source"),
            reason: str_field(v, "reason"),
            prompt: str_field(v, "prompt").map(|s| truncate_chars(&s, MAX_PROMPT_CHARS)),
            prompt_id: str_field(v, "prompt_id"),
            tool_name: str_field(v, "tool_name"),
            tool_summary,
            duration_ms,
            error,
            last_assistant_message: str_field(v, "last_assistant_message")
                .map(|s| truncate_chars(&s, MAX_RESULT_CHARS)),
            background_tasks,
        }
    }
}

/// Whether an event may already have been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyMode {
    /// First application: `agent_run_events` rows are always inserted.
    Fresh,
    /// The event may have been applied before (a drain that committed but
    /// crashed before deleting its file). `agent_run_events` inserts are
    /// skipped when an identical row exists. Everything else is already
    /// idempotent.
    Replay,
}

/// Apply one hook event in a single IMMEDIATE transaction.
///
/// IMMEDIATE takes the write lock up front, so a busy database fails at once
/// with `SQLITE_BUSY` instead of midway through on a lock upgrade.
pub fn apply_hook_event(conn: &Connection, ev: &HookEvent) -> Result<HookOutcome> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let outcome = apply_in_tx(&tx, ev, ApplyMode::Fresh)?;
    tx.commit()?;
    Ok(outcome)
}

/// The state machine, run inside a transaction the caller owns and commits.
pub(crate) fn apply_in_tx(tx: &Connection, ev: &HookEvent, mode: ApplyMode) -> Result<HookOutcome> {
    if !ev.env.attended {
        return Ok(HookOutcome::Ignored("not attended"));
    }
    let Some(kind) = ev
        .hook_event_name
        .as_deref()
        .and_then(|n| n.parse::<HookKind>().ok())
    else {
        return Ok(HookOutcome::Ignored("unhandled event"));
    };
    let session_id = match ev.session_id.as_deref() {
        Some(s) => s,
        None => return Ok(HookOutcome::Ignored("no session_id")),
    };

    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT id, status FROM agent_runs WHERE session_id = ?1 AND runtime = ?2 \
             ORDER BY started_at DESC LIMIT 1",
            params![session_id, RUNTIME_INTERACTIVE],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let existed = existing.is_some();
    let run_id = match existing {
        Some((id, status)) => {
            let closed = status == AgentRunStatus::Completed.as_str()
                || status == AgentRunStatus::Failed.as_str();
            if closed && !matches!(kind, HookKind::SessionStart | HookKind::UserPromptSubmit) {
                return Ok(HookOutcome::Ignored("session closed"));
            }
            id
        }
        None => {
            if kind == HookKind::SessionEnd {
                return Ok(HookOutcome::Ignored("unknown session"));
            }
            let Some(cwd) = ev.cwd.as_deref() else {
                return Ok(HookOutcome::Ignored("no cwd"));
            };
            let Some((worktree_id, repo_id)) = resolve_worktree(tx, cwd)? else {
                return Ok(HookOutcome::Ignored("not in an active worktree"));
            };
            insert_run(tx, ev, session_id, &worktree_id, &repo_id)?
        }
    };

    match kind {
        HookKind::SessionStart => on_session_start(tx, mode, ev, &run_id, existed)?,
        HookKind::UserPromptSubmit => on_user_prompt(tx, mode, ev, &run_id)?,
        HookKind::PermissionRequest => on_permission_request(tx, mode, ev, &run_id, session_id)?,
        HookKind::PostToolUse | HookKind::PostToolUseFailure => {
            on_post_tool_use(tx, mode, ev, &run_id, kind)?
        }
        HookKind::Stop => on_stop(tx, mode, ev, &run_id)?,
        HookKind::SessionEnd => on_session_end(tx, mode, ev, &run_id)?,
    }

    Ok(HookOutcome::Applied { run_id })
}

/// Longest active-worktree path that contains `cwd` (component-wise, so
/// `/ws/foo-bar` is not inside `/ws/foo`). Retries with the canonicalized `cwd`
/// when the raw path matches nothing (symlinked `/var` vs `/private/var`).
fn resolve_worktree(conn: &Connection, cwd: &str) -> Result<Option<(String, String)>> {
    let mut stmt =
        conn.prepare("SELECT id, repo_id, path FROM worktrees WHERE status = 'active'")?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let best = |cwd: &Path| {
        rows.iter()
            .filter(|(_, _, p)| cwd.starts_with(Path::new(p)))
            .max_by_key(|(_, _, p)| Path::new(p).components().count())
            .map(|(id, repo, _)| (id.clone(), repo.clone()))
    };

    if let Some(found) = best(Path::new(cwd)) {
        return Ok(Some(found));
    }
    Ok(std::fs::canonicalize(cwd).ok().and_then(|c| best(&c)))
}

fn insert_run(
    conn: &Connection,
    ev: &HookEvent,
    session_id: &str,
    worktree_id: &str,
    repo_id: &str,
) -> Result<String> {
    let id = new_id();
    let (pid, pid_started) = pid_columns(ev);
    conn.execute(
        "INSERT INTO agent_runs (id, worktree_id, repo_id, session_id, prompt, status, started_at, \
         model, runtime, transcript_path, tmux_pane, subprocess_pid, subprocess_started_at) \
         VALUES (:id, :wt, :repo, :sid, :prompt, :status, :now, :model, :runtime, :tp, :pane, :pid, :pid_started)",
        named_params! {
            ":id": id,
            ":wt": worktree_id,
            ":repo": repo_id,
            ":sid": session_id,
            ":prompt": PLACEHOLDER_PROMPT,
            ":status": AgentRunStatus::Idle.as_str(),
            ":now": ev.received_at,
            ":model": ev.model,
            ":runtime": RUNTIME_INTERACTIVE,
            ":tp": ev.transcript_path,
            ":pane": ev.env.tmux_pane,
            ":pid": pid,
            ":pid_started": pid_started,
        },
    )?;
    Ok(id)
}

/// `(subprocess_pid, subprocess_started_at)` for the reaper's PID-reuse guard.
///
/// When the process start time can't be read, `subprocess_started_at` is NULL so
/// the reaper falls back to the run's `started_at` instead of trusting a made-up
/// baseline. The failure is reported when `CONDUCTOR_SESSION_HOOK_DEBUG` is set.
fn pid_columns(ev: &HookEvent) -> (Option<i64>, Option<String>) {
    match ev.env.claude_pid {
        Some(pid) if pid > 0 => {
            let started = crate::process_utils::process_started_at_rfc3339(pid);
            if started.is_none() && std::env::var_os("CONDUCTOR_SESSION_HOOK_DEBUG").is_some() {
                eprintln!(
                    "conductor session hook: no start time for pid {pid} ({}); subprocess_started_at left NULL",
                    ev.hook_event_name.as_deref().unwrap_or("?")
                );
            }
            (Some(i64::from(pid)), started)
        }
        _ => (None, None),
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_event_with(
    mode: ApplyMode,
    conn: &Connection,
    run_id: &str,
    kind: &str,
    summary: &str,
    started_at: &str,
    ended_at: Option<&str>,
    metadata: Value,
) -> Result<()> {
    let sql = match mode {
        ApplyMode::Fresh => {
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, ended_at, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        }
        ApplyMode::Replay => {
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, ended_at, metadata) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 WHERE NOT EXISTS (\
               SELECT 1 FROM agent_run_events \
               WHERE run_id = ?2 AND kind = ?3 AND started_at = ?5 AND summary = ?4)"
        }
    };
    conn.execute(
        sql,
        params![
            new_id(),
            run_id,
            kind,
            summary,
            started_at,
            ended_at,
            metadata.to_string()
        ],
    )?;
    Ok(())
}

/// Move the run's pending feedback requests to `status`, recording `response`.
fn resolve_pending(
    conn: &Connection,
    run_id: &str,
    status: FeedbackStatus,
    response: Option<&str>,
    now: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE feedback_requests SET status = ?2, response = ?3, responded_at = ?4 \
         WHERE run_id = ?1 AND status = ?5",
        params![
            run_id,
            status.as_str(),
            response,
            now,
            FeedbackStatus::Pending.as_str()
        ],
    )?;
    Ok(())
}

pub(crate) fn dismiss_pending(conn: &Connection, run_id: &str, now: &str) -> Result<()> {
    resolve_pending(conn, run_id, FeedbackStatus::Dismissed, None, now)
}

/// Set `ended_at` on the run's open prompt span, preferring the one with
/// `prompt_id`, else the latest open one.
pub(crate) fn close_prompt_span(
    conn: &Connection,
    run_id: &str,
    prompt_id: Option<&str>,
    now: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE agent_run_events SET ended_at = ?3 WHERE id = (\
           SELECT id FROM agent_run_events WHERE run_id = ?1 AND kind = 'prompt' \
           AND ended_at IS NULL \
           ORDER BY COALESCE(json_extract(metadata, '$.prompt_id') = ?2, 0) DESC, \
           started_at DESC, id DESC LIMIT 1)",
        params![run_id, prompt_id, now],
    )?;
    Ok(())
}

fn on_session_start(
    conn: &Connection,
    mode: ApplyMode,
    ev: &HookEvent,
    run_id: &str,
    existed: bool,
) -> Result<()> {
    let (pid, pid_started) = pid_columns(ev);
    // `started_at` is deliberately untouched so a resume keeps the original start.
    conn.execute(
        "UPDATE agent_runs SET status = ?7, idle_since = NULL, ended_at = NULL, \
         subprocess_pid = ?2, subprocess_started_at = ?3, tmux_pane = ?4, \
         transcript_path = COALESCE(?5, transcript_path), model = COALESCE(?6, model) \
         WHERE id = ?1",
        params![
            run_id,
            pid,
            pid_started,
            ev.env.tmux_pane,
            ev.transcript_path,
            ev.model,
            AgentRunStatus::Idle.as_str()
        ],
    )?;
    // On replay the run already exists because this very event created it, so
    // `existed` would flip "Session started" to "Session resumed". Match either.
    if mode == ApplyMode::Replay {
        let seen: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_run_events WHERE run_id = ?1 AND kind = ?2 \
             AND started_at = ?3 AND summary IN (?4, ?5))",
            params![
                run_id,
                EVENT_KIND_SYSTEM,
                ev.received_at,
                SUMMARY_SESSION_STARTED,
                SUMMARY_SESSION_RESUMED
            ],
            |r| r.get(0),
        )?;
        if seen {
            return Ok(());
        }
    }
    let resumed = existed || ev.source.as_deref() == Some("resume");
    insert_event_with(
        mode,
        conn,
        run_id,
        EVENT_KIND_SYSTEM,
        if resumed {
            SUMMARY_SESSION_RESUMED
        } else {
            SUMMARY_SESSION_STARTED
        },
        &ev.received_at,
        None,
        json!({ "source": ev.source }),
    )
}

fn on_user_prompt(conn: &Connection, mode: ApplyMode, ev: &HookEvent, run_id: &str) -> Result<()> {
    let now = ev.received_at.as_str();
    dismiss_pending(conn, run_id, now)?;
    let prompt = ev.prompt.as_deref().unwrap_or("");
    conn.execute(
        "UPDATE agent_runs SET status = ?3, ended_at = NULL, idle_since = NULL, \
         prompt = CASE WHEN ?2 = '' THEN prompt ELSE ?2 END WHERE id = ?1",
        params![run_id, prompt, AgentRunStatus::Running.as_str()],
    )?;
    insert_event_with(
        mode,
        conn,
        run_id,
        EVENT_KIND_PROMPT,
        prompt,
        now,
        None,
        json!({ "prompt_id": ev.prompt_id }),
    )
}

fn on_permission_request(
    conn: &Connection,
    mode: ApplyMode,
    ev: &HookEvent,
    run_id: &str,
    session_id: &str,
) -> Result<()> {
    let now = ev.received_at.as_str();
    let tool = ev.tool_name.as_deref().unwrap_or("tool");
    let summary = ev.tool_summary.as_deref().unwrap_or("");
    let prompt = truncate_chars(
        format!("Permission: {tool} {summary}").trim_end(),
        MAX_FEEDBACK_PROMPT_CHARS,
    );
    let dedup_key = format!(
        "{session_id}|{}|{tool}|{now}",
        ev.prompt_id.as_deref().unwrap_or("")
    );

    conn.execute(
        "UPDATE agent_runs SET status = ?2 WHERE id = ?1",
        params![run_id, AgentRunStatus::WaitingForFeedback.as_str()],
    )?;
    let inserted = conn.execute(
        "INSERT INTO feedback_requests (id, run_id, prompt, status, created_at, feedback_type, dedup_key) \
         VALUES (?1, ?2, ?3, 'pending', ?4, 'text', ?5) \
         ON CONFLICT(dedup_key) WHERE dedup_key IS NOT NULL DO NOTHING",
        params![new_id(), run_id, prompt, now, dedup_key],
    )?;
    // A replayed event leaves its original rows alone.
    if inserted > 0 {
        insert_event_with(
            mode,
            conn,
            run_id,
            EVENT_KIND_SYSTEM,
            &format!("Waiting: {prompt}"),
            now,
            None,
            json!({ "prompt_id": ev.prompt_id }),
        )?;
    }
    Ok(())
}

fn on_post_tool_use(
    conn: &Connection,
    mode: ApplyMode,
    ev: &HookEvent,
    run_id: &str,
    kind: HookKind,
) -> Result<()> {
    let now = ev.received_at.as_str();
    resolve_pending(
        conn,
        run_id,
        FeedbackStatus::Responded,
        Some("answered in terminal"),
        now,
    )?;
    conn.execute(
        "UPDATE agent_runs SET status = ?2 WHERE id = ?1 AND status = ?3",
        params![
            run_id,
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::WaitingForFeedback.as_str()
        ],
    )?;

    let tool = ev.tool_name.as_deref().unwrap_or("tool");
    let started_at = match ev.duration_ms.and_then(|ms| {
        chrono::DateTime::parse_from_rfc3339(now)
            .ok()
            .map(|t| (t, ms))
    }) {
        Some((t, ms)) => (t - chrono::Duration::milliseconds(ms.max(0))).to_rfc3339(),
        None => now.to_string(),
    };
    let mut meta = json!({ "prompt_id": ev.prompt_id, "duration_ms": ev.duration_ms });
    let (kind, summary) = if kind == HookKind::PostToolUseFailure {
        if let Some(err) = &ev.error {
            meta[META_KEY_ERROR_TEXT] = json!(err);
        }
        (EVENT_KIND_TOOL_ERROR, tool.to_string())
    } else {
        (
            EVENT_KIND_TOOL,
            format!("{tool} {}", ev.tool_summary.as_deref().unwrap_or(""))
                .trim_end()
                .to_string(),
        )
    };
    insert_event_with(
        mode,
        conn,
        run_id,
        kind,
        &summary,
        &started_at,
        Some(now),
        meta,
    )
}

fn on_stop(conn: &Connection, mode: ApplyMode, ev: &HookEvent, run_id: &str) -> Result<()> {
    let now = ev.received_at.as_str();
    dismiss_pending(conn, run_id, now)?;
    conn.execute(
        "UPDATE agent_runs SET status = ?4, idle_since = ?2, \
         result_text = COALESCE(?3, result_text) WHERE id = ?1",
        params![
            run_id,
            now,
            ev.last_assistant_message,
            AgentRunStatus::Idle.as_str()
        ],
    )?;
    close_prompt_span(conn, run_id, ev.prompt_id.as_deref(), now)?;
    insert_event_with(
        mode,
        conn,
        run_id,
        EVENT_KIND_RESULT,
        ev.last_assistant_message.as_deref().unwrap_or(""),
        now,
        Some(now),
        json!({ "prompt_id": ev.prompt_id, "background_tasks": ev.background_tasks }),
    )
}

/// What `on_stop` recorded about the turn that began the idle period `idle_since`.
///
/// This is the read side of `on_stop`: the `result` event it inserts has
/// `started_at = idle_since` and carries `background_tasks`, and the prompt span it closes
/// has `ended_at = idle_since`. Keep it next to `on_stop` so a change to how those events
/// are written updates the reader in the same place.
///
/// Returns `(background_tasks, prompt_started_at)`; each is `None` when not recorded.
pub fn stop_turn_details(
    conn: &Connection,
    run_id: &str,
    idle_since: &str,
) -> Result<(Option<i64>, Option<String>)> {
    let background_tasks = conn
        .query_row(
            "SELECT json_extract(metadata, '$.background_tasks') FROM agent_run_events \
             WHERE run_id = ?1 AND kind = ?2 AND started_at = ?3 \
             ORDER BY id DESC LIMIT 1",
            params![run_id, EVENT_KIND_RESULT, idle_since],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    let prompt_started = conn
        .query_row(
            "SELECT started_at FROM agent_run_events \
             WHERE run_id = ?1 AND kind = ?2 AND ended_at = ?3 \
             ORDER BY id DESC LIMIT 1",
            params![run_id, EVENT_KIND_PROMPT, idle_since],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok((background_tasks, prompt_started))
}

fn on_session_end(conn: &Connection, mode: ApplyMode, ev: &HookEvent, run_id: &str) -> Result<()> {
    let now = ev.received_at.as_str();
    dismiss_pending(conn, run_id, now)?;
    conn.execute(
        "UPDATE agent_runs SET status = ?3, ended_at = ?2, idle_since = NULL WHERE id = ?1",
        params![run_id, now, AgentRunStatus::Completed.as_str()],
    )?;
    close_prompt_span(conn, run_id, None, now)?;
    insert_event_with(
        mode,
        conn,
        run_id,
        EVENT_KIND_SYSTEM,
        &format!(
            "Session ended ({})",
            ev.reason.as_deref().unwrap_or("unknown")
        ),
        now,
        None,
        json!({ "reason": ev.reason }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{insert_test_repo, insert_test_worktree, setup_db};

    const T0: &str = "2026-11-02T10:00:00+00:00";
    const T1: &str = "2026-11-02T10:00:05+00:00";
    const T2: &str = "2026-11-02T10:00:10+00:00";

    fn env() -> HookEnv {
        HookEnv {
            attended: true,
            claude_pid: Some(std::process::id()),
            tmux_pane: Some("%3".into()),
        }
    }

    fn ev(name: &str, extra: Value, at: &str) -> HookEvent {
        let mut v = json!({
            "hook_event_name": name,
            "session_id": "s1",
            "cwd": "/tmp/ws/feat-test/src",
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        HookEvent::from_payload(&v, env(), at.to_string())
    }

    fn apply(conn: &Connection, e: &HookEvent) -> HookOutcome {
        apply_hook_event(conn, e).unwrap()
    }

    fn counts(conn: &Connection) -> (i64, i64, i64) {
        let c = |t: &str| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };
        (
            c("agent_runs"),
            c("feedback_requests"),
            c("agent_run_events"),
        )
    }

    fn run_status(conn: &Connection) -> String {
        conn.query_row(
            "SELECT status FROM agent_runs WHERE session_id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn run_col(conn: &Connection, col: &str) -> Option<String> {
        conn.query_row(
            &format!("SELECT CAST({col} AS TEXT) FROM agent_runs WHERE session_id='s1'"),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Events of a kind, oldest first: (summary, started_at, ended_at, metadata).
    fn events(conn: &Connection, kind: &str) -> Vec<(String, String, Option<String>, Value)> {
        let mut stmt = conn
            .prepare(
                "SELECT summary, started_at, ended_at, metadata FROM agent_run_events \
                 WHERE kind = ?1 ORDER BY started_at, id",
            )
            .unwrap();
        stmt.query_map([kind], |r| {
            let m: String = r.get(3)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                serde_json::from_str(&m).unwrap(),
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    fn feedback(conn: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut stmt = conn
            .prepare(
                "SELECT prompt, status, response FROM feedback_requests ORDER BY created_at, id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn start(conn: &Connection) {
        apply(
            conn,
            &ev(
                "SessionStart",
                json!({"source": "startup", "model": "claude-opus", "transcript_path": "/t/s1.jsonl"}),
                T0,
            ),
        );
    }

    #[test]
    fn session_start_creates_idle_row() {
        let conn = setup_db();
        start(&conn);
        assert_eq!(run_status(&conn), "idle");
        assert_eq!(run_col(&conn, "idle_since"), None);
        assert_eq!(run_col(&conn, "runtime").as_deref(), Some("interactive"));
        assert_eq!(run_col(&conn, "worktree_id").as_deref(), Some("w1"));
        assert_eq!(run_col(&conn, "repo_id").as_deref(), Some("r1"));
        assert_eq!(run_col(&conn, "model").as_deref(), Some("claude-opus"));
        assert_eq!(run_col(&conn, "tmux_pane").as_deref(), Some("%3"));
        assert_eq!(
            run_col(&conn, "transcript_path").as_deref(),
            Some("/t/s1.jsonl")
        );
        assert_eq!(
            run_col(&conn, "subprocess_pid"),
            Some(std::process::id().to_string())
        );
        // The process start time is only readable on macOS (process_utils);
        // elsewhere the hook stores NULL and the reaper falls back to started_at.
        assert_eq!(
            run_col(&conn, "subprocess_started_at").is_some(),
            cfg!(target_os = "macos")
        );
        let evs = events(&conn, "system");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "Session started");
    }

    #[test]
    fn user_prompt_runs_and_opens_span() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev(
                "UserPromptSubmit",
                json!({"prompt": "fix it", "prompt_id": "p1"}),
                T1,
            ),
        );
        assert_eq!(run_status(&conn), "running");
        assert_eq!(run_col(&conn, "prompt").as_deref(), Some("fix it"));
        assert_eq!(run_col(&conn, "idle_since"), None);
        let evs = events(&conn, "prompt");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "fix it");
        assert_eq!(evs[0].2, None);
        assert_eq!(evs[0].3["prompt_id"], "p1");
    }

    #[test]
    fn permission_request_waits_with_feedback() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev(
                "PermissionRequest",
                json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf x"}, "prompt_id": "p1"}),
                T1,
            ),
        );
        assert_eq!(run_status(&conn), "waiting_for_feedback");
        let fb = feedback(&conn);
        assert_eq!(
            fb,
            vec![("Permission: Bash rm -rf x".into(), "pending".into(), None)]
        );
        let evs = events(&conn, "system");
        assert_eq!(evs.last().unwrap().0, "Waiting: Permission: Bash rm -rf x");
    }

    #[test]
    fn post_tool_use_answers_and_resumes_running() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev(
                "PermissionRequest",
                json!({"tool_name": "Edit", "tool_input": {"file_path": "/a.rs"}}),
                T1,
            ),
        );
        apply(
            &conn,
            &ev(
                "PostToolUse",
                json!({"tool_name": "Edit", "tool_input": {"file_path": "/a.rs"}, "duration_ms": 2000}),
                T2,
            ),
        );
        assert_eq!(run_status(&conn), "running");
        let fb = feedback(&conn);
        assert_eq!(fb[0].1, "responded");
        assert_eq!(fb[0].2.as_deref(), Some("answered in terminal"));
        let tools = events(&conn, "tool");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "Edit /a.rs");
        assert_eq!(tools[0].1, "2026-11-02T10:00:08+00:00");
        assert_eq!(tools[0].2.as_deref(), Some(T2));
        assert_eq!(tools[0].3["duration_ms"], 2000);
    }

    #[test]
    fn post_tool_use_failure_records_tool_error() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev(
                "PostToolUseFailure",
                json!({"tool_name": "Bash", "error": "boom"}),
                T1,
            ),
        );
        let evs = events(&conn, EVENT_KIND_TOOL_ERROR);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "Bash");
        assert_eq!(evs[0].3[META_KEY_ERROR_TEXT], "boom");
    }

    #[test]
    fn post_tool_use_while_idle_stays_idle() {
        let conn = setup_db();
        start(&conn);
        apply(&conn, &ev("PostToolUse", json!({"tool_name": "Read"}), T1));
        assert_eq!(run_status(&conn), "idle");
        assert_eq!(events(&conn, "tool").len(), 1);
    }

    #[test]
    fn stop_goes_idle_and_closes_span() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev(
                "UserPromptSubmit",
                json!({"prompt": "go", "prompt_id": "p1"}),
                T1,
            ),
        );
        apply(
            &conn,
            &ev(
                "Stop",
                json!({"last_assistant_message": "done", "background_tasks": [1, 2], "prompt_id": "p1"}),
                T2,
            ),
        );
        assert_eq!(run_status(&conn), "idle");
        assert_eq!(run_col(&conn, "idle_since").as_deref(), Some(T2));
        assert_eq!(run_col(&conn, "result_text").as_deref(), Some("done"));
        assert_eq!(events(&conn, "prompt")[0].2.as_deref(), Some(T2));
        let res = events(&conn, "result");
        assert_eq!(res[0].0, "done");
        assert_eq!(res[0].3["background_tasks"], 2);
    }

    #[test]
    fn stop_turn_details_reads_what_on_stop_wrote() {
        let conn = setup_db();
        start(&conn);
        let run_id = run_col(&conn, "id").unwrap();
        // Nothing recorded yet: SessionStart alone writes no result or prompt span.
        assert_eq!(stop_turn_details(&conn, &run_id, T2).unwrap(), (None, None));
        apply(
            &conn,
            &ev(
                "UserPromptSubmit",
                json!({"prompt": "go", "prompt_id": "p1"}),
                T1,
            ),
        );
        apply(
            &conn,
            &ev(
                "Stop",
                json!({"last_assistant_message": "done", "background_tasks": [1, 2], "prompt_id": "p1"}),
                T2,
            ),
        );
        assert_eq!(
            stop_turn_details(&conn, &run_id, T2).unwrap(),
            (Some(2), Some(T1.to_string()))
        );
        // A later turn's idle_since must not match the earlier turn's events.
        let t3 = "2026-11-02T10:00:20+00:00";
        assert_eq!(stop_turn_details(&conn, &run_id, t3).unwrap(), (None, None));
        // Stop without background_tasks records no count but still yields the span start.
        let t4 = "2026-11-02T10:00:30+00:00";
        let t5 = "2026-11-02T10:00:40+00:00";
        apply(
            &conn,
            &ev(
                "UserPromptSubmit",
                json!({"prompt": "again", "prompt_id": "p2"}),
                t4,
            ),
        );
        apply(
            &conn,
            &ev(
                "Stop",
                json!({"last_assistant_message": "ok", "prompt_id": "p2"}),
                t5,
            ),
        );
        assert_eq!(
            stop_turn_details(&conn, &run_id, t5).unwrap(),
            (None, Some(t4.to_string()))
        );
    }

    #[test]
    fn session_end_completes_and_dismisses() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev("PermissionRequest", json!({"tool_name": "Bash"}), T1),
        );
        apply(&conn, &ev("SessionEnd", json!({"reason": "logout"}), T2));
        assert_eq!(run_status(&conn), "completed");
        assert_eq!(run_col(&conn, "ended_at").as_deref(), Some(T2));
        assert_eq!(run_col(&conn, "idle_since"), None);
        assert_eq!(feedback(&conn)[0].1, "dismissed");
        assert_eq!(
            events(&conn, "system").last().unwrap().0,
            "Session ended (logout)"
        );
    }

    #[test]
    fn resume_reopens_same_row_and_keeps_started_at() {
        let conn = setup_db();
        start(&conn);
        let id = run_col(&conn, "id");
        apply(&conn, &ev("SessionEnd", json!({"reason": "other"}), T1));
        apply(&conn, &ev("SessionStart", json!({"source": "resume"}), T2));
        assert_eq!(run_col(&conn, "id"), id);
        assert_eq!(run_col(&conn, "started_at").as_deref(), Some(T0));
        assert_eq!(run_status(&conn), "idle");
        assert_eq!(run_col(&conn, "ended_at"), None);
        assert_eq!(run_col(&conn, "model").as_deref(), Some("claude-opus"));
        assert_eq!(events(&conn, "system").last().unwrap().0, "Session resumed");
        assert_eq!(counts(&conn).0, 1);
    }

    #[test]
    fn permission_approved_then_stop() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev(
                "PermissionRequest",
                json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
                T1,
            ),
        );
        apply(&conn, &ev("PostToolUse", json!({"tool_name": "Bash"}), T1));
        apply(
            &conn,
            &ev("Stop", json!({"last_assistant_message": "ok"}), T2),
        );
        assert_eq!(feedback(&conn)[0].1, "responded");
        assert_eq!(run_status(&conn), "idle");
    }

    #[test]
    fn permission_denied_then_next_prompt_dismisses() {
        let conn = setup_db();
        start(&conn);
        apply(
            &conn,
            &ev("PermissionRequest", json!({"tool_name": "Bash"}), T1),
        );
        apply(
            &conn,
            &ev("UserPromptSubmit", json!({"prompt": "never mind"}), T2),
        );
        assert_eq!(feedback(&conn)[0].1, "dismissed");
        assert_eq!(run_status(&conn), "running");
    }

    #[test]
    fn replayed_permission_request_is_deduped() {
        let conn = setup_db();
        start(&conn);
        let e = ev(
            "PermissionRequest",
            json!({"tool_name": "Bash", "prompt_id": "p1"}),
            T1,
        );
        apply(&conn, &e);
        let before = counts(&conn);
        assert!(matches!(apply(&conn, &e), HookOutcome::Applied { .. }));
        assert_eq!(counts(&conn), before);
        assert_eq!(feedback(&conn).len(), 1);
    }

    #[test]
    fn longest_active_worktree_wins_and_siblings_do_not_match() {
        let conn = setup_db();
        insert_test_worktree(&conn, "w2", "r1", "nested", "/tmp/ws/feat-test/nested");
        insert_test_worktree(&conn, "w3", "r1", "sib", "/tmp/ws/feat");
        let mut e = ev("SessionStart", json!({}), T0);
        e.cwd = Some("/tmp/ws/feat-test/nested/deep".into());
        apply(&conn, &e);
        assert_eq!(run_col(&conn, "worktree_id").as_deref(), Some("w2"));

        // `/tmp/ws/feat-test-other` is only a string-prefix match of `feat-test`.
        let mut e = ev("SessionStart", json!({}), T0);
        e.session_id = Some("s2".into());
        e.cwd = Some("/tmp/ws/feat-test-other".into());
        assert_eq!(
            apply(&conn, &e),
            HookOutcome::Ignored("not in an active worktree")
        );
    }

    #[test]
    fn noops_write_nothing() {
        let conn = setup_db();
        insert_test_repo(&conn, "r2", "other", "/tmp/main-checkout");
        conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w1'", [])
            .unwrap();
        insert_test_worktree(&conn, "w9", "r2", "live", "/tmp/ws/live");
        let before = counts(&conn);

        // merged worktree
        assert!(matches!(
            apply(&conn, &ev("SessionStart", json!({}), T0)),
            HookOutcome::Ignored(_)
        ));
        // main checkout
        let mut e = ev("SessionStart", json!({}), T0);
        e.cwd = Some("/tmp/main-checkout".into());
        assert!(matches!(apply(&conn, &e), HookOutcome::Ignored(_)));
        // not attended
        let mut e = ev("SessionStart", json!({}), T0);
        e.cwd = Some("/tmp/ws/live".into());
        e.env.attended = false;
        assert_eq!(apply(&conn, &e), HookOutcome::Ignored("not attended"));
        // unknown SessionEnd
        let mut e = ev("SessionEnd", json!({}), T0);
        e.cwd = Some("/tmp/ws/live".into());
        assert_eq!(apply(&conn, &e), HookOutcome::Ignored("unknown session"));
        // ignored events
        for name in ["PreToolUse", "Notification", "SubagentStop", "Bogus"] {
            let mut e = ev(name, json!({}), T0);
            e.cwd = Some("/tmp/ws/live".into());
            assert_eq!(apply(&conn, &e), HookOutcome::Ignored("unhandled event"));
        }
        assert_eq!(counts(&conn), before);
    }

    #[test]
    fn late_events_do_not_resurrect_a_closed_session() {
        let conn = setup_db();
        start(&conn);
        apply(&conn, &ev("SessionEnd", json!({}), T1));
        assert_eq!(
            apply(&conn, &ev("Stop", json!({}), T2)),
            HookOutcome::Ignored("session closed")
        );
        assert_eq!(run_status(&conn), "completed");
    }

    #[test]
    fn from_payload_truncates_and_parses_leniently() {
        let long = "x".repeat(5000);
        let v = json!({
            "hook_event_name": "Stop",
            "prompt": long,
            "last_assistant_message": long,
            "error": long,
            "tool_input": {"command": long},
            "model": {"id": "m-1", "display_name": "M"},
            "background_tasks": 3,
        });
        let e = HookEvent::from_payload(&v, env(), T0.into());
        assert_eq!(e.prompt.unwrap().chars().count(), 500);
        assert_eq!(e.last_assistant_message.unwrap().chars().count(), 2000);
        assert_eq!(e.error.unwrap().chars().count(), 500);
        assert_eq!(e.tool_summary.unwrap().chars().count(), 200);
        assert_eq!(e.model.as_deref(), Some("m-1"));
        assert_eq!(e.background_tasks, Some(3));

        let e =
            HookEvent::from_payload(&json!({"background_tasks": [1, 2, 3, 4]}), env(), T0.into());
        assert_eq!(e.background_tasks, Some(4));
    }

    #[test]
    fn feedback_prompt_is_capped_at_200_chars() {
        let conn = setup_db();
        start(&conn);
        let long = "y".repeat(1000);
        apply(
            &conn,
            &ev(
                "PermissionRequest",
                json!({"tool_name": "Bash", "tool_input": {"command": long}}),
                T1,
            ),
        );
        assert_eq!(feedback(&conn)[0].0.chars().count(), 200);
    }

    #[test]
    fn event_roundtrips_through_serde() {
        let e = ev("Stop", json!({"last_assistant_message": "hi"}), T0);
        let back: HookEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.last_assistant_message.as_deref(), Some("hi"));
        assert_eq!(back.received_at, T0);
    }

    // ── HookKind ────────────────────────────────────────────────────────────

    #[test]
    fn hook_kind_all_covers_every_variant_and_round_trips() {
        // Exhaustive match: adding a variant breaks compilation here, forcing
        // an update to `ALL` (checked below) and this test.
        fn index(k: HookKind) -> usize {
            match k {
                HookKind::SessionStart => 0,
                HookKind::UserPromptSubmit => 1,
                HookKind::PermissionRequest => 2,
                HookKind::PostToolUse => 3,
                HookKind::PostToolUseFailure => 4,
                HookKind::Stop => 5,
                HookKind::SessionEnd => 6,
            }
        }
        let mut seen = [false; 7];
        for k in HookKind::ALL {
            seen[index(k)] = true;
            assert_eq!(k.as_str().parse::<HookKind>(), Ok(k));
        }
        assert!(seen.iter().all(|s| *s), "ALL is missing a HookKind variant");
        let names: Vec<_> = handled_event_names().collect();
        assert_eq!(names.len(), HookKind::ALL.len());
        for n in names {
            assert!(is_handled_event(n));
        }
    }

    #[test]
    fn hook_kind_rejects_unknown_names() {
        for bad in [
            "PreToolUse",
            "Notification",
            "SubagentStop",
            "",
            "stop",
            "SESSIONSTART",
        ] {
            assert_eq!(bad.parse::<HookKind>(), Err(()), "{bad:?}");
            assert!(!is_handled_event(bad), "{bad:?}");
        }
    }

    // ── close_prompt_span ───────────────────────────────────────────────────

    fn open_spans(conn: &Connection) -> Vec<(String, bool)> {
        events(conn, "prompt")
            .into_iter()
            .map(|(_, _, ended, m)| {
                (
                    m["prompt_id"].as_str().unwrap().to_string(),
                    ended.is_some(),
                )
            })
            .collect()
    }

    fn insert_event(
        conn: &Connection,
        run_id: &str,
        kind: &str,
        summary: &str,
        started_at: &str,
        ended_at: Option<&str>,
        metadata: Value,
    ) -> Result<()> {
        insert_event_with(
            ApplyMode::Fresh,
            conn,
            run_id,
            kind,
            summary,
            started_at,
            ended_at,
            metadata,
        )
    }

    fn two_open_spans(conn: &Connection) {
        start(conn);
        for (id, at) in [("p1", T1), ("p2", T2)] {
            insert_event(
                conn,
                &run_col(conn, "id").unwrap(),
                EVENT_KIND_PROMPT,
                id,
                at,
                None,
                json!({"prompt_id": id}),
            )
            .unwrap();
        }
    }

    #[test]
    fn close_prompt_span_without_prompt_id_closes_latest_open() {
        let conn = setup_db();
        two_open_spans(&conn);
        let run_id = run_col(&conn, "id").unwrap();
        close_prompt_span(&conn, &run_id, None, T2).unwrap();
        assert_eq!(
            open_spans(&conn),
            vec![("p1".into(), false), ("p2".into(), true)]
        );
    }

    #[test]
    fn close_prompt_span_prefers_matching_older_span() {
        let conn = setup_db();
        two_open_spans(&conn);
        let run_id = run_col(&conn, "id").unwrap();
        close_prompt_span(&conn, &run_id, Some("p1"), T2).unwrap();
        assert_eq!(
            open_spans(&conn),
            vec![("p1".into(), true), ("p2".into(), false)]
        );
    }

    #[test]
    fn close_prompt_span_unmatched_prompt_id_falls_back_to_latest() {
        let conn = setup_db();
        two_open_spans(&conn);
        let run_id = run_col(&conn, "id").unwrap();
        close_prompt_span(&conn, &run_id, Some("nope"), T2).unwrap();
        assert_eq!(
            open_spans(&conn),
            vec![("p1".into(), false), ("p2".into(), true)]
        );
    }

    // ── pid_columns ─────────────────────────────────────────────────────────

    #[test]
    fn pid_columns_leaves_start_time_null_when_unreadable() {
        // A PID far above any pid_max: the process cannot exist.
        let mut e = ev("SessionStart", json!({}), T0);
        e.env.claude_pid = Some(4_000_000_000);
        let (pid, started) = pid_columns(&e);
        assert_eq!(pid, Some(4_000_000_000));
        assert_eq!(started, None);
    }

    #[test]
    fn pid_columns_none_without_pid() {
        let mut e = ev("SessionStart", json!({}), T0);
        e.env.claude_pid = None;
        assert_eq!(pid_columns(&e), (None, None));
    }

    #[test]
    fn session_start_with_dead_pid_stores_null_start_time() {
        let conn = setup_db();
        let mut e = ev("SessionStart", json!({"source": "startup"}), T0);
        e.env.claude_pid = Some(4_000_000_000);
        apply(&conn, &e);
        assert_eq!(run_col(&conn, "subprocess_started_at"), None);
    }
}
