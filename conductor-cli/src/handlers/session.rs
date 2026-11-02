//! SPIKE (RFC 021, Spike C): `conductor session hook`.
//!
//! Reads one Claude Code hook payload on stdin and mirrors the interactive
//! session's state into an `agent_runs` row with `runtime = 'interactive'`,
//! keyed by `session_id`. Not production code: no tests, minimal error
//! handling, and it never fails the hook (Claude Code would surface a
//! non-zero exit to the developer).
//!
//! Idle (a finished turn) has no status of its own. `CONDUCTOR_SPIKE_IDLE`
//! picks how to represent it so the TUI and web can be compared:
//! `completed` (default), `feedback` (a pending feedback request) or
//! `running`.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use chrono::Utc;
use rusqlite::{named_params, Connection, OptionalExtension};
use serde_json::Value;

use conductor_core::agent::AgentManager;

const RUNTIME: &str = "interactive";

pub fn handle_session_hook(conn: &Connection, started: Instant) -> Result<()> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let payload: Value = serde_json::from_str(&raw)?;
    let event = payload["hook_event_name"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let outcome = apply(conn, &payload, &event);
    log_line(&payload, &event, started, &outcome);
    if let Err(e) = outcome {
        eprintln!("conductor session hook: {e}");
    }
    Ok(())
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn parent_pid() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: getppid has no preconditions.
        unsafe { libc::getppid() as i64 }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn idle_mode() -> String {
    std::env::var("CONDUCTOR_SPIKE_IDLE").unwrap_or_else(|_| "completed".into())
}

fn apply(conn: &Connection, p: &Value, event: &str) -> Result<String> {
    let session_id = str_field(p, "session_id").ok_or_else(|| anyhow::anyhow!("no session_id"))?;
    let now = Utc::now().to_rfc3339();
    let pid = parent_pid();

    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT id, status FROM agent_runs WHERE session_id = :sid AND runtime = :rt \
             ORDER BY started_at DESC LIMIT 1",
            named_params! { ":sid": session_id, ":rt": RUNTIME },
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    // SessionEnd for a session we never saw: nothing to do.
    if existing.is_none() && event == "SessionEnd" {
        return Ok("ignored: unknown session".into());
    }

    let run_id = match &existing {
        Some((id, _)) => id.clone(),
        None => create_run(conn, p, session_id, &now, pid)?,
    };

    match event {
        "SessionStart" => {
            // New session, or `--resume`/`--continue` of one we tracked.
            // `SPIKE_RESET_STARTED_AT=1` also resets started_at so the
            // reaper's PID-recycle guard (process start vs started_at, 60 s)
            // accepts the new claude process.
            let reset = std::env::var("SPIKE_RESET_STARTED_AT").is_ok();
            conn.execute(
                "UPDATE agent_runs SET subprocess_pid = :pid, \
                 started_at = CASE WHEN :reset THEN :now ELSE started_at END \
                 WHERE id = :id",
                named_params! { ":pid": pid, ":reset": reset, ":now": now, ":id": run_id },
            )?;
            set_idle(conn, &run_id, None, &now)?;
        }
        "UserPromptSubmit" => {
            resolve_pending(conn, &run_id, "dismissed", None, &now)?;
            let prompt: String = str_field(p, "prompt")
                .unwrap_or("")
                .chars()
                .take(500)
                .collect();
            conn.execute(
                "UPDATE agent_runs SET status = 'running', ended_at = NULL, \
                 prompt = :prompt, subprocess_pid = :pid WHERE id = :id",
                named_params! { ":prompt": prompt, ":pid": pid, ":id": run_id },
            )?;
        }
        "PermissionRequest" => {
            let tool = str_field(p, "tool_name").unwrap_or("?");
            let input = p.get("tool_input").map(summarize_input).unwrap_or_default();
            let prompt = format!("Permission: {tool} {input}");
            // A second request in the same turn (parallel tool calls) gets
            // its own row; the run stays waiting until all are cleared.
            AgentManager::new(conn).request_feedback(&run_id, &prompt, None)?;
        }
        "PostToolUse" | "PostToolUseFailure" => {
            // The prompt (if any) was answered. Cheap no-op when not waiting.
            resolve_pending(
                conn,
                &run_id,
                "responded",
                Some("answered in terminal"),
                &now,
            )?;
            conn.execute(
                "UPDATE agent_runs SET status = 'running' \
                 WHERE id = :id AND status = 'waiting_for_feedback'",
                named_params! { ":id": run_id },
            )?;
        }
        "Stop" => {
            // A denied permission never produces PostToolUse; clear it here.
            resolve_pending(conn, &run_id, "dismissed", None, &now)?;
            let msg = str_field(p, "last_assistant_message");
            set_idle(conn, &run_id, msg, &now)?;
        }
        "SessionEnd" => {
            resolve_pending(conn, &run_id, "dismissed", None, &now)?;
            let reason = str_field(p, "reason").unwrap_or("");
            conn.execute(
                "UPDATE agent_runs SET status = 'completed', ended_at = :now, \
                 result_text = COALESCE(result_text, '') || :suffix WHERE id = :id",
                named_params! {
                    ":now": now,
                    ":suffix": format!("\n[session ended: {reason}]"),
                    ":id": run_id,
                },
            )?;
        }
        _ => return Ok(format!("logged only ({run_id})")),
    }
    Ok(run_id)
}

fn create_run(
    conn: &Connection,
    p: &Value,
    session_id: &str,
    now: &str,
    pid: i64,
) -> Result<String> {
    let cwd = str_field(p, "cwd").unwrap_or("");
    let (worktree_id, repo_id) = resolve_worktree(conn, cwd)?;
    let id = conductor_core::new_id();
    conn.execute(
        "INSERT INTO agent_runs (id, worktree_id, repo_id, session_id, prompt, status, \
         started_at, model, subprocess_pid, runtime) \
         VALUES (:id, :wt, :repo, :sid, '(interactive session)', 'running', :now, :model, :pid, :rt)",
        named_params! {
            ":id": id,
            ":wt": worktree_id,
            ":repo": repo_id,
            ":sid": session_id,
            ":now": now,
            ":model": str_field(p, "model"),
            ":pid": pid,
            ":rt": RUNTIME,
        },
    )?;
    Ok(id)
}

/// Longest active worktree path that contains `cwd`; else the repo whose
/// checkout contains it (a repo-scoped run).
fn resolve_worktree(conn: &Connection, cwd: &str) -> Result<(Option<String>, Option<String>)> {
    let cwd = Path::new(cwd);
    let mut best: Option<(usize, String, String)> = None;
    let mut stmt =
        conn.prepare("SELECT id, repo_id, path FROM worktrees WHERE status = 'active'")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, repo, path) = row?;
        if cwd.starts_with(&path) && best.as_ref().is_none_or(|b| path.len() > b.0) {
            best = Some((path.len(), id, repo));
        }
    }
    if let Some((_, id, repo)) = best {
        return Ok((Some(id), Some(repo)));
    }
    let mut stmt = conn.prepare("SELECT id, local_path FROM repos")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, path) = row?;
        if cwd.starts_with(&path) {
            return Ok((None, Some(id)));
        }
    }
    Ok((None, None))
}

