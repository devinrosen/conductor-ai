//! Block until a workflow run finishes, stalls, or a timeout passes.
//!
//! One loop ([`wait_for_workflow_run`]) backs both `conductor workflow wait` and the
//! `conductor_wait_run` MCP tool. Each poll classifies a snapshot with [`evaluate`].
//!
//! ## Stall definition
//!
//! A run is *stalled* when it is non-terminal and its newest sign of life is older
//! than `stall_after`. Signs of life across the run and all descendant runs are:
//! `lease_until` (a live engine keeps it in the future), `started_at`, and each
//! step's `started_at` / `ended_at`. "Lease stale AND no step progress" is used
//! rather than OR, because long agent steps legitimately emit no step events for a
//! long time while the engine keeps refreshing its lease.
//!
//! All timestamps are normalized with `datetime()` in SQL: `lease_until` is written
//! as `YYYY-MM-DD HH:MM:SS` while `started_at`/`ended_at` are RFC 3339, and comparing
//! them as strings is wrong (see #3076).

use std::time::{Duration, Instant};

use rusqlite::{named_params, Connection};
use serde::Serialize;

use crate::error::{ConductorError, Result};
use crate::workflow::{ConductorWorkflowRun, WorkflowRunStatus, WorkflowStepStatus};

use super::queries::{get_workflow_run, get_workflow_steps};

/// How often the wait loop re-checks the run.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How a wait ended. Maps to the `conductor workflow wait` exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitOutcome {
    Completed,
    Failed,
    Cancelled,
    Timeout,
    Stalled,
}

