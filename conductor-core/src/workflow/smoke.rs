//! Smoke mode for `conductor workflow validate --dry-run` (#3292).
//!
//! A smoke run is a real, recorded workflow run that executes script steps and
//! stops, cancelled, at the first agent (`call`) or gate step. It starts the
//! workflow the way a normal run does, so registries, resolvers, input
//! defaults and the engine's own start-of-run validation all run, but no agent
//! executor is ever built, so no model call is possible.
//!
//! The pieces:
//! - [`SmokeStop`]: shared handle that records the first stop point.
//! - [`SmokeStopActionExecutor`]: the action registry's catch-all in smoke mode.
//! - [`SmokeStopGateResolver`]: wraps each gate resolver in smoke mode.
//! - [`smoke_run_workflow`]: runs a workflow in smoke mode and classifies the outcome.

use std::sync::{Arc, Mutex};

use runkon_flow::cancellation_reason::CancellationReason;
use runkon_flow::engine_error::EngineError;
use runkon_flow::traits::action_executor::{ActionExecutor, ActionOutput, ActionParams, StepInfo};
use runkon_flow::traits::gate_resolver::{GateParams, GatePoll, GateResolver};
use runkon_flow::traits::run_context::RunContext;

use crate::error::ConductorError;
use crate::workflow::types::{RunIdSlot, WorkflowExecStandalone};
use crate::workflow::WorkflowRunStatus;

/// What kind of step a smoke run stopped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmokeStopKind {
    Agent,
    Gate,
}

impl std::fmt::Display for SmokeStopKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmokeStopKind::Agent => f.write_str("agent"),
            SmokeStopKind::Gate => f.write_str("gate"),
        }
    }
}

/// Where a smoke run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeStopPoint {
    /// The workflow that owns the step (a child workflow's name for a child).
    pub workflow: String,
    /// The agent name or gate step name.
    pub step: String,
    pub kind: SmokeStopKind,
}

/// Shared handle that records where a smoke run stopped. Cloning shares the
/// recording, so the executor, the gate resolvers and the child-workflow runner
/// all report into the same slot. Only the first stop is kept.
#[derive(Debug, Clone, Default)]
pub struct SmokeStop {
    point: Arc<Mutex<Option<SmokeStopPoint>>>,
}

impl SmokeStop {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record(&self, point: SmokeStopPoint) {
        let mut slot = self.point.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(point);
        }
    }

    /// Build the stop point for `step` in the running workflow, record it
    /// (first one wins) and return it.
    pub(crate) fn record_at(
        &self,
        ctx: &dyn RunContext,
        step: &str,
        kind: SmokeStopKind,
    ) -> SmokeStopPoint {
        let point = SmokeStopPoint {
            workflow: ctx.workflow_name().to_string(),
            step: step.to_string(),
            kind,
        };
        self.record(point.clone());
        point
    }

    /// The first stop point recorded, if any.
    pub fn stopped_at(&self) -> Option<SmokeStopPoint> {
        self.point.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl SmokeStopPoint {
    /// The cancellation reason recorded on the run for this stop.
    pub fn reason(&self) -> String {
        format!("smoke: reached {} step '{}'", self.kind, self.step)
    }
}

fn smoke_cancellation(point: &SmokeStopPoint) -> EngineError {
    EngineError::Cancelled(CancellationReason::UserRequested(Some(point.reason())))
}

/// The catch-all action executor of a smoke run. Every `call` step lands here;
/// it records the step and cancels the run without spawning anything.
pub(crate) struct SmokeStopActionExecutor {
    smoke: SmokeStop,
}

impl SmokeStopActionExecutor {
    pub(crate) fn new(smoke: SmokeStop) -> Self {
        Self { smoke }
    }
}

impl ActionExecutor for SmokeStopActionExecutor {
    fn name(&self) -> &str {
        "__smoke_stop__"
    }

    fn execute(
        &self,
        ctx: &dyn RunContext,
        _info: &StepInfo,
        params: &ActionParams,
    ) -> Result<ActionOutput, EngineError> {
        let point = self
            .smoke
            .record_at(ctx, &params.name, SmokeStopKind::Agent);
        Err(smoke_cancellation(&point))
    }
}

/// Wraps a real gate resolver for a smoke run. It reports the same gate type,
/// so engine validation is unchanged, but polling it never approves anything:
/// it records the stop, marks the run cancelled and answers `Pending`, so the
/// engine's gate loop sees the cancellation and ends the run.
pub(crate) struct SmokeStopGateResolver<R> {
    inner: R,
    smoke: SmokeStop,
    db_path: std::path::PathBuf,
}

impl<R: GateResolver> SmokeStopGateResolver<R> {
    pub(crate) fn new(inner: R, smoke: SmokeStop, db_path: std::path::PathBuf) -> Self {
        Self {
            inner,
            smoke,
            db_path,
        }
    }
}

impl<R: GateResolver> GateResolver for SmokeStopGateResolver<R> {
    fn gate_type(&self) -> &str {
        self.inner.gate_type()
    }

    fn poll(
        &self,
        run_id: &str,
        params: &GateParams,
        ctx: &dyn RunContext,
    ) -> Result<GatePoll, EngineError> {
        let point = self
            .smoke
            .record_at(ctx, &params.gate_name, SmokeStopKind::Gate);
        let reason = point.reason();
        match crate::db::open_database(&self.db_path) {
            Ok(conn) => {
                if let Err(e) = crate::workflow::cancel_run(&conn, run_id, &reason) {
                    tracing::warn!("smoke: failed to cancel run {run_id} at a gate: {e}");
                }
            }
            Err(e) => tracing::warn!("smoke: failed to open the database to cancel {run_id}: {e}"),
        }
        Ok(GatePoll::Pending)
    }

    fn poll_interval(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_millis(10))
    }
}

