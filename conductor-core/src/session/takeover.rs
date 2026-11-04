//! Take over a finished headless agent run's Claude session interactively (RFC 021, #3163).
//!
//! [`take_over_run`] launches `claude --resume <session_id>` in the run's worktree, in a tmux
//! window, through the #3158 launcher, and records an interactive `agent_runs` row with the
//! same `session_id` and `parent_run_id` set to the headless run. The headless row is never
//! modified.
//!
//! Once a session has been continued by a human, no headless path may resume it again: they
//! all go through [`resumable_session_id`] and start a fresh session instead.

use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;

use super::hook::{PLACEHOLDER_PROMPT, RUNTIME_INTERACTIVE};
use super::launch::{
    attach_command, find_live, launch_window, process_is_alive, real_hooks_settings,
    sanitize_window_name, window_name, FocusMode, InteractiveRunRow, LaunchSpec, Tmux,
    LAUNCH_SETTLE,
};
use crate::agent::status::AgentRunStatus;
use crate::agent::types::EVENT_KIND_SYSTEM;
use crate::agent::AgentManager;
use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::repo::RepoManager;
use crate::tickets::TicketSyncer;
use crate::worktree::WorktreeManager;
use crate::new_id;

// ---------------------------------------------------------------------------
// Ownership: headless code never resumes a session a human has continued
// ---------------------------------------------------------------------------

/// The newest interactive run that owns `session_id`, if any.
///
/// This deliberately covers every interactive row with that `session_id`, not only takeover
/// rows (those with a `parent_run_id`): a session a human has continued, however it started,
/// must never be driven headless. That is a superset of the #3163 rule.
pub fn session_owner(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM agent_runs WHERE session_id = ?1 AND runtime = ?2 \
             ORDER BY started_at DESC, id DESC LIMIT 1",
            params![session_id, RUNTIME_INTERACTIVE],
            |r| r.get(0),
        )
        .optional()?)
}

/// `session_id` when a headless run may resume it: `None` when it is missing, empty, or owned
/// by an interactive run (see [`session_owner`]). Every headless `--resume` path goes through
/// here.
pub fn resumable_session_id(conn: &Connection, session_id: Option<&str>) -> Result<Option<String>> {
    let Some(id) = session_id.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Some(owner) = session_owner(conn, id)? {
        tracing::info!(
            session_id = id,
            interactive_run = %owner,
            "session was continued interactively; not resuming it headless"
        );
        return Ok(None);
    }
    Ok(Some(id.to_string()))
}