impl WaitOutcome {
    /// Process exit code: 0 completed, 1 failed or cancelled, 2 timeout, 3 stalled.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Failed | Self::Cancelled => 1,
            Self::Timeout => 2,
            Self::Stalled => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Stalled => "stalled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WaitOptions {
    /// Give up after this long. `None` waits until the run finishes or stalls.
    pub timeout: Option<Duration>,
    /// A non-terminal run with no sign of life for this long is reported as stalled.
    pub stall_after: Duration,
    pub poll_interval: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        let lease_ttl = runkon_flow::types::WorkflowExecConfig::default().lease_ttl_secs;
        Self {
            timeout: None,
            stall_after: Duration::from_secs((lease_ttl.max(1) as u64) * 2),
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

impl WaitOptions {
    /// Defaults with optional caller overrides, shared by the CLI and the MCP tool.
    pub fn with_overrides(timeout: Option<Duration>, stall_after: Option<Duration>) -> Self {
        let defaults = Self::default();
        Self {
            timeout,
            stall_after: stall_after.unwrap_or(defaults.stall_after),
            ..defaults
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WaitStepSummary {
    pub step_name: String,
    pub status: String,
    pub iteration: i64,
    pub step_error: Option<String>,
    pub context_out: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WaitSummary {
    pub run_id: String,
    pub workflow_name: String,
    pub outcome: WaitOutcome,
    /// Raw run status at the time the wait ended.
    pub status: String,
    pub error: Option<String>,
    pub elapsed_secs: u64,
    /// Seconds since the newest sign of life in the run tree, if any.
    pub last_signal_age_secs: Option<i64>,
    pub last_step: Option<WaitStepSummary>,
}

/// Recursive CTE yielding `tree(id)` for `:root` and all its descendant runs.
const RUN_TREE_CTE: &str = "WITH RECURSIVE tree(id) AS (
           SELECT :root
           UNION ALL
           SELECT r.id FROM workflow_runs r JOIN tree t ON r.parent_workflow_run_id = t.id
         )";

/// Returns `(age of newest sign of life in secs, non-terminal descendants with a fresh lease)`.
fn run_tree_liveness(conn: &Connection, run_id: &str) -> Result<(Option<i64>, usize)> {
    let age: Option<i64> = conn.query_row(
        &format!(
            "{RUN_TREE_CTE},
         signals(ts) AS (
           SELECT datetime(lease_until) FROM workflow_runs WHERE id IN (SELECT id FROM tree)
           UNION ALL SELECT datetime(started_at) FROM workflow_runs WHERE id IN (SELECT id FROM tree)
           UNION ALL SELECT datetime(started_at) FROM workflow_run_steps
             WHERE workflow_run_id IN (SELECT id FROM tree)
           UNION ALL SELECT datetime(ended_at) FROM workflow_run_steps
             WHERE workflow_run_id IN (SELECT id FROM tree)
         )
         SELECT CAST((julianday('now') - julianday(MAX(ts))) * 86400 AS INTEGER) FROM signals"
        ),
        named_params![":root": run_id],
        |row| row.get(0),
    )?;

    let live_descendants: i64 = conn.query_row(
        &format!(
            "{RUN_TREE_CTE}
         SELECT COUNT(*) FROM workflow_runs
         WHERE id IN (SELECT id FROM tree)
           AND id != :root
           AND status NOT IN ('completed', 'failed', 'cancelled')
           AND lease_until IS NOT NULL
           AND datetime(lease_until) > datetime('now')"
        ),
        named_params![":root": run_id],
        |row| row.get(0),
    )?;

    Ok((age, live_descendants.max(0) as usize))
}

fn load_run(conn: &Connection, run_id: &str) -> Result<ConductorWorkflowRun> {
    get_workflow_run(conn, run_id)?
        .ok_or_else(|| ConductorError::WorkflowRunNotFound { id: run_id.into() })
}

fn last_step_summary(conn: &Connection, run_id: &str) -> Result<Option<WaitStepSummary>> {
    let steps = get_workflow_steps(conn, run_id)?;
    // `started_at` is RFC 3339 for every step, so string order is time order here;
    // steps that never started sort first.
    let last = steps.iter().max_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then(a.position.cmp(&b.position))
    });
    Ok(last.map(|s| WaitStepSummary {
        step_name: s.step_name.clone(),
        status: s.status.to_string(),
        iteration: s.iteration,
        step_error: if s.status == WorkflowStepStatus::Failed {
            s.result_text.clone()
        } else {
            None
        },
        context_out: s.context_out.clone(),
        started_at: s.started_at.clone(),
        ended_at: s.ended_at.clone(),
    }))
}

fn build_summary(
    conn: &Connection,
    run: &ConductorWorkflowRun,
    outcome: WaitOutcome,
    started: Instant,
) -> Result<WaitSummary> {
    let run_id = run.id.as_str();
    let (age, _) = run_tree_liveness(conn, run_id)?;
    Ok(WaitSummary {
        run_id: run.id.clone(),
        workflow_name: run.workflow_name.clone(),
        outcome,
        status: run.status.to_string(),
        error: run.error.clone(),
        elapsed_secs: started.elapsed().as_secs(),
        last_signal_age_secs: age,
        last_step: last_step_summary(conn, run_id)?,
    })
}

/// Classify one snapshot. `Some` if the run is finished or stalled, `None` to keep polling.
///
/// A future progress signal for engines that are alive but stuck (#2837) can extend
/// the "sign of life" set here.
fn evaluate(
    conn: &Connection,
    run_id: &str,
    opts: &WaitOptions,
    started: Instant,
) -> Result<Option<WaitSummary>> {
    let run = load_run(conn, run_id)?;
    let status = run.status.clone();
    let (age, live_descendants) = run_tree_liveness(conn, run_id)?;

    if status.is_terminal() {
        // A descendant still driven by a live engine keeps the wait open; orphans with
        // stale leases do not.
        if live_descendants > 0 {
            return Ok(None);
        }
        let outcome = match status {
            WorkflowRunStatus::Completed => WaitOutcome::Completed,
            WorkflowRunStatus::Cancelled => WaitOutcome::Cancelled,
            _ => WaitOutcome::Failed,
        };
        return Ok(Some(build_summary(conn, &run, outcome, started)?));
    }

    if age.is_some_and(|a| a > opts.stall_after.as_secs() as i64) {
        return Ok(Some(build_summary(
            conn,
            &run,
            WaitOutcome::Stalled,
            started,
        )?));
    }
    Ok(None)
}

/// Block until the run (and its child runs) finish, the run stalls, or `opts.timeout` passes.
///
/// Returns `WorkflowRunNotFound` for an unknown run ID.
pub fn wait_for_workflow_run(
    conn: &Connection,
    run_id: &str,
    opts: &WaitOptions,
) -> Result<WaitSummary> {
    let started = Instant::now();
    loop {
        if let Some(summary) = evaluate(conn, run_id, opts, started)? {
            return Ok(summary);
        }
        if let Some(timeout) = opts.timeout {
            if started.elapsed() >= timeout {
                return build_summary(
                    conn,
                    &load_run(conn, run_id)?,
                    WaitOutcome::Timeout,
                    started,
                );
            }
        }
        std::thread::sleep(opts.poll_interval);
    }
}

/// Human-readable summary, shared by the CLI and the MCP tool. Starts with `outcome: <x>`.
pub fn format_wait_summary(s: &WaitSummary) -> String {
    let mut out = format!(
        "outcome: {}\nrun: {} ({})\nstatus: {}\nwaited: {}s\n",
        s.outcome.as_str(),
        s.run_id,
        s.workflow_name,
        s.status,
        s.elapsed_secs
    );
    if let Some(err) = &s.error {
        out.push_str(&format!("error: {err}\n"));
    }
    if let Some(step) = &s.last_step {
        out.push_str(&format!(
            "last step: {} [{}] (iteration {})\n",
            step.step_name, step.status, step.iteration
        ));
        if let Some(e) = &step.step_error {
            out.push_str(&format!("  step error: {e}\n"));
        }
        if let Some(c) = &step.context_out {
            out.push_str(&format!("  context: {c}\n"));
        }
    }
    match s.outcome {
        WaitOutcome::Stalled => {
            let age = s.last_signal_age_secs.unwrap_or(0);
            out.push_str(&format!(
                "hint: no live engine (no lease or step activity for {age}s). \
                 Resume with `conductor workflow resume {}`.\n",
                s.run_id
            ));
        }
        WaitOutcome::Timeout => {
            out.push_str("hint: run is still in progress; wait again to keep waiting.\n");
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        crate::test_helpers::setup_db()
    }

    fn insert_run(
        conn: &Connection,
        id: &str,
        parent_wf: Option<&str>,
        status: &str,
        lease_offset: Option<&str>,
        started_offset: &str,
    ) {
        // `parent_run_id` is a NOT NULL FK to agent_runs; reuse one agent run.
        let agent_parent = ensure_agent_run(conn);
        let lease = lease_offset.map(|o| format!("datetime('now', '{o}')"));
        let sql = format!(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, parent_workflow_run_id, status, \
              dry_run, trigger, started_at, iteration, lease_until) \
             VALUES (:id, 'test-wf', 'w1', :agent, :pwf, :status, 0, 'manual', \
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '{started_offset}'), 0, {})",
            lease.unwrap_or_else(|| "NULL".into())
        );
        conn.execute(
            &sql,
            named_params![":id": id, ":agent": agent_parent, ":pwf": parent_wf, ":status": status],
        )
        .unwrap();
    }

    fn ensure_agent_run(conn: &Connection) -> String {
        crate::agent::AgentManager::new(conn)
            .create_run(Some("w1"), "workflow", None)
            .unwrap()
            .id
    }

    fn insert_step(
        conn: &Connection,
        id: &str,
        run_id: &str,
        name: &str,
        started: &str,
        ended: Option<&str>,
    ) {
        let ended_sql = ended
            .map(|o| format!("strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '{o}')"))
            .unwrap_or_else(|| "NULL".into());
        let sql = format!(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, started_at, ended_at, context_out) \
             VALUES (:id, :run, :name, 'actor', 0, 'completed', \
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '{started}'), {ended_sql}, 'ctx')"
        );
        conn.execute(
            &sql,
            named_params![":id": id, ":run": run_id, ":name": name],
        )
        .unwrap();
    }