fn set_idle(conn: &Connection, run_id: &str, msg: Option<&str>, now: &str) -> Result<()> {
    let msg: Option<String> = msg.map(|m| m.chars().take(2000).collect());
    match idle_mode().as_str() {
        "feedback" => {
            conn.execute(
                "UPDATE agent_runs SET result_text = COALESCE(:msg, result_text) WHERE id = :id",
                named_params! { ":msg": msg, ":id": run_id },
            )?;
            let preview: String = msg.as_deref().unwrap_or("").chars().take(200).collect();
            AgentManager::new(conn).request_feedback(run_id, &format!("Idle: {preview}"), None)?;
        }
        "running" => {
            conn.execute(
                "UPDATE agent_runs SET status = 'running', ended_at = NULL, \
                 result_text = COALESCE(:msg, result_text) WHERE id = :id",
                named_params! { ":msg": msg, ":id": run_id },
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE agent_runs SET status = 'completed', ended_at = :now, \
                 result_text = COALESCE(:msg, result_text) WHERE id = :id",
                named_params! { ":now": now, ":msg": msg, ":id": run_id },
            )?;
        }
    }
    Ok(())
}

fn resolve_pending(
    conn: &Connection,
    run_id: &str,
    status: &str,
    response: Option<&str>,
    now: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE feedback_requests SET status = :st, response = :resp, responded_at = :now \
         WHERE run_id = :id AND status = 'pending'",
        named_params! { ":st": status, ":resp": response, ":now": now, ":id": run_id },
    )?;
    Ok(())
}

fn summarize_input(v: &Value) -> String {
    let s = v
        .get("command")
        .or_else(|| v.get("file_path"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| v.to_string());
    s.chars().take(200).collect()
}

/// Append one line per invocation to `$CONDUCTOR_HOME/session-hook.log`.
fn log_line(p: &Value, event: &str, started: Instant, outcome: &Result<String>) {
    let Some(home) = std::env::var_os("CONDUCTOR_HOME") else {
        return;
    };
    let mut extra = serde_json::Map::new();
    for k in [
        "source",
        "reason",
        "tool_name",
        "notification_type",
        "message",
        "prompt_id",
        "permission_mode",
        "error",
        "is_interrupt",
        "model",
        "context_tokens",
        "estimated_cache_write_usd",
        "prompt_cache_likely_expired",
        "seconds_since_last_response",
        "background_tasks",
    ] {
        if let Some(v) = p.get(k) {
            extra.insert(k.into(), v.clone());
        }
    }
    let line = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "event": event,
        "session_id": p.get("session_id"),
        "in_process_us": started.elapsed().as_micros() as u64,
        "ppid": parent_pid(),
        "tmux_pane": std::env::var("TMUX_PANE").ok(),
        "outcome": match outcome { Ok(s) => s.clone(), Err(e) => format!("ERR {e}") },
        "keys": p.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
        "extra": extra,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(&home).join("session-hook.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}