/// How a smoke run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmokeResult {
    /// The run reached an agent or gate step and was cancelled there
    /// (`Some`), or ran to completion without one (`None`).
    Passed { stopped_at: Option<SmokeStopPoint> },
    /// The run failed before reaching an agent or gate step.
    Failed {
        /// The step that failed, when one could be found.
        step: Option<String>,
        error: String,
    },
}

/// The outcome of [`smoke_run_workflow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeOutcome {
    /// The recorded run's ID, once the run row was created.
    pub run_id: Option<String>,
    pub result: SmokeResult,
}

impl SmokeOutcome {
    pub fn passed(&self) -> bool {
        matches!(self.result, SmokeResult::Passed { .. })
    }
}

/// The deepest failed step of a run: a failed child run's step wins over the
/// parent's `call workflow` step that merely reports it.
fn find_failed_step(conn: &rusqlite::Connection, run_id: &str, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    if let Ok(children) = crate::workflow::list_child_workflow_runs(conn, run_id) {
        for child in children.iter().rev() {
            if child.status == WorkflowRunStatus::Failed {
                if let Some(found) = find_failed_step(conn, &child.id, depth + 1) {
                    return Some(format!("{} (in {})", found, child.workflow_name));
                }
            }
        }
    }
    crate::workflow::manager::queries::get_last_failed_step(conn, run_id)
        .ok()
        .flatten()
        .map(|s| s.step_name)
}

/// Make sure a smoke run that stopped on purpose ends `cancelled`, whatever
/// state the engine left it in.
fn ensure_cancelled(
    conn: &rusqlite::Connection,
    run_id: &str,
    reason: &str,
) -> std::result::Result<(), String> {
    let status = crate::workflow::get_workflow_run_status(conn, run_id)
        .ok()
        .flatten();
    if status.as_deref() == Some("cancelled") {
        return Ok(());
    }
    if let Err(e1) = crate::workflow::cancel_run(conn, run_id, reason) {
        // Terminal already (the engine marked it failed): record the stop anyway.
        crate::workflow::update_workflow_status(
            conn,
            run_id,
            WorkflowRunStatus::Cancelled,
            Some(reason),
            None,
        )
        .map_err(|e2| {
            format!(
                "failed to cancel smoke run {run_id}: {e1}; fallback status update also failed: {e2}"
            )
        })?;
    }
    Ok(())
}

fn db_open_error(path: &std::path::Path, e: &dyn std::fmt::Display) -> String {
    format!("failed to open database at {}: {e}", path.display())
}