    fn set_step(conn: &Connection, id: &str, status: &str, position: i64, result: Option<&str>) {
        conn.execute(
            "UPDATE workflow_run_steps SET status = :s, position = :p, result_text = :r WHERE id = :id",
            named_params![":s": status, ":p": position, ":r": result, ":id": id],
        )
        .unwrap();
    }

    fn opts() -> WaitOptions {
        WaitOptions {
            timeout: None,
            stall_after: Duration::from_secs(60),
            poll_interval: Duration::ZERO,
        }
    }

    fn eval(conn: &Connection, id: &str) -> Option<WaitSummary> {
        evaluate(conn, id, &opts(), Instant::now()).unwrap()
    }

    #[test]
    fn terminal_statuses_map_to_outcomes() {
        let conn = setup();
        for (id, status, want) in [
            ("a", "completed", WaitOutcome::Completed),
            ("b", "failed", WaitOutcome::Failed),
            ("c", "cancelled", WaitOutcome::Cancelled),
        ] {
            insert_run(&conn, id, None, status, None, "-1 hours");
            assert_eq!(eval(&conn, id).unwrap().outcome, want);
        }
        assert_eq!(WaitOutcome::Completed.exit_code(), 0);
        assert_eq!(WaitOutcome::Failed.exit_code(), 1);
        assert_eq!(WaitOutcome::Cancelled.exit_code(), 1);
        assert_eq!(WaitOutcome::Timeout.exit_code(), 2);
        assert_eq!(WaitOutcome::Stalled.exit_code(), 3);
    }

    #[test]
    fn fresh_lease_is_not_stalled() {
        let conn = setup();
        insert_run(&conn, "r", None, "running", Some("+20 seconds"), "-1 hours");
        insert_step(&conn, "s", "r", "old", "-1 hours", Some("-59 minutes"));
        assert!(eval(&conn, "r").is_none());
    }

