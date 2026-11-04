//! Launch, resume or focus a worktree's interactive Claude Code session in tmux
//! (RFC 021 §5, #3158).
//!
//! [`open_session`] is the single entry point behind `conductor session open`, the
//! `conductor_open_session` MCP tool and the TUI `C` key. Everything that touches tmux
//! goes through the [`Tmux`] trait, so tests need neither tmux nor `claude`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;

use crate::config::{conductor_dir, Config, RepoConfig};
use crate::error::{ConductorError, Result};
use crate::repo::RepoManager;
use crate::tickets::{Ticket, TicketSyncer};
use crate::worktree::{Worktree, WorktreeManager};

/// Default opener for a fresh session on a worktree with a linked ticket.
pub const DEFAULT_OPENER_TEMPLATE: &str = "Work on {{key}} ({{title}}). You are in its conductor \
worktree on branch {{branch}}, cut from {{base}}. Read the ticket in {{source}} first: {{url}}. \
Follow this repo's CLAUDE.md, including what it says to do with the ticket when work starts, \
then investigate and fix it.";

/// How long to wait after launching before checking that claude is still running.
const POST_LAUNCH_SETTLE: Duration = Duration::from_millis(1500);

/// What to do with the user's tmux client once a session is located or launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusMode {
    /// Switch the current tmux client to the session's pane.
    Switch,
    /// Never touch focus; only report the attach command.
    Report,
}