/// Run `params` as a smoke run and classify the outcome.
///
/// Sets `params.smoke`; every other field is used as given. The run executes
/// script steps for real and stops at the first agent or gate step.
pub fn smoke_run_workflow(mut params: WorkflowExecStandalone) -> SmokeOutcome {
    let stop = SmokeStop::new();
    params.smoke = Some(stop.clone());
    let slot: RunIdSlot = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    params.run_id_notify = Some(Arc::clone(&slot));
    let db_path = params
        .db_path
        .clone()
        .unwrap_or_else(crate::config::db_path);
    let workflow_name = params.workflow.name.clone();

    let run = crate::workflow::execute_workflow_standalone(&params);
    let run_id = slot.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stopped_at = stop.stopped_at();

    let conn = crate::db::open_database(&db_path);
    // A failure outside a smoke stop: look up the failing step, and when the
    // database can't be opened say so instead of silently dropping the step.
    let fail = |error: String| match (&conn, run_id.as_deref()) {
        (Ok(conn), Some(id)) => SmokeResult::Failed {
            step: find_failed_step(conn, id, 0),
            error,
        },
        (Ok(_), None) => SmokeResult::Failed { step: None, error },
        (Err(e), _) => SmokeResult::Failed {
            step: None,
            error: format!(
                "{error}; failing step lookup skipped: {}",
                db_open_error(&db_path, e)
            ),
        },
    };

    let result = match (run, stopped_at) {
        (Ok(_) | Err(ConductorError::WorkflowCancelled), Some(point)) => {
            match (conn.as_ref(), run_id.as_deref()) {
                (Ok(conn), Some(id)) => match ensure_cancelled(conn, id, &point.reason()) {
                    Ok(()) => SmokeResult::Passed {
                        stopped_at: Some(point),
                    },
                    Err(error) => SmokeResult::Failed {
                        step: Some(point.step.clone()),
                        error,
                    },
                },
                (Err(e), _) => SmokeResult::Failed {
                    step: Some(point.step.clone()),
                    error: db_open_error(&db_path, e),
                },
                (Ok(_), None) => SmokeResult::Passed {
                    stopped_at: Some(point),
                },
            }
        }
        (Ok(result), None) if result.all_succeeded => SmokeResult::Passed { stopped_at: None },
        (Ok(_), None) => fail(format!("workflow '{workflow_name}' did not succeed")),
        (Err(ConductorError::WorkflowCancelled), None) => fail(format!(
            "workflow '{workflow_name}' was cancelled before an agent or gate step"
        )),
        (Err(e), _) => fail(e.to_string()),
    };
    SmokeOutcome { run_id, result }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(step: &str, kind: SmokeStopKind) -> SmokeStopPoint {
        SmokeStopPoint {
            workflow: "wf".into(),
            step: step.into(),
            kind,
        }
    }

    #[test]
    fn smoke_stop_keeps_only_the_first_point() {
        let stop = SmokeStop::new();
        assert!(stop.stopped_at().is_none());
        stop.record(point("a", SmokeStopKind::Agent));
        stop.clone().record(point("b", SmokeStopKind::Gate));
        assert_eq!(stop.stopped_at(), Some(point("a", SmokeStopKind::Agent)));
    }

    #[test]
    fn stop_kind_displays_lowercase() {
        assert_eq!(SmokeStopKind::Agent.to_string(), "agent");
        assert_eq!(SmokeStopKind::Gate.to_string(), "gate");
    }

    #[test]
    fn reason_names_kind_and_step() {
        assert_eq!(
            point("x", SmokeStopKind::Agent).reason(),
            "smoke: reached agent step 'x'"
        );
        assert_eq!(
            point("g", SmokeStopKind::Gate).reason(),
            "smoke: reached gate step 'g'"
        );
    }

    fn new_run(conn: &rusqlite::Connection) -> String {
        let parent = crate::agent::AgentManager::new(conn)
            .create_run(Some("w1"), "workflow", None)
            .unwrap();
        crate::workflow::create_workflow_run(
            conn,
            "wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            None,
        )
        .unwrap()
        .id
        .clone()
    }

    fn status_of(conn: &rusqlite::Connection, id: &str) -> Option<String> {
        crate::workflow::get_workflow_run_status(conn, id).unwrap()
    }

    #[test]
    fn ensure_cancelled_returns_ok_when_already_cancelled() {
        let conn = crate::test_helpers::setup_db();
        let id = new_run(&conn);
        crate::workflow::update_workflow_status(
            &conn,
            &id,
            WorkflowRunStatus::Cancelled,
            Some("earlier"),
            None,
        )
        .unwrap();
        assert!(ensure_cancelled(&conn, &id, "smoke").is_ok());
        assert_eq!(status_of(&conn, &id).as_deref(), Some("cancelled"));
    }

    #[test]
    fn ensure_cancelled_cancels_a_running_run() {
        let conn = crate::test_helpers::setup_db();
        let id = new_run(&conn);
        ensure_cancelled(&conn, &id, "smoke").unwrap();
        assert_eq!(status_of(&conn, &id).as_deref(), Some("cancelled"));
    }

    #[test]
    fn ensure_cancelled_falls_back_for_a_failed_run() {
        let conn = crate::test_helpers::setup_db();
        let id = new_run(&conn);
        crate::workflow::update_workflow_status(
            &conn,
            &id,
            WorkflowRunStatus::Failed,
            None,
            Some("boom"),
        )
        .unwrap();
        ensure_cancelled(&conn, &id, "smoke").unwrap();
        assert_eq!(status_of(&conn, &id).as_deref(), Some("cancelled"));
    }

    #[test]
    fn ensure_cancelled_errors_when_both_updates_fail() {
        let conn = crate::test_helpers::setup_db();
        let id = new_run(&conn);
        conn.execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE workflow_runs;")
            .unwrap();
        let err = ensure_cancelled(&conn, &id, "smoke").unwrap_err();
        assert!(err.contains("failed to cancel smoke run"), "{err}");
    }

    #[test]
    fn cancellation_names_the_step() {
        let err = smoke_cancellation(&point("plan", SmokeStopKind::Agent));
        match err {
            EngineError::Cancelled(CancellationReason::UserRequested(Some(msg))) => {
                assert_eq!(msg, "smoke: reached agent step 'plan'");
            }
            other => panic!("expected a cancellation, got {other:?}"),
        }
    }
}