    #[test]
    fn stale_lease_and_old_steps_is_stalled() {
        let conn = setup();
        insert_run(
            &conn,
            "r",
            None,
            "running",
            Some("-300 seconds"),
            "-1 hours",
        );
        insert_step(&conn, "s", "r", "old", "-1 hours", Some("-300 seconds"));
        let s = eval(&conn, "r").unwrap();
        assert_eq!(s.outcome, WaitOutcome::Stalled);
        assert!(s.last_signal_age_secs.unwrap() >= 299);
    }

    #[test]
    fn recent_step_blocks_stall() {
        let conn = setup();
        insert_run(
            &conn,
            "r",
            None,
            "running",
            Some("-300 seconds"),
            "-1 hours",
        );
        insert_step(&conn, "s", "r", "recent", "-10 seconds", None);
        assert!(eval(&conn, "r").is_none());
    }

    #[test]
    fn pending_null_lease_falls_back_to_started_at() {
        let conn = setup();
        insert_run(&conn, "fresh", None, "pending", None, "-5 seconds");
        assert!(eval(&conn, "fresh").is_none());
        insert_run(&conn, "old", None, "pending", None, "-300 seconds");
        assert_eq!(eval(&conn, "old").unwrap().outcome, WaitOutcome::Stalled);
    }

    #[test]
    fn child_with_fresh_lease_keeps_wait_alive() {
        let conn = setup();
        insert_run(
            &conn,
            "root",
            None,
            "running",
            Some("-300 seconds"),
            "-1 hours",
        );
        insert_run(
            &conn,
            "kid",
            Some("root"),
            "running",
            Some("+20 seconds"),
            "-1 hours",
        );
        assert!(eval(&conn, "root").is_none());
    }

    #[test]
    fn terminal_root_waits_for_live_child_but_ignores_orphan() {
        let conn = setup();
        insert_run(&conn, "root", None, "completed", None, "-1 hours");
        insert_run(
            &conn,
            "kid",
            Some("root"),
            "running",
            Some("+20 seconds"),
            "-1 hours",
        );
        assert!(eval(&conn, "root").is_none());

        conn.execute(
            "UPDATE workflow_runs SET lease_until = datetime('now', '-300 seconds') WHERE id = 'kid'",
            [],
        )
        .unwrap();
        assert_eq!(eval(&conn, "root").unwrap().outcome, WaitOutcome::Completed);
    }

    #[test]
    fn zero_timeout_returns_timeout() {
        let conn = setup();
        insert_run(
            &conn,
            "r",
            None,
            "running",
            Some("+20 seconds"),
            "-1 minutes",
        );
        let o = WaitOptions {
            timeout: Some(Duration::ZERO),
            ..opts()
        };
        let s = wait_for_workflow_run(&conn, "r", &o).unwrap();
        assert_eq!(s.outcome, WaitOutcome::Timeout);
        assert_eq!(s.outcome.exit_code(), 2);
    }

    #[test]
    fn unknown_run_is_error() {
        let conn = setup();
        let err = wait_for_workflow_run(&conn, "nope", &opts()).unwrap_err();
        assert!(matches!(err, ConductorError::WorkflowRunNotFound { .. }));
    }

    #[test]
    fn mixed_timestamp_formats_compare_correctly() {
        // lease_until is `YYYY-MM-DD HH:MM:SS`, started_at is RFC 3339 on the same day (#3076).
        let conn = setup();
        insert_run(
            &conn,
            "r",
            None,
            "running",
            Some("+20 seconds"),
            "-30 seconds",
        );
        let (age, _) = run_tree_liveness(&conn, "r").unwrap();
        // Newest signal is the future lease, so age is <= 0, never ~a day.
        assert!(age.unwrap() <= 0, "age was {age:?}");
        assert!(eval(&conn, "r").is_none());
    }

    #[test]
    fn last_step_is_latest_started_with_context() {
        let conn = setup();
        insert_run(&conn, "r", None, "failed", None, "-1 hours");
        insert_step(
            &conn,
            "s1",
            "r",
            "first",
            "-50 minutes",
            Some("-49 minutes"),
        );
        insert_step(
            &conn,
            "s2",
            "r",
            "second",
            "-40 minutes",
            Some("-39 minutes"),
        );
        let s = eval(&conn, "r").unwrap();
        let step = s.last_step.unwrap();
        assert_eq!(step.step_name, "second");
        assert_eq!(step.context_out.as_deref(), Some("ctx"));
        let text = format_wait_summary(&WaitSummary {
            last_step: Some(step),
            ..s
        });
        assert!(text.starts_with("outcome: failed"));
        assert!(text.contains("second"));
    }