#[derive(Debug, Clone)]
pub struct OpenSessionRequest {
    pub repo_slug: String,
    /// Worktree slug or branch name.
    pub worktree_slug: String,
    /// Skip resuming the newest past session.
    pub new_session: bool,
    /// Explicit start prompt; overrides every generated one.
    pub prompt: Option<String>,
    /// Explicit tmux window name.
    pub window: Option<String>,
    pub focus: FocusMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchMode {
    New,
    Resume { session_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OpenSessionOutcome {
    /// A live session already exists; its pane was located (and focused when asked).
    Focused {
        run_id: String,
        pane: String,
        window_target: String,
        switched: bool,
        attach_command: String,
    },
    /// A live session exists but has no tmux pane to focus (started outside tmux).
    LiveNoPane { run_id: String },
    /// A new window was opened.
    Launched {
        mode: LaunchMode,
        tmux_session: String,
        window: String,
        pane_id: String,
        cwd: String,
        created_session: bool,
        attach_command: String,
        prompt_used: bool,
        switched: bool,
    },
}

impl OpenSessionOutcome {
    /// One-line human summary, shared by the CLI and the MCP tool.
    pub fn summary(&self) -> String {
        match self {
            Self::Focused {
                window_target,
                switched,
                ..
            } => {
                if *switched {
                    format!("Focused live session at {window_target}")
                } else {
                    format!("Session is already live at {window_target}")
                }
            }
            Self::LiveNoPane { run_id } => format!(
                "A live session already exists (run {run_id}) but it did not start in tmux, so there is no pane to focus"
            ),
            Self::Launched {
                mode,
                tmux_session,
                window,
                created_session,
                ..
            } => {
                let how = match mode {
                    LaunchMode::New => "new session".to_string(),
                    LaunchMode::Resume { session_id } => format!("resume {session_id}"),
                };
                let mut s = format!("Launched {window} in {tmux_session} ({how})");
                if *created_session {
                    s.push_str(&format!(
                        ". Created tmux session '{tmux_session}'; attach with: tmux attach -t {tmux_session}"
                    ));
                }
                s
            }
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
// tmux seam
// ---------------------------------------------------------------------------

/// A thin seam over the `tmux` binary.
pub trait Tmux {
    fn has_session(&self, name: &str) -> Result<bool>;
    /// Create the session detached with `argv` as its first window; returns the pane id.
    fn new_session_detached(
        &self,
        session: &str,
        window: &str,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String>;
    /// Add a detached window to an existing session; returns the pane id.
    fn new_window_detached(
        &self,
        session: &str,
        window: &str,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String>;
    fn window_exists(&self, session: &str, window: &str) -> Result<bool>;
    /// `(session_name, window_index)` of a pane, or `None` when the pane is gone.
    fn pane_target(&self, pane: &str) -> Result<Option<(String, String)>>;
    /// Switch the current client to `pane`'s window and select the pane.
    fn switch_to(&self, pane: &str) -> Result<()>;
    fn pane_alive(&self, pane: &str) -> Result<bool>;
}

/// [`Tmux`] backed by the real `tmux` binary.
pub struct RealTmux;

impl RealTmux {
    fn run(args: &[&str]) -> Result<std::process::Output> {
        Command::new("tmux").args(args).output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ConductorError::InvalidInput("tmux is not installed or not on PATH".into())
            } else {
                ConductorError::Io(e)
            }
        })
    }

    fn run_ok(args: &[&str]) -> Result<String> {
        let out = Self::run(args)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(ConductorError::Agent(format!(
                "tmux {} failed: {} (tmux 3.2 or later is required)",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    fn create(
        verb: &str,
        target_flag: &str,
        target: &str,
        window: &str,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String> {
        let mut args: Vec<String> = vec![
            verb.into(),
            "-d".into(),
            target_flag.into(),
            target.into(),
            "-n".into(),
            window.into(),
            "-c".into(),
            cwd.into(),
        ];
        for (k, v) in env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.extend(["-P".into(), "-F".into(), "#{pane_id}".into(), "--".into()]);
        args.extend(argv.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        Self::run_ok(&refs)
    }
}

impl Tmux for RealTmux {
    fn has_session(&self, name: &str) -> Result<bool> {
        Ok(Self::run(&["has-session", "-t", &format!("={name}")])?
            .status
            .success())
    }

    fn new_session_detached(
        &self,
        session: &str,
        window: &str,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String> {
        Self::create("new-session", "-s", session, window, cwd, env, argv)
    }

    fn new_window_detached(
        &self,
        session: &str,
        window: &str,
        cwd: &str,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<String> {
        Self::create(
            "new-window",
            "-t",
            &format!("={session}:"),
            window,
            cwd,
            env,
            argv,
        )
    }

    fn window_exists(&self, session: &str, window: &str) -> Result<bool> {
        let out = Self::run_ok(&[
            "list-windows",
            "-t",
            &format!("={session}:"),
            "-F",
            "#{window_name}",
        ])?;
        Ok(out.lines().any(|l| l == window))
    }

    fn pane_target(&self, pane: &str) -> Result<Option<(String, String)>> {
        let out = Self::run(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_name}\t#{window_index}",
        ])?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .trim()
            .split_once('\t')
            .map(|(s, w)| (s.to_string(), w.to_string())))
    }

    fn switch_to(&self, pane: &str) -> Result<()> {
        // `switch-client -t <pane>` switches session and selects the window and pane.
        Self::run_ok(&["switch-client", "-t", pane]).map(|_| ())
    }

    fn pane_alive(&self, pane: &str) -> Result<bool> {
        let out = Self::run(&["display-message", "-p", "-t", pane, "#{pane_dead}"])?;
        Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "0")
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The columns of an interactive `agent_runs` row the launcher needs.
#[derive(Debug, Clone)]
pub struct InteractiveRunRow {
    pub id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub subprocess_pid: Option<i64>,
    pub subprocess_started_at: Option<String>,
    pub started_at: String,
    pub tmux_pane: Option<String>,
}

/// Interactive runs for a worktree, most recent activity first.
///
/// A resumed session reopens its existing row and `started_at` keeps its original
/// value, so order by the latest of end / idle / start.
pub fn interactive_runs_for_worktree(
    conn: &Connection,
    worktree_id: &str,
) -> Result<Vec<InteractiveRunRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, status, subprocess_pid, subprocess_started_at, started_at, tmux_pane \
         FROM agent_runs WHERE worktree_id = ?1 AND runtime = 'interactive' \
         ORDER BY COALESCE(ended_at, idle_since, started_at) DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([worktree_id], |r| {
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

fn is_live_status(status: &str) -> bool {
    matches!(status, "running" | "waiting_for_feedback" | "idle")
}

/// The first (newest) row in a live status whose process `is_alive` accepts.
pub fn find_live<'a>(
    rows: &'a [InteractiveRunRow],
    is_alive: &dyn Fn(&InteractiveRunRow) -> bool,
) -> Option<&'a InteractiveRunRow> {
    rows.iter()
        .find(|r| is_live_status(&r.status) && is_alive(r))
}

/// Real liveness: the PID exists and was not recycled by another process.
pub fn process_is_alive(row: &InteractiveRunRow) -> bool {
    let Some(pid) = row.subprocess_pid.and_then(|p| u32::try_from(p).ok()) else {
        return false;
    };
    if !crate::process_utils::pid_is_alive(pid) {
        return false;
    }
    let started = row
        .subprocess_started_at
        .as_deref()
        .unwrap_or(&row.started_at);
    !crate::process_utils::pid_was_recycled(pid, started)
}

/// The newest row's `session_id`, for `claude --resume`.
pub fn newest_session_id(rows: &[InteractiveRunRow]) -> Option<String> {
    rows.iter()
        .filter_map(|r| r.session_id.as_deref())
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// `.` and `:` are tmux target separators; whitespace is awkward. Replace them with `-`.
fn sanitize_window_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '.' || c == ':' || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// tmux window name: the ticket key lowercased (`mob-367`, `<repo>-<n>` for GitHub),
/// or the worktree slug when no ticket is linked.
pub fn window_name(ticket: Option<&Ticket>, repo_slug: &str, worktree_slug: &str) -> String {
    let raw = match ticket {
        Some(t) => match t.source_type.as_str() {
            "github" => format!("{repo_slug}-{}", t.source_id),
            "jira" => t.source_id.to_lowercase(),
            other => format!("{other}-{}", t.source_id).to_lowercase(),
        },
        None => worktree_slug.to_string(),
    };
    sanitize_window_name(&raw)
}

fn ticket_key(t: &Ticket) -> String {
    match t.source_type.as_str() {
        "github" => format!("#{}", t.source_id),
        _ => t.source_id.clone(),
    }
}

fn ticket_source_label(t: &Ticket) -> String {
    match t.source_type.as_str() {
        "github" => "GitHub".to_string(),
        "jira" => "Jira".to_string(),
        other => other.to_string(),
    }
}

/// Replace `{{name}}` placeholders. Unknown placeholders are left as written.
pub fn render_template(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Context a start-prompt template can use.
pub struct PromptContext<'a> {
    pub ticket: Option<&'a Ticket>,
    pub worktree: &'a Worktree,
    pub repo_default_branch: &'a str,
}

/// The start prompt, if any.
///
/// An explicit prompt always wins. Resuming uses `resume_template` when set, otherwise no
/// prompt. A fresh session with a linked ticket gets the opener; without a ticket, none.
pub fn start_prompt(
    explicit: Option<&str>,
    resuming: bool,
    opener_template: Option<&str>,
    resume_template: Option<&str>,
    ctx: &PromptContext<'_>,
) -> Option<String> {
    if let Some(p) = explicit.filter(|p| !p.trim().is_empty()) {
        return Some(p.to_string());
    }
    let template = if resuming {
        resume_template?
    } else {
        ctx.ticket?;
        opener_template.unwrap_or(DEFAULT_OPENER_TEMPLATE)
    };
    let mut vars: Vec<(&str, String)> = vec![
        ("branch", ctx.worktree.branch.clone()),
        (
            "base",
            ctx.worktree.effective_base(ctx.repo_default_branch).into(),
        ),
        ("worktree_path", ctx.worktree.path.clone()),
    ];
    if let Some(t) = ctx.ticket {
        vars.push(("key", ticket_key(t)));
        vars.push(("title", t.title.clone()));
        vars.push(("url", t.url.clone()));
        vars.push(("source", ticket_source_label(t)));
    }
    Some(render_template(template, &vars))
}

/// argv for the launched process, starting with `claude`.
pub fn claude_args(
    resume: Option<&str>,
    settings: Option<&Path>,
    prompt: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["claude".to_string()];
    if let Some(s) = settings {
        args.push("--settings".into());
        args.push(s.to_string_lossy().into_owned());
    }
    if let Some(id) = resume {
        args.push("--resume".into());
        args.push(id.into());
    }
    if let Some(p) = prompt {
        if p.starts_with('-') {
            args.push("--".into());
        }
        args.push(p.into());
    }
    args
}

/// `tmux attach -t <session> \; select-window -t <target>`.
pub fn attach_command(session: &str, window_target: &str) -> String {
    format!("tmux attach -t {session} \\; select-window -t {window_target}")
}

// ---------------------------------------------------------------------------
// Hooks (--settings fallback)
// ---------------------------------------------------------------------------

/// True when a `conductor@…` plugin is enabled in the user or worktree Claude settings.
pub fn conductor_plugin_enabled(claude_dir: &Path, worktree_path: &Path) -> bool {
    let files = [
        claude_dir.join("settings.json"),
        worktree_path.join(".claude").join("settings.json"),
        worktree_path.join(".claude").join("settings.local.json"),
    ];
    files.iter().any(|f| {
        std::fs::read_to_string(f)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("enabledPlugins").cloned())
            .and_then(|p| p.as_object().cloned())
            .is_some_and(|m| {
                m.iter()
                    .any(|(k, v)| k.starts_with("conductor@") && v.as_bool() == Some(true))
            })
    })
}

/// Settings JSON for `claude --settings`: the plugin's `hooks.json` events with every hook
/// command replaced by `conductor session hook`.
pub fn hooks_settings_json() -> Result<String> {
    let mut value: serde_json::Value =
        serde_json::from_str(include_str!("../../../plugin/hooks/hooks.json"))
            .map_err(|e| ConductorError::Config(format!("plugin hooks.json: {e}")))?;
    let hooks = value
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .ok_or_else(|| ConductorError::Config("plugin hooks.json has no hooks".into()))?;
    for entries in hooks.values_mut() {
        for entry in entries.as_array_mut().into_iter().flatten() {
            for hook in entry
                .get_mut("hooks")
                .and_then(|h| h.as_array_mut())
                .into_iter()
                .flatten()
            {
                if let Some(obj) = hook.as_object_mut() {
                    obj.insert(
                        "command".into(),
                        "conductor session hook >/dev/null 2>&1 || true".into(),
                    );
                }
            }
        }
    }
    let settings = serde_json::json!({ "hooks": value["hooks"] });
    serde_json::to_string_pretty(&settings)
        .map_err(|e| ConductorError::Config(format!("serialize hooks settings: {e}")))
}

/// Write `<conductor_dir>/session-hooks.json` atomically and return its path.
fn write_hooks_settings_file() -> Result<PathBuf> {
    let dir = conductor_dir();
    std::fs::create_dir_all(dir)?;
    let path = dir.join("session-hooks.json");
    let tmp = dir.join(format!("session-hooks.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, hooks_settings_json()?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn claude_dir(config: &Config) -> Option<PathBuf> {
    config
        .general
        .resolve_optional_claude_dir()
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
}

fn launch_env(config: &Config) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(dir) = config.general.resolve_optional_claude_dir() {
        env.push((
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        ));
    }
    for key in ["CONDUCTOR_HOME", "CONDUCTOR_DB_PATH"] {
        if let Some(v) = std::env::var_os(key).filter(|v| !v.is_empty()) {
            env.push((key.to_string(), v.to_string_lossy().into_owned()));
        }
    }
    env
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Focus, resume or start the interactive session for a worktree.
pub fn open_session(
    conn: &Connection,
    config: &Config,
    req: &OpenSessionRequest,
    tmux: &dyn Tmux,
) -> Result<OpenSessionOutcome> {
    open_session_with(
        conn,
        config,
        req,
        tmux,
        &process_is_alive,
        &|wt_path| {
            let enabled = claude_dir(config)
                .is_some_and(|d| conductor_plugin_enabled(&d, Path::new(wt_path)));
            if enabled {
                Ok(None)
            } else {
                write_hooks_settings_file().map(Some)
            }
        },
        POST_LAUNCH_SETTLE,
    )
}

/// [`open_session`] with its process-level seams injected for tests.
fn open_session_with(
    conn: &Connection,
    config: &Config,
    req: &OpenSessionRequest,
    tmux: &dyn Tmux,
    is_alive: &dyn Fn(&InteractiveRunRow) -> bool,
    hooks_settings: &dyn Fn(&str) -> Result<Option<PathBuf>>,
    settle: Duration,
) -> Result<OpenSessionOutcome> {
    let repo = RepoManager::new(conn, config).get_by_slug(&req.repo_slug)?;
    let wt =
        WorktreeManager::new(conn, config).get_by_slug_or_branch(&repo.id, &req.worktree_slug)?;
    if !wt.is_active() {
        return Err(ConductorError::InvalidInput(format!(
            "worktree {} is not active; cannot open a session in it",
            wt.slug
        )));
    }

    let rows = interactive_runs_for_worktree(conn, &wt.id)?;

    // 1. Already live: focus rather than open a second session on the same transcript.
    if let Some(live) = find_live(&rows, is_alive) {
        let Some(pane) = live.tmux_pane.as_deref().filter(|p| !p.is_empty()) else {
            return Ok(OpenSessionOutcome::LiveNoPane {
                run_id: live.id.clone(),
            });
        };
        let Some((session, index)) = tmux.pane_target(pane)? else {
            return Ok(OpenSessionOutcome::LiveNoPane {
                run_id: live.id.clone(),
            });
        };
        let window_target = format!("{session}:{index}");
        let switched = req.focus == FocusMode::Switch;
        if switched {
            tmux.switch_to(pane)?;
        }
        return Ok(OpenSessionOutcome::Focused {
            run_id: live.id.clone(),
            pane: pane.to_string(),
            attach_command: attach_command(&session, &window_target),
            window_target,
            switched,
        });
    }

    // 2./3. Resume the newest past session, or start fresh.
    let ticket = wt
        .ticket_id
        .as_deref()
        .and_then(|id| TicketSyncer::new(conn).get_by_id(id).ok());
    let repo_cfg = RepoConfig::load(Path::new(&repo.local_path)).unwrap_or_default();
    let resume_id = if req.new_session {
        None
    } else {
        newest_session_id(&rows)
    };
    let opener = repo_cfg
        .sessions
        .opener_template
        .as_deref()
        .or(config.sessions.opener_template.as_deref());
    let resume_tpl = repo_cfg
        .sessions
        .resume_template
        .as_deref()
        .or(config.sessions.resume_template.as_deref());
    let prompt = start_prompt(
        req.prompt.as_deref(),
        resume_id.is_some(),
        opener,
        resume_tpl,
        &PromptContext {
            ticket: ticket.as_ref(),
            worktree: &wt,
            repo_default_branch: &repo.default_branch,
        },
    );

    let session = config.sessions.tmux_session.clone();
    let window = match req.window.as_deref().filter(|w| !w.is_empty()) {
        Some(w) => sanitize_window_name(w),
        None => window_name(ticket.as_ref(), &repo.slug, &wt.slug),
    };
    let session_exists = tmux.has_session(&session)?;
    if session_exists && tmux.window_exists(&session, &window)? {
        return Err(ConductorError::InvalidInput(format!(
            "tmux window '{window}' already exists in session '{session}'; \
             pass a different window name"
        )));
    }

    let settings = hooks_settings(&wt.path)?;
    let argv = claude_args(resume_id.as_deref(), settings.as_deref(), prompt.as_deref());
    let env = launch_env(config);
    let pane_id = if session_exists {
        tmux.new_window_detached(&session, &window, &wt.path, &env, &argv)?
    } else {
        tmux.new_session_detached(&session, &window, &wt.path, &env, &argv)?
    };

    if !settle.is_zero() {
        std::thread::sleep(settle);
    }
    if !tmux.pane_alive(&pane_id)? {
        return Err(ConductorError::Agent(format!(
            "claude exited right after launch in window '{window}'; run `claude` in {} to see why",
            wt.path
        )));
    }

    let switched = req.focus == FocusMode::Switch;
    if switched {
        tmux.switch_to(&pane_id)?;
    }
    let mode = match resume_id {
        Some(session_id) => LaunchMode::Resume { session_id },
        None => LaunchMode::New,
    };
    Ok(OpenSessionOutcome::Launched {
        mode,
        attach_command: attach_command(&session, &format!("{session}:{window}")),
        tmux_session: session,
        window,
        pane_id,
        cwd: wt.path.clone(),
        created_session: !session_exists,
        prompt_used: prompt.is_some(),
        switched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{create_test_conn, insert_test_repo, insert_test_worktree};
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeTmux {
        session_exists: bool,
        windows: Vec<String>,
        pane_target: Option<(String, String)>,
        pane_alive: bool,
        calls: RefCell<Vec<String>>,
        argv: RefCell<Vec<String>>,
    }

    impl FakeTmux {
        fn new() -> Self {
            Self {
                pane_alive: true,
                ..Default::default()
            }
        }
        fn called(&self, s: &str) -> bool {
            self.calls.borrow().iter().any(|c| c == s)
        }
    }

    impl Tmux for FakeTmux {
        fn has_session(&self, _: &str) -> Result<bool> {
            Ok(self.session_exists)
        }
        fn new_session_detached(
            &self,
            _: &str,
            window: &str,
            _: &str,
            _: &[(String, String)],
            argv: &[String],
        ) -> Result<String> {
            self.calls.borrow_mut().push(format!("new-session:{window}"));
            *self.argv.borrow_mut() = argv.to_vec();
            Ok("%9".into())
        }
        fn new_window_detached(
            &self,
            _: &str,
            window: &str,
            _: &str,
            _: &[(String, String)],
            argv: &[String],
        ) -> Result<String> {
            self.calls.borrow_mut().push(format!("new-window:{window}"));
            *self.argv.borrow_mut() = argv.to_vec();
            Ok("%9".into())
        }
        fn window_exists(&self, _: &str, window: &str) -> Result<bool> {
            Ok(self.windows.iter().any(|w| w == window))
        }
        fn pane_target(&self, _: &str) -> Result<Option<(String, String)>> {
            Ok(self.pane_target.clone())
        }
        fn switch_to(&self, pane: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("switch:{pane}"));
            Ok(())
        }
        fn pane_alive(&self, _: &str) -> Result<bool> {
            Ok(self.pane_alive)
        }
    }

    fn setup() -> (Connection, Config) {
        let conn = create_test_conn();
        insert_test_repo(&conn, "r1", "myrepo", "/nonexistent/myrepo");
        insert_test_worktree(&conn, "w1", "r1", "fix-1-thing", "/tmp/wt/fix-1-thing");
        (conn, Config::default())
    }

    fn add_ticket(conn: &Connection, source_type: &str, source_id: &str) {
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', ?1, ?2, 'Fix the thing', '', 'open', '', 'https://example.com/t', '2024-01-01T00:00:00Z', '{}')",
            [source_type, source_id],
        )
        .unwrap();
        conn.execute("UPDATE worktrees SET ticket_id = 't1' WHERE id = 'w1'", [])
            .unwrap();
    }

    fn add_run(conn: &Connection, id: &str, session: &str, status: &str, started: &str, pane: Option<&str>) {
        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, session_id, prompt, status, started_at, runtime, subprocess_pid, tmux_pane) \
             VALUES (?1, 'w1', ?2, '', ?3, ?4, 'interactive', 4242, ?5)",
            rusqlite::params![id, session, status, started, pane],
        )
        .unwrap();
    }

    fn req(new_session: bool) -> OpenSessionRequest {
        OpenSessionRequest {
            repo_slug: "myrepo".into(),
            worktree_slug: "fix-1-thing".into(),
            new_session,
            prompt: None,
            window: None,
            focus: FocusMode::Report,
        }
    }

    fn run(
        conn: &Connection,
        config: &Config,
        r: &OpenSessionRequest,
        tmux: &FakeTmux,
        alive: bool,
    ) -> Result<OpenSessionOutcome> {
        open_session_with(
            conn,
            config,
            r,
            tmux,
            &|_| alive,
            &|_| Ok(None),
            Duration::ZERO,
        )
    }

    #[test]
    fn fresh_session_without_ticket_is_blank_and_named_by_slug() {
        let (conn, config) = setup();
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        let out = run(&conn, &config, &req(false), &tmux, false).unwrap();
        assert!(tmux.called("new-window:fix-1-thing"));
        assert_eq!(*tmux.argv.borrow(), vec!["claude"]);
        match out {
            OpenSessionOutcome::Launched {
                mode, prompt_used, ..
            } => {
                assert_eq!(mode, LaunchMode::New);
                assert!(!prompt_used);
            }
            o => panic!("unexpected {o:?}"),
        }
    }

    #[test]
    fn fresh_session_with_ticket_gets_opener_and_ticket_window() {
        let (conn, config) = setup();
        add_ticket(&conn, "github", "3158");
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        run(&conn, &config, &req(false), &tmux, false).unwrap();
        assert!(tmux.called("new-window:myrepo-3158"));
        let argv = tmux.argv.borrow();
        assert_eq!(argv.len(), 2);
        assert!(argv[1].starts_with("Work on #3158 (Fix the thing)"));
        assert!(argv[1].contains("https://example.com/t"));
    }

    #[test]
    fn resume_uses_newest_session_and_no_prompt() {
        let (conn, config) = setup();
        add_ticket(&conn, "jira", "MOB-367");
        add_run(&conn, "a", "old-session", "completed", "2024-01-01T00:00:00Z", None);
        add_run(&conn, "b", "new-session", "completed", "2024-02-01T00:00:00Z", None);
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        run(&conn, &config, &req(false), &tmux, false).unwrap();
        assert!(tmux.called("new-window:mob-367"));
        assert_eq!(*tmux.argv.borrow(), vec!["claude", "--resume", "new-session"]);
    }

    #[test]
    fn new_flag_overrides_resume() {
        let (conn, config) = setup();
        add_ticket(&conn, "jira", "MOB-367");
        add_run(&conn, "a", "old-session", "completed", "2024-01-01T00:00:00Z", None);
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        run(&conn, &config, &req(true), &tmux, false).unwrap();
        let argv = tmux.argv.borrow();
        assert!(!argv.contains(&"--resume".to_string()));
        assert!(argv[1].starts_with("Work on MOB-367"));
    }

    #[test]
    fn live_session_is_focused_not_relaunched() {
        let (conn, config) = setup();
        add_run(&conn, "a", "s1", "idle", "2024-01-01T00:00:00Z", Some("%3"));
        let tmux = FakeTmux {
            session_exists: true,
            pane_target: Some(("work".into(), "2".into())),
            ..FakeTmux::new()
        };
        let mut r = req(false);
        r.focus = FocusMode::Switch;
        let out = run(&conn, &config, &r, &tmux, true).unwrap();
        assert!(tmux.called("switch:%3"));
        assert!(!tmux.called("new-window:fix-1-thing"));
        match out {
            OpenSessionOutcome::Focused {
                window_target,
                switched,
                attach_command,
                ..
            } => {
                assert_eq!(window_target, "work:2");
                assert!(switched);
                assert_eq!(
                    attach_command,
                    "tmux attach -t work \\; select-window -t work:2"
                );
            }
            o => panic!("unexpected {o:?}"),
        }
    }

    #[test]
    fn report_mode_never_switches() {
        let (conn, config) = setup();
        add_run(&conn, "a", "s1", "running", "2024-01-01T00:00:00Z", Some("%3"));
        let tmux = FakeTmux {
            pane_target: Some(("work".into(), "2".into())),
            ..FakeTmux::new()
        };
        let out = run(&conn, &config, &req(false), &tmux, true).unwrap();
        assert!(!out.switched());
        assert!(!tmux.called("switch:%3"));
    }

    #[test]
    fn live_without_pane_reports_and_does_not_launch() {
        let (conn, config) = setup();
        add_run(&conn, "a", "s1", "running", "2024-01-01T00:00:00Z", None);
        let tmux = FakeTmux::new();
        let out = run(&conn, &config, &req(false), &tmux, true).unwrap();
        assert!(matches!(out, OpenSessionOutcome::LiveNoPane { .. }));
        assert!(tmux.calls.borrow().is_empty());
    }

    #[test]
    fn dead_pid_does_not_count_as_live() {
        let (conn, config) = setup();
        add_run(&conn, "a", "s1", "running", "2024-01-01T00:00:00Z", Some("%3"));
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        let out = run(&conn, &config, &req(false), &tmux, false).unwrap();
        match out {
            OpenSessionOutcome::Launched { mode, .. } => {
                assert_eq!(mode, LaunchMode::Resume { session_id: "s1".into() })
            }
            o => panic!("unexpected {o:?}"),
        }
    }

    #[test]
    fn missing_tmux_session_is_created_detached() {
        let (conn, config) = setup();
        let tmux = FakeTmux::new();
        let out = run(&conn, &config, &req(false), &tmux, false).unwrap();
        assert!(tmux.called("new-session:fix-1-thing"));
        match out {
            OpenSessionOutcome::Launched {
                created_session,
                attach_command,
                tmux_session,
                ..
            } => {
                assert!(created_session);
                assert_eq!(tmux_session, "work");
                assert_eq!(
                    attach_command,
                    "tmux attach -t work \\; select-window -t work:fix-1-thing"
                );
            }
            o => panic!("unexpected {o:?}"),
        }
    }

    #[test]
    fn window_collision_errors() {
        let (conn, config) = setup();
        let tmux = FakeTmux {
            session_exists: true,
            windows: vec!["fix-1-thing".into()],
            ..FakeTmux::new()
        };
        let err = run(&conn, &config, &req(false), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn dead_pane_after_launch_errors() {
        let (conn, config) = setup();
        let tmux = FakeTmux {
            session_exists: true,
            pane_alive: false,
            ..FakeTmux::new()
        };
        let err = run(&conn, &config, &req(false), &tmux, false).unwrap_err();
        assert!(err.to_string().contains("exited right after launch"));
    }

    #[test]
    fn explicit_prompt_overrides_on_resume_and_fresh() {
        let (conn, config) = setup();
        add_run(&conn, "a", "s1", "completed", "2024-01-01T00:00:00Z", None);
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        let mut r = req(false);
        r.prompt = Some("-status please".into());
        run(&conn, &config, &r, &tmux, false).unwrap();
        assert_eq!(
            *tmux.argv.borrow(),
            vec!["claude", "--resume", "s1", "--", "-status please"]
        );
    }

    #[test]
    fn window_name_cases() {
        let t = |st: &str, id: &str| Ticket {
            id: "t".into(),
            repo_id: "r".into(),
            source_type: st.into(),
            source_id: id.into(),
            title: String::new(),
            body: String::new(),
            state: String::new(),
            labels: String::new(),
            assignee: None,
            priority: None,
            url: String::new(),
            synced_at: String::new(),
            raw_json: String::new(),
            workflow: None,
            agent_map: None,
        };
        assert_eq!(window_name(Some(&t("jira", "MOB-367")), "r", "s"), "mob-367");
        assert_eq!(window_name(Some(&t("github", "12")), "repo", "s"), "repo-12");
        assert_eq!(window_name(None, "repo", "fix.a b:c"), "fix-a-b-c");
    }

    #[test]
    fn resume_template_applies_only_when_resuming() {
        let (conn, mut config) = setup();
        config.sessions.resume_template = Some("status of {{branch}}?".into());
        add_run(&conn, "a", "s1", "completed", "2024-01-01T00:00:00Z", None);
        let tmux = FakeTmux {
            session_exists: true,
            ..FakeTmux::new()
        };
        run(&conn, &config, &req(false), &tmux, false).unwrap();
        assert_eq!(tmux.argv.borrow().last().unwrap(), "status of feat/test?");
        run(&conn, &config, &req(true), &tmux, false).unwrap();
        assert_eq!(*tmux.argv.borrow(), vec!["claude"]);
    }

    #[test]
    fn generated_hooks_cover_plugin_events() {
        let generated: serde_json::Value =
            serde_json::from_str(&hooks_settings_json().unwrap()).unwrap();
        let plugin: serde_json::Value =
            serde_json::from_str(include_str!("../../../plugin/hooks/hooks.json")).unwrap();
        let keys = |v: &serde_json::Value| {
            v["hooks"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&generated), keys(&plugin));
        assert!(hooks_settings_json()
            .unwrap()
            .contains("conductor session hook"));
        assert!(!hooks_settings_json().unwrap().contains("CLAUDE_PLUGIN_ROOT"));
    }

    #[test]
    fn plugin_detection_reads_enabled_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        assert!(!conductor_plugin_enabled(dir.path(), wt.path()));
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"enabledPlugins":{"conductor@conductor-ai":true}}"#,
        )
        .unwrap();
        assert!(conductor_plugin_enabled(dir.path(), wt.path()));
    }
}