// ---------------------------------------------------------------------------
// Request / outcome
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TakeOverRequest {
    /// The headless run to take over.
    pub run_id: String,
    /// Explicit tmux window name.
    pub window: Option<String>,
    pub focus: FocusMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TakeOverOutcome {
    /// A new window resumed the headless run's session.
    Launched {
        interactive_run_id: String,
        headless_run_id: String,
        session_id: String,
        tmux_session: String,
        window: String,
        pane_id: String,
        cwd: String,
        created_session: bool,
        attach_command: String,
        switched: bool,
    },
    /// The run was already taken over and that session is live; its pane was located.
    Focused {
        interactive_run_id: String,
        headless_run_id: String,
        pane: String,
        window_target: String,
        attach_command: String,
        switched: bool,
    },
    /// Already taken over and live, but with no tmux pane to focus.
    LiveNoPane { interactive_run_id: String },
}

impl TakeOverOutcome {
    /// Summary plus, when the client was not switched, the attach command.
    pub fn human_text(&self) -> String {
        let mut text = self.summary();
        if !self.switched() {
            if let Some(cmd) = self.attach_command() {
                text.push_str(&format!("\nAttach with: {cmd}"));
            }
        }
        text
    }

    /// One-line human summary, shared by the CLI, MCP and TUI.
    pub fn summary(&self) -> String {
        match self {
            Self::Launched {
                headless_run_id,
                tmux_session,
                window,
                created_session,
                ..
            } => {
                let mut s = format!(
                    "Took over run {headless_run_id}: launched {window} in {tmux_session} (resume)"
                );
                if *created_session {
                    s.push_str(&format!(
                        ". Created tmux session '{tmux_session}'; attach with: tmux attach -t {tmux_session}"
                    ));
                }
                s
            }
            Self::Focused {
                window_target,
                switched,
                ..
            } => {
                if *switched {
                    format!("Run was already taken over; focused its session at {window_target}")
                } else {
                    format!("Run was already taken over; its session is live at {window_target}")
                }
            }
            Self::LiveNoPane { interactive_run_id } => format!(
                "Run was already taken over (interactive run {interactive_run_id}) but that session did not start in tmux, so there is no pane to focus"
            ),
        }
    }

    /// The command that attaches to the session's window, when there is one.
    pub fn attach_command(&self) -> Option<&str> {
        match self {
            Self::Focused { attach_command, .. } | Self::Launched { attach_command, .. } => {
                Some(attach_command)
            }
            Self::LiveNoPane { .. } => None,
        }
    }

    /// True when the user's tmux client was switched to the session.
    pub fn switched(&self) -> bool {
        match self {
            Self::Focused { switched, .. } | Self::Launched { switched, .. } => *switched,
            Self::LiveNoPane { .. } => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Take over a finished headless run's session. See the module docs.
pub fn take_over_run(
    conn: &Connection,
    config: &Config,
    req: &TakeOverRequest,
    tmux: &dyn Tmux,
) -> Result<TakeOverOutcome> {
    take_over_run_with(
        conn,
        config,
        req,
        tmux,
        &process_is_alive,
        &|wt_path| real_hooks_settings(config, wt_path),
        LAUNCH_SETTLE,
    )
}

/// [`take_over_run`] with its process-level seams injected for tests.
pub(crate) fn take_over_run_with(
    conn: &Connection,
    config: &Config,
    req: &TakeOverRequest,
    tmux: &dyn Tmux,
    is_alive: &dyn Fn(&InteractiveRunRow) -> bool,
    hooks_settings: &dyn Fn(&str) -> Result<Option<PathBuf>>,
    settle: Duration,
) -> Result<TakeOverOutcome> {
    let run_id = req.run_id.as_str();
    let run = AgentManager::new(conn)
        .get_run(run_id)?
        .ok_or_else(|| ConductorError::InvalidInput(format!("agent run {run_id} not found")))?;

    if run.is_interactive() {
        return Err(ConductorError::InvalidInput(format!(
            "run {run_id} is already an interactive session; use `conductor session open`"
        )));
    }
    if !matches!(
        run.status,
        AgentRunStatus::Completed | AgentRunStatus::Failed | AgentRunStatus::Cancelled
    ) {
        return Err(ConductorError::InvalidInput(format!(
            "run {run_id} is {}; stop it first (press x in the TUI) and try again",
            run.status
        )));
    }
    let Some(session_id) = run.session_id.clone().filter(|s| !s.is_empty()) else {
        return Err(ConductorError::InvalidInput(format!(
            "run {run_id} has no Claude session to resume"
        )));
    };
    let Some(worktree_id) = run.worktree_id.as_deref() else {
        return Err(ConductorError::InvalidInput(format!(
            "run {run_id} is not tied to a worktree; only worktree runs can be taken over"
        )));
    };
    let wt = WorktreeManager::new(conn, config).get_by_id(worktree_id)?;
    if !wt.is_active() {
        return Err(ConductorError::InvalidInput(format!(
            "worktree {} is not active; cannot take over a session in it",
            wt.slug
        )));
    }
    let repo = RepoManager::new(conn, config).get_by_id(&wt.repo_id)?;

    // Already taken over and live: focus it rather than resume the transcript twice.
    let owners = interactive_rows_for_session(conn, &session_id)?;
    if let Some(live) = find_live(&owners, is_alive) {
        let Some(pane) = live.tmux_pane.as_deref().filter(|p| !p.is_empty()) else {
            return Ok(TakeOverOutcome::LiveNoPane {
                interactive_run_id: live.id.clone(),
            });
        };
        let Some((session, index)) = tmux.pane_target(pane)? else {
            return Ok(TakeOverOutcome::LiveNoPane {
                interactive_run_id: live.id.clone(),
            });
        };
        let window_target = format!("{session}:{index}");
        let switched = req.focus == FocusMode::Switch;
        if switched {
            tmux.switch_to(pane)?;
        }
        return Ok(TakeOverOutcome::Focused {
            interactive_run_id: live.id.clone(),
            headless_run_id: run.id.clone(),
            pane: pane.to_string(),
            attach_command: attach_command(&session, &window_target),
            window_target,
            switched,
        });
    }

    // Launch `claude --resume <id>` with no prompt and no permission flags: the developer's
    // own interactive mode applies, not the headless `agent_permission_mode`.
    let ticket = match wt.ticket_id.as_deref() {
        None => None,
        Some(id) => match TicketSyncer::new(conn).get_by_id(id) {
            Ok(t) => Some(t),
            Err(ConductorError::TicketNotFound { .. }) => None,
            Err(e) => return Err(e),
        },
    };
    let tmux_session = config.sessions.tmux_session.clone();
    let window = match req.window.as_deref().filter(|w| !w.is_empty()) {
        Some(w) => sanitize_window_name(w),
        None => {
            let base = window_name(ticket.as_ref(), &repo.slug, &wt.slug);
            let session_exists = tmux.has_session(&tmux_session)?;
            if session_exists && tmux.window_exists(&tmux_session, &base)? {
                format!("{base}-takeover")
            } else {
                base
            }
        }
    };

    let launched = launch_window(
        tmux,
        config,
        &LaunchSpec {
            session: &tmux_session,
            window: &window,
            cwd: &wt.path,
            resume: Some(&session_id),
            prompt: None,
        },
        hooks_settings,
        settle,
    )?;

    let pid = tmux.pane_pid(&launched.pane_id)?;
    let interactive_run_id = record_takeover(conn, &run.id, &session_id, &wt.repo_id, &launched.pane_id, pid)?;

    let switched = req.focus == FocusMode::Switch;
    if switched {
        tmux.switch_to(&launched.pane_id)?;
    }
    Ok(TakeOverOutcome::Launched {
        interactive_run_id,
        headless_run_id: run.id,
        session_id,
        attach_command: attach_command(&tmux_session, &format!("{tmux_session}:{window}")),
        tmux_session,
        window,
        pane_id: launched.pane_id,
        cwd: wt.path.clone(),
        created_session: launched.created_session,
        switched,
    })
}

/// Interactive rows with `session_id`, newest first, in the shape the launcher's liveness
/// check takes.
fn interactive_rows_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<InteractiveRunRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, status, subprocess_pid, subprocess_started_at, started_at, tmux_pane \
         FROM agent_runs WHERE session_id = ?1 AND runtime = ?2 \
         ORDER BY COALESCE(ended_at, idle_since, started_at) DESC, id DESC",
    )?;
    let rows = stmt
        .query_map(params![session_id, RUNTIME_INTERACTIVE], |r| {
            Ok(InteractiveRunRow {
                id: r.get(0)?,
                session_id: r.get(1)?,
                status: r.get(2)?,
                subprocess_pid: r.get(3)?,
                subprocess_started_at: r.get(4)?,
                started_at: r.get(5)?,
                tmux_pane: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Write (or adopt) the interactive row for a takeover and return its id.
///
/// The `SessionStart` hook of the resumed session looks the row up by `session_id` and
/// `runtime = 'interactive'`, so whichever of the hook and this function runs first creates
/// the row and the other updates it: both orders converge on one row. It is written after
/// launch, with the pane's pid, so the reaper never sees an active row without a pid.
fn record_takeover(
    conn: &Connection,
    headless_run_id: &str,
    session_id: &str,
    repo_id: &str,
    pane_id: &str,
    pid: Option<u32>,
) -> Result<String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let now = chrono::Utc::now().to_rfc3339();

    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM agent_runs WHERE session_id = ?1 AND runtime = ?2 \
             ORDER BY started_at DESC, id DESC LIMIT 1",
            params![session_id, RUNTIME_INTERACTIVE],
            |r| r.get(0),
        )
        .optional()?;

    let interactive_id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE agent_runs SET parent_run_id = COALESCE(parent_run_id, ?2), tmux_pane = ?3 \
                 WHERE id = ?1",
                params![id, headless_run_id, pane_id],
            )?;
            id
        }
        None => {
            let id = new_id();
            let pid = pid.filter(|p| *p > 0);
            let pid_started = pid.and_then(crate::process_utils::process_started_at_rfc3339);
            tx.execute(
                "INSERT INTO agent_runs (id, worktree_id, repo_id, session_id, prompt, status, \
                 started_at, model, runtime, tmux_pane, parent_run_id, subprocess_pid, \
                 subprocess_started_at) \
                 SELECT ?1, h.worktree_id, ?2, ?3, ?4, ?5, ?6, h.model, ?7, ?8, h.id, ?9, ?10 \
                 FROM agent_runs h WHERE h.id = ?11",
                params![
                    id,
                    repo_id,
                    session_id,
                    PLACEHOLDER_PROMPT,
                    AgentRunStatus::Idle.as_str(),
                    now,
                    RUNTIME_INTERACTIVE,
                    pane_id,
                    pid.map(i64::from),
                    pid_started,
                    headless_run_id,
                ],
            )?;
            id
        }
    };

    let step_name: Option<String> = tx
        .query_row(
            "SELECT step_name FROM workflow_run_steps WHERE child_run_id = ?1 \
             ORDER BY started_at DESC LIMIT 1",
            [headless_run_id],
            |r| r.get(0),
        )
        .optional()?;
    let summary = match step_name {
        Some(step) => format!("Taken over from run {headless_run_id} (step '{step}')"),
        None => format!("Taken over from run {headless_run_id}"),
    };
    tx.execute(
        "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, ended_at, metadata) \
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
        params![
            new_id(),
            interactive_id,
            EVENT_KIND_SYSTEM,
            summary,
            now,
            serde_json::json!({ "taken_over_from": headless_run_id }).to_string()
        ],
    )?;
    tx.commit()?;
    Ok(interactive_id)
}

// ---------------------------------------------------------------------------
// Display query
// ---------------------------------------------------------------------------

/// One takeover: which interactive run continued which headless run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakeoverLink {
    pub interactive_run_id: String,
    pub headless_run_id: String,
    pub interactive_status: String,
    /// The workflow step the headless run was, when it was a step's child run.
    pub step_name: Option<String>,
}

/// Every takeover link, for showing the relation in both directions.
pub fn takeover_links(conn: &Connection) -> Result<Vec<TakeoverLink>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.parent_run_id, i.status, \
                (SELECT MAX(s.step_name) FROM workflow_run_steps s \
                 WHERE s.child_run_id = i.parent_run_id) \
         FROM agent_runs i \
         WHERE i.runtime = ?1 AND i.parent_run_id IS NOT NULL \
         ORDER BY i.started_at, i.id",
    )?;
    let links = stmt
        .query_map([RUNTIME_INTERACTIVE], |r| {
            Ok(TakeoverLink {
                interactive_run_id: r.get(0)?,
                headless_run_id: r.get(1)?,
                interactive_status: r.get(2)?,
                step_name: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::hook::{apply_hook_event, HookEnv, HookEvent, HookOutcome};
    use crate::session::launch::test_support::{FakeTmux, FAKE_PANE_PID};
    use crate::test_helpers::{create_test_conn, insert_test_repo, insert_test_worktree};

    fn setup() -> (Connection, Config) {
        let conn = create_test_conn();
        insert_test_repo(&conn, "r1", "myrepo", "/nonexistent/myrepo");
        insert_test_worktree(&conn, "w1", "r1", "fix-1-thing", "/tmp/wt/fix-1-thing");
        (conn, Config::default())
    }

    fn add_headless(conn: &Connection, id: &str, status: &str, session: Option<&str>) {
        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, repo_id, session_id, prompt, status, \
             started_at, runtime, cost_usd, model) \
             VALUES (?1, 'w1', 'r1', ?2, 'do it', ?3, '2024-01-01T00:00:00Z', 'claude', 1.5, 'opus')",
            params![id, session, status],
        )
        .unwrap();
    }

    fn req(run_id: &str) -> TakeOverRequest {
        TakeOverRequest {
            run_id: run_id.into(),
            window: None,
            focus: FocusMode::Report,
        }
    }

    fn take(
        conn: &Connection,
        config: &Config,
        r: &TakeOverRequest,
        tmux: &FakeTmux,
        alive: bool,
    ) -> Result<TakeOverOutcome> {
        take_over_run_with(
            conn,
            config,
            r,
            tmux,
            &|_| alive,
            &|_| Ok(None),
            Duration::ZERO,
        )
    }

    fn interactive_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE runtime = 'interactive'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn start_event(session: &str, pane: &str) -> HookEvent {
        HookEvent {
            received_at: "2024-01-02T00:00:00Z".into(),
            env: HookEnv {
                attended: true,
                claude_pid: Some(FAKE_PANE_PID),
                tmux_pane: Some(pane.into()),
            },
            hook_event_name: Some("SessionStart".into()),
            session_id: Some(session.into()),
            cwd: Some("/tmp/wt/fix-1-thing".into()),
            source: Some("resume".into()),
            ..Default::default()
        }
    }

    #[test]
    fn terminal_runs_launch_resume_with_no_permission_flags() {
        for status in ["completed", "failed", "cancelled"] {
            let (conn, config) = setup();
            add_headless(&conn, "h1", status, Some("sess-1"));
            let tmux = FakeTmux::new();
            let out = take(&conn, &config, &req("h1"), &tmux, false).unwrap();
            let TakeOverOutcome::Launched {
                headless_run_id,
                session_id,
                cwd,
                ..
            } = &out
            else {
                panic!("expected Launched, got {out:?}");
            };
            assert_eq!(headless_run_id, "h1");
            assert_eq!(session_id, "sess-1");
            assert_eq!(cwd, "/tmp/wt/fix-1-thing");
            assert_eq!(*tmux.argv.borrow(), ["claude", "--resume", "sess-1"]);
            assert_eq!(*tmux.cwd.borrow(), "/tmp/wt/fix-1-thing");
            let argv = tmux.argv.borrow().join(" ");
            assert!(!argv.contains("permission"));
            assert!(!argv.contains("dangerously"));
        }
    }

    #[test]
    fn non_terminal_runs_are_refused_with_stop_hint() {
        for status in ["running", "waiting_for_feedback", "idle"] {
            let (conn, config) = setup();
            add_headless(&conn, "h1", status, Some("sess-1"));
            let tmux = FakeTmux::new();
            let err = take(&conn, &config, &req("h1"), &tmux, false)
                .unwrap_err()
                .to_string();
            assert!(err.contains("stop it first"), "{status}: {err}");
            assert!(tmux.calls.borrow().is_empty());
            assert_eq!(interactive_count(&conn), 0);
        }
    }

    #[test]
    fn invalid_runs_are_refused() {
        let (conn, config) = setup();
        let tmux = FakeTmux::new();
        let err = take(&conn, &config, &req("nope"), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("not found"));

        add_headless(&conn, "nosess", "completed", None);
        let err = take(&conn, &config, &req("nosess"), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("no Claude session"));

        add_headless(&conn, "emptysess", "completed", Some(""));
        let err = take(&conn, &config, &req("emptysess"), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("no Claude session"));

        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, session_id, prompt, status, started_at, runtime) \
             VALUES ('inter', 'w1', 's9', '', 'completed', '2024-01-01T00:00:00Z', 'interactive')",
            [],
        )
        .unwrap();
        let err = take(&conn, &config, &req("inter"), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("already an interactive"));

        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, session_id, prompt, status, started_at) \
             VALUES ('nowt', NULL, 's8', '', 'completed', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let err = take(&conn, &config, &req("nowt"), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("not tied to a worktree"));

        add_headless(&conn, "h2", "completed", Some("sess-2"));
        conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w1'", [])
            .unwrap();
        let err = take(&conn, &config, &req("h2"), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("not active"));
        assert!(tmux.calls.borrow().is_empty());
    }

    #[test]
    fn records_interactive_row_and_leaves_headless_untouched() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        let tmux = FakeTmux::new();
        let out = take(&conn, &config, &req("h1"), &tmux, false).unwrap();
        let TakeOverOutcome::Launched {
            interactive_run_id, ..
        } = out
        else {
            panic!("expected Launched");
        };

        let (runtime, session, parent, pane, pid, status, model): (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT runtime, session_id, parent_run_id, tmux_pane, subprocess_pid, status, model \
                 FROM agent_runs WHERE id = ?1",
                [&interactive_run_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(runtime, "interactive");
        assert_eq!(session, "sess-1");
        assert_eq!(parent.as_deref(), Some("h1"));
        assert_eq!(pane.as_deref(), Some("%9"));
        assert_eq!(pid, Some(i64::from(FAKE_PANE_PID)));
        assert_eq!(status, "idle");
        assert_eq!(model.as_deref(), Some("opus"));

        let (hstatus, hsession, hcost, hparent, hruntime): (
            String,
            String,
            f64,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT status, session_id, cost_usd, parent_run_id, runtime FROM agent_runs WHERE id = 'h1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(hstatus, "completed");
        assert_eq!(hsession, "sess-1");
        assert_eq!(hcost, 1.5);
        assert_eq!(hparent, None);
        assert_eq!(hruntime, "claude");

        let events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_run_events WHERE run_id = ?1 AND summary LIKE 'Taken over from run h1%'",
                [&interactive_run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 1);
    }

    #[test]
    fn hook_row_first_gets_parent_and_no_duplicate() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        let outcome = apply_hook_event(&conn, &start_event("sess-1", "%9")).unwrap();
        assert!(matches!(outcome, HookOutcome::Applied { .. }));
        assert_eq!(interactive_count(&conn), 1);

        let tmux = FakeTmux::new();
        let out = take(&conn, &config, &req("h1"), &tmux, false).unwrap();
        let TakeOverOutcome::Launched {
            interactive_run_id, ..
        } = out
        else {
            panic!("expected Launched");
        };
        assert_eq!(interactive_count(&conn), 1);
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_run_id FROM agent_runs WHERE id = ?1",
                [&interactive_run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("h1"));
    }

    #[test]
    fn takeover_row_first_then_hook_attaches_to_it() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        let tmux = FakeTmux::new();
        let out = take(&conn, &config, &req("h1"), &tmux, false).unwrap();
        let TakeOverOutcome::Launched {
            interactive_run_id, ..
        } = out
        else {
            panic!("expected Launched");
        };
        let outcome = apply_hook_event(&conn, &start_event("sess-1", "%9")).unwrap();
        assert!(matches!(outcome, HookOutcome::Applied { ref run_id } if *run_id == interactive_run_id));
        assert_eq!(interactive_count(&conn), 1);
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_run_id FROM agent_runs WHERE id = ?1",
                [&interactive_run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent.as_deref(), Some("h1"));
    }

    #[test]
    fn retakeover_focuses_when_live_and_relaunches_when_not() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        let tmux = FakeTmux::new();
        let first = take(&conn, &config, &req("h1"), &tmux, false).unwrap();
        let TakeOverOutcome::Launched {
            interactive_run_id, ..
        } = first
        else {
            panic!("expected Launched");
        };

        // Live: focus, no new launch.
        let mut live = FakeTmux::new();
        live.pane_target = Some(("work".into(), "3".into()));
        let mut r = req("h1");
        r.focus = FocusMode::Switch;
        let out = take(&conn, &config, &r, &live, true).unwrap();
        assert!(matches!(
            out,
            TakeOverOutcome::Focused { interactive_run_id: ref id, switched: true, .. }
                if *id == interactive_run_id
        ));
        assert!(live.called("switch:%9"));
        assert!(!live.calls.borrow().iter().any(|c| c.starts_with("new-")));

        // Live but no pane known to tmux.
        let nopane = FakeTmux::new();
        let out = take(&conn, &config, &req("h1"), &nopane, true).unwrap();
        assert!(matches!(out, TakeOverOutcome::LiveNoPane { .. }));

        // Not live: relaunch and reuse the row.
        let again = FakeTmux::new();
        let out = take(&conn, &config, &req("h1"), &again, false).unwrap();
        assert!(matches!(out, TakeOverOutcome::Launched { .. }));
        assert_eq!(interactive_count(&conn), 1);
    }

    #[test]
    fn window_fallback_and_collisions() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));

        // Base name taken -> "<base>-takeover".
        let mut tmux = FakeTmux::new();
        tmux.session_exists = true;
        tmux.windows = vec!["fix-1-thing".into()];
        let out = take(&conn, &config, &req("h1"), &tmux, false).unwrap();
        let TakeOverOutcome::Launched { window, .. } = out else {
            panic!("expected Launched");
        };
        assert_eq!(window, "fix-1-thing-takeover");

        // Both taken -> error.
        conn.execute("DELETE FROM agent_runs WHERE runtime = 'interactive'", [])
            .unwrap();
        let mut both = FakeTmux::new();
        both.session_exists = true;
        both.windows = vec!["fix-1-thing".into(), "fix-1-thing-takeover".into()];
        let err = take(&conn, &config, &req("h1"), &both, false).unwrap_err();
        assert!(err.to_string().contains("already exists"));

        // Explicit window collision -> error, no fallback.
        let mut explicit = FakeTmux::new();
        explicit.session_exists = true;
        explicit.windows = vec!["mine".into()];
        let mut r = req("h1");
        r.window = Some("mine".into());
        let err = take(&conn, &config, &r, &explicit, false).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn dead_pane_after_launch_records_nothing() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        let mut tmux = FakeTmux::new();
        tmux.pane_alive = false;
        assert!(take(&conn, &config, &req("h1"), &tmux, false).is_err());
        assert_eq!(interactive_count(&conn), 0);
    }

    #[test]
    fn ownership_helpers() {
        let (conn, _config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        assert_eq!(session_owner(&conn, "sess-1").unwrap(), None);
        assert_eq!(
            resumable_session_id(&conn, Some("sess-1")).unwrap().as_deref(),
            Some("sess-1")
        );
        assert_eq!(resumable_session_id(&conn, Some("")).unwrap(), None);
        assert_eq!(resumable_session_id(&conn, None).unwrap(), None);

        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, session_id, prompt, status, started_at, runtime, parent_run_id) \
             VALUES ('i1', 'w1', 'sess-1', '', 'idle', '2024-01-02T00:00:00Z', 'interactive', 'h1')",
            [],
        )
        .unwrap();
        assert_eq!(session_owner(&conn, "sess-1").unwrap().as_deref(), Some("i1"));
        assert_eq!(resumable_session_id(&conn, Some("sess-1")).unwrap(), None);
        // A different session is unaffected.
        assert_eq!(
            resumable_session_id(&conn, Some("other")).unwrap().as_deref(),
            Some("other")
        );
    }

    #[test]
    fn takeover_links_report_step_names() {
        let (conn, config) = setup();
        add_headless(&conn, "h1", "completed", Some("sess-1"));
        add_headless(&conn, "h2", "failed", Some("sess-2"));
        let tmux = FakeTmux::new();
        take(&conn, &config, &req("h1"), &tmux, false).unwrap();
        let tmux2 = FakeTmux::new();
        take(&conn, &config, &req("h2"), &tmux2, false).unwrap();

        let wf = crate::test_helpers::seed_workflow_run_on_worktree(
            &conn,
            "w1",
            "wf",
            crate::workflow::WorkflowRunStatus::Completed,
            None,
            None,
        );
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, status, child_run_id, position) \
             VALUES ('s1', ?1, 'implement', 'actor', 'completed', 'h1', 0)",
            [&wf],
        )
        .unwrap();

        let links = takeover_links(&conn).unwrap();
        assert_eq!(links.len(), 2);
        let l1 = links.iter().find(|l| l.headless_run_id == "h1").unwrap();
        assert_eq!(l1.step_name.as_deref(), Some("implement"));
        assert_eq!(l1.interactive_status, "idle");
        let l2 = links.iter().find(|l| l.headless_run_id == "h2").unwrap();
        assert_eq!(l2.step_name, None);
    }
}