    #[test]
    fn failed_last_step_populates_step_error() {
        let conn = setup();
        insert_run(&conn, "r", None, "failed", None, "-1 hours");
        insert_step(&conn, "s", "r", "boom", "-50 minutes", Some("-49 minutes"));
        set_step(&conn, "s", "failed", 0, Some("it broke"));
        let step = eval(&conn, "r").unwrap().last_step.unwrap();
        assert_eq!(step.status, "failed");
        assert_eq!(step.step_error.as_deref(), Some("it broke"));
    }

    #[test]
    fn non_failed_step_has_no_step_error() {
        let conn = setup();
        insert_run(&conn, "r", None, "failed", None, "-1 hours");
        insert_step(&conn, "s", "r", "ok", "-50 minutes", Some("-49 minutes"));
        set_step(&conn, "s", "completed", 0, Some("fine"));
        assert!(eval(&conn, "r")
            .unwrap()
            .last_step
            .unwrap()
            .step_error
            .is_none());
    }

    #[test]
    fn run_without_steps_has_no_last_step() {
        let conn = setup();
        insert_run(&conn, "r", None, "failed", None, "-1 hours");
        assert!(eval(&conn, "r").unwrap().last_step.is_none());
    }

    #[test]
    fn last_step_ties_break_by_position() {
        let conn = setup();
        insert_run(&conn, "r", None, "failed", None, "-1 hours");
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, started_at) \
             VALUES ('a', 'r', 'low', 'actor', 1, 'completed', '2026-01-01T00:00:00Z'), \
                    ('b', 'r', 'high', 'actor', 2, 'completed', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let step = eval(&conn, "r").unwrap().last_step.unwrap();
        assert_eq!(step.step_name, "high");
    }

    fn summary(outcome: WaitOutcome) -> WaitSummary {
        WaitSummary {
            run_id: "run1".into(),
            workflow_name: "wf".into(),
            outcome,
            status: "running".into(),
            error: None,
            elapsed_secs: 5,
            last_signal_age_secs: Some(123),
            last_step: None,
        }
    }

    #[test]
    fn format_includes_error_and_step_error_lines() {
        let text = format_wait_summary(&WaitSummary {
            error: Some("engine died".into()),
            last_step: Some(WaitStepSummary {
                step_name: "build".into(),
                status: "failed".into(),
                iteration: 2,
                step_error: Some("exit 1".into()),
                context_out: Some("partial".into()),
                started_at: None,
                ended_at: None,
            }),
            ..summary(WaitOutcome::Failed)
        });
        assert!(text.contains("error: engine died\n"));
        assert!(text.contains("last step: build [failed] (iteration 2)"));
        assert!(text.contains("  step error: exit 1\n"));
        assert!(text.contains("  context: partial\n"));
        assert!(!text.contains("hint:"));
    }

    #[test]
    fn format_stalled_hint_has_resume_command() {
        let text = format_wait_summary(&summary(WaitOutcome::Stalled));
        assert!(text.starts_with("outcome: stalled"));
        assert!(text.contains("for 123s"));
        assert!(text.contains("conductor workflow resume run1"));
    }

    #[test]
    fn format_timeout_hint() {
        let text = format_wait_summary(&summary(WaitOutcome::Timeout));
        assert!(text.starts_with("outcome: timeout"));
        assert!(text.contains("hint: run is still in progress"));
    }

    #[test]
    fn with_overrides_applies_or_falls_back() {
        let d = WaitOptions::default();
        let o = WaitOptions::with_overrides(None, None);
        assert_eq!(o.timeout, None);
        assert_eq!(o.stall_after, d.stall_after);
        let o =
            WaitOptions::with_overrides(Some(Duration::from_secs(7)), Some(Duration::from_secs(9)));
        assert_eq!(o.timeout, Some(Duration::from_secs(7)));
        assert_eq!(o.stall_after, Duration::from_secs(9));
        assert_eq!(o.poll_interval, d.poll_interval);
    }

    #[test]
    fn descendant_count_excludes_root() {
        let conn = setup();
        insert_run(
            &conn,
            "root",
            None,
            "running",
            Some("+20 seconds"),
            "-1 hours",
        );
        assert_eq!(run_tree_liveness(&conn, "root").unwrap().1, 0);
        insert_run(
            &conn,
            "kid",
            Some("root"),
            "running",
            Some("+20 seconds"),
            "-1 hours",
        );
        assert_eq!(run_tree_liveness(&conn, "root").unwrap().1, 1);
    }
}
