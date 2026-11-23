//! Smoke mode for `conductor workflow validate --dry-run` (#3292).
//!
//! A smoke run is a recorded workflow run executed as an engine dry run: no
//! script step is ever executed (runkon skips them in a dry run), gates are
//! auto-approved, and the run stops, cancelled, at the first agent (`call`)
//! step. It starts the workflow the way a normal run does, so registries,
//! resolvers, input defaults and the engine's own start-of-run validation
//! (unknown gate types, executors, item providers) all run, but no agent
//! executor is ever built, so no model call is possible and nothing with side
//! effects runs. The gate resolver wrapper below only matters if the engine
//! ever polls a gate in a dry run.
//!
//! A smoke that stops on purpose ends with no `failed` run row. The root is set
//! `cancelled`, and so is every descendant run the engine left `failed`: runkon-flow's
//! parallel executor counts the stop as a branch failure, so a child run holding a
//! `parallel` block (e.g. `review-pr`) would otherwise be recorded as failed.
//!
//! Input defaults are filled only by the shared start path (the caller must not
//! pre-fill them), so a launcher that skips the fill fails the smoke through the
//! run-start check. After the run, each started run's script steps are scanned
//! for an `env` value that uses a declared input with no value; those come back
//! as [`SmokeOutcome::env_warnings`] and don't change the result (#3454).
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

/// Start of every smoke cancellation reason (see [`SmokeStopPoint::reason`]).
///
/// `notify::transitions` matches this prefix on an agent run's result text to
/// recognise a cancelled smoke driver and skip `agent_run.failed`. The coupling
/// is pinned by `reason_starts_with_notify_prefix_for_every_kind`.
pub const SMOKE_REASON_PREFIX: &str = "smoke: reached ";

impl SmokeStopPoint {
    /// The cancellation reason recorded on the run for this stop.
    pub fn reason(&self) -> String {
        format!("{SMOKE_REASON_PREFIX}{} step '{}'", self.kind, self.step)
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
/// so engine validation is unchanged. A smoke run is an engine dry run, which
/// auto-approves gates without polling, so this is a safeguard: if the engine
/// does poll, the first poll records the stop and cancels the run (once), and
/// every poll answers `Pending`, never approving anything.
pub(crate) struct SmokeStopGateResolver<R> {
    inner: R,
    smoke: SmokeStop,
    db_path: std::path::PathBuf,
    /// Set after the first poll cancels the run, so later polls (if the engine
    /// polls again before it sees the cancellation) don't reopen the database.
    cancelled: std::sync::atomic::AtomicBool,
}

impl<R: GateResolver> SmokeStopGateResolver<R> {
    pub(crate) fn new(inner: R, smoke: SmokeStop, db_path: std::path::PathBuf) -> Self {
        Self {
            inner,
            smoke,
            db_path,
            cancelled: std::sync::atomic::AtomicBool::new(false),
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
        if self
            .cancelled
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(GatePoll::Pending);
        }
        let point = self
            .smoke
            .record_at(ctx, &params.gate_name, SmokeStopKind::Gate);
        let reason = point.reason();
        // A failed cancel must not leave the run waiting on the gate: fail
        // the step instead, so the smoke result reports it.
        let conn = crate::db::open_database(&self.db_path).map_err(|e| {
            EngineError::Workflow(format!(
                "smoke: failed to open the database to cancel run {run_id} at gate '{}': {e}",
                params.gate_name
            ))
        })?;
        crate::workflow::cancel_run(&conn, run_id, &reason).map_err(|e| {
            EngineError::Workflow(format!(
                "smoke: failed to cancel run {run_id} at gate '{}': {e}",
                params.gate_name
            ))
        })?;
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
    /// Script steps whose `env` uses a declared input that has no value.
    pub env_warnings: Vec<UnresolvedEnvInput>,
    /// Reasons the env scan could not complete (database or snapshot errors).
    /// Non-empty means `env_warnings` may be incomplete, not that it is clean.
    pub env_scan_errors: Vec<String>,
}

/// A script step's `env` value that names a declared input with no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedEnvInput {
    /// The workflow that owns the step (a child workflow's name for a child).
    pub workflow: String,
    pub step: String,
    pub env_var: String,
    pub input: String,
}

impl UnresolvedEnvInput {
    pub fn describe(&self) -> String {
        format!(
            "script step '{}' env {} uses input '{}', which has no value",
            self.step, self.env_var, self.input
        )
    }
}

/// Script `env` placeholders of `workflow` that name a declared input missing
/// from `inputs`. Pure: no DB access.
pub(crate) fn unresolved_env_inputs(
    workflow: &crate::workflow::WorkflowDef,
    inputs: &std::collections::HashMap<String, String>,
) -> Vec<UnresolvedEnvInput> {
    use crate::workflow::batch_validate::script_nodes;
    use crate::workflow::placeholder_scan::placeholder_spans;

    let mut out = Vec::new();
    for node in script_nodes(workflow) {
        let mut vars: Vec<_> = node.env.iter().collect();
        vars.sort();
        for (var, value) in vars {
            for span in placeholder_spans(value) {
                let name = span.key.trim();
                let declared = workflow.inputs.iter().any(|d| d.name == name);
                let entry = UnresolvedEnvInput {
                    workflow: workflow.name.clone(),
                    step: node.name.clone(),
                    env_var: var.clone(),
                    input: name.to_string(),
                };
                if declared && !inputs.contains_key(name) && !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
    }
    out
}

/// Scan a run and its descendants (depth-capped) with [`unresolved_env_inputs`],
/// using each run's definition snapshot and stored inputs. Failures are
/// recorded in `errors` so an incomplete scan never looks like a clean one.
fn scan_env_inputs(
    conn: &rusqlite::Connection,
    run_id: &str,
    depth: u32,
    out: &mut Vec<UnresolvedEnvInput>,
    errors: &mut Vec<String>,
) {
    if depth > 16 {
        return;
    }
    match crate::workflow::get_workflow_run(conn, run_id) {
        Ok(Some(run)) => {
            if let Some(snapshot) = run.definition_snapshot.as_deref() {
                match serde_json::from_str::<crate::workflow::WorkflowDef>(snapshot) {
                    Ok(def) => {
                        for w in unresolved_env_inputs(&def, &run.inputs) {
                            if !out.contains(&w) {
                                out.push(w);
                            }
                        }
                    }
                    Err(e) => errors.push(format!(
                        "run {run_id}: definition snapshot did not parse: {e}"
                    )),
                }
            }
        }
        Ok(None) => errors.push(format!("run {run_id}: run row not found")),
        Err(e) => errors.push(format!("run {run_id}: could not read the run: {e}")),
    }
    match crate::workflow::list_child_workflow_runs(conn, run_id) {
        Ok(children) => {
            for child in children {
                scan_env_inputs(conn, &child.id, depth + 1, out, errors);
            }
        }
        Err(e) => errors.push(format!("run {run_id}: could not list child runs: {e}")),
    }
}

impl SmokeOutcome {
    pub fn passed(&self) -> bool {
        matches!(self.result, SmokeResult::Passed { .. })
    }
}

/// The deepest failed step of a run: a failed child run's step wins over the
/// parent's `call workflow` step that merely reports it.
pub(crate) fn find_failed_step(
    conn: &rusqlite::Connection,
    run_id: &str,
    depth: u32,
) -> Option<String> {
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

/// End the agent run driving a smoke run as `cancelled` with the smoke reason.
/// The one place the rule lives; callers decide whether a failure is fatal.
pub(super) fn cancel_driver(
    agent_mgr: &crate::agent::AgentManager,
    driver_id: &str,
    reason: &str,
) -> std::result::Result<(), String> {
    agent_mgr
        .update_run_cancelled_with_reason(driver_id, reason)
        .map_err(|e| format!("failed to cancel driving agent run {driver_id}: {e}"))
}

/// Make the smoke run's tree end the way a passing smoke must: the agent run
/// driving `run_id` and each descendant's driver end `cancelled` with the smoke
/// reason, and every descendant run that isn't `completed` or `cancelled` does too.
///
/// The engine's parallel executor counts a branch's smoke stop as a branch failure,
/// and the fail-fast path then records the child run `failed` (runkon-flow logs
/// "finished with failures" for it). A passing smoke must leave no `failed` run or
/// agent run. A `completed` run or driver ran to the end before the stop and stays
/// as it is. Step rows are left as the engine wrote them. `run_id` itself is handled
/// by `ensure_cancelled`; only its driver is finalized here.
fn cancel_smoke_descendants(
    conn: &rusqlite::Connection,
    run_id: &str,
    reason: &str,
    depth: u32,
) -> std::result::Result<(), String> {
    if depth > 16 {
        return Ok(());
    }
    let driver: Option<String> = conn
        .query_row(
            "SELECT parent_run_id FROM workflow_runs WHERE id = ?1",
            [run_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("failed to read driver of smoke run {run_id}: {e}"))?;
    if let Some(driver) = driver.filter(|d| !d.is_empty()) {
        cancel_driver(&crate::agent::AgentManager::new(conn), &driver, reason)?;
    }
    let children = crate::workflow::list_child_workflow_runs(conn, run_id)
        .map_err(|e| format!("failed to list child runs of smoke run {run_id}: {e}"))?;
    for child in children {
        if !matches!(
            child.status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Cancelled
        ) {
            ensure_cancelled(conn, &child.id, reason)?;
        }
        cancel_smoke_descendants(conn, &child.id, reason, depth + 1)?;
    }
    Ok(())
}

fn db_open_error(path: &std::path::Path, e: &dyn std::fmt::Display) -> String {
    format!("failed to open database at {}: {e}", path.display())
}

/// Run `params` as a smoke run and classify the outcome.
///
/// Sets `params.smoke`; every other field is used as given. The run is an
/// engine dry run (no script executes, gates auto-approve) and stops at the
/// first agent step.
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
                (Ok(conn), Some(id)) => match ensure_cancelled(conn, id, &point.reason())
                    .and_then(|()| cancel_smoke_descendants(conn, id, &point.reason(), 0))
                {
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
    let mut env_warnings = Vec::new();
    let mut env_scan_errors = Vec::new();
    match (conn.as_ref(), run_id.as_deref()) {
        (Ok(conn), Some(id)) => {
            scan_env_inputs(conn, id, 0, &mut env_warnings, &mut env_scan_errors)
        }
        (Err(e), Some(_)) => env_scan_errors.push(db_open_error(&db_path, e)),
        // No run row was created (the run failed before start): nothing to scan.
        (_, None) => {}
    }
    SmokeOutcome {
        run_id,
        result,
        env_warnings,
        env_scan_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runkon_flow::dsl::{Condition, IfNode, ScriptNode, WorkflowNode};

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

    /// `notify` recognises a cancelled smoke driver by this prefix on the agent
    /// run's result text, so every stop kind's reason must carry it.
    #[test]
    fn reason_starts_with_notify_prefix_for_every_kind() {
        for kind in [SmokeStopKind::Agent, SmokeStopKind::Gate] {
            let reason = point("s", kind).reason();
            assert!(
                reason.starts_with(SMOKE_REASON_PREFIX),
                "reason {reason:?} must start with {SMOKE_REASON_PREFIX:?}"
            );
        }
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

    use crate::workflow::tests::run_status_of as status_of;

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

    fn driver_of(conn: &rusqlite::Connection, run_id: &str) -> (String, Option<String>) {
        conn.query_row(
            "SELECT ar.status, ar.result_text FROM workflow_runs wr \
             JOIN agent_runs ar ON ar.id = wr.parent_run_id WHERE wr.id = ?1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn driver_id(conn: &rusqlite::Connection, run_id: &str) -> String {
        conn.query_row(
            "SELECT parent_run_id FROM workflow_runs WHERE id = ?1",
            [run_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn cancel_smoke_descendants_turns_a_failed_driver_cancelled() {
        let conn = crate::test_helpers::setup_db();
        let id = new_run(&conn);
        crate::agent::AgentManager::new(&conn)
            .update_run_failed(&driver_id(&conn, &id), "Workflow 'wf' failed")
            .unwrap();
        cancel_smoke_descendants(&conn, &id, "smoke: reached agent step 'a'", 0).unwrap();
        assert_eq!(
            driver_of(&conn, &id),
            (
                "cancelled".to_string(),
                Some("smoke: reached agent step 'a'".to_string())
            )
        );
    }

    #[test]
    fn cancel_smoke_descendants_leaves_a_completed_driver() {
        let conn = crate::test_helpers::setup_db();
        let id = new_run(&conn);
        conn.execute(
            "UPDATE agent_runs SET status = 'completed' WHERE id = ?1",
            [driver_id(&conn, &id)],
        )
        .unwrap();
        cancel_smoke_descendants(&conn, &id, "smoke", 0).unwrap();
        assert_eq!(driver_of(&conn, &id).0, "completed");
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

    fn env_wf(
        inputs: Vec<(&str, Option<&str>)>,
        nodes: Vec<WorkflowNode>,
    ) -> crate::workflow::WorkflowDef {
        use runkon_flow::dsl::{InputDecl, InputType, WorkflowTrigger};
        crate::workflow::WorkflowDef {
            name: "wf".to_string(),
            title: None,
            description: String::new(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            group: None,
            inputs: inputs
                .into_iter()
                .map(|(n, d)| InputDecl {
                    name: n.to_string(),
                    required: false,
                    input_type: InputType::String,
                    default: d.map(str::to_string),
                    description: None,
                })
                .collect(),
            body: nodes,
            always: vec![],
            source_path: String::new(),
            schedule: None,
        }
    }

    fn script(env: &[(&str, &str)]) -> ScriptNode {
        ScriptNode {
            name: "s".to_string(),
            run: "x.sh".to_string(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            timeout: None,
            retries: 0,
            on_fail: None,
            as_identity: None,
        }
    }

    #[test]
    fn env_scan_reports_only_declared_inputs_without_a_value() {
        let wf = env_wf(
            vec![("opt", None), ("dflt", Some("v"))],
            vec![WorkflowNode::Script(script(&[
                ("A", "{{opt}}"),
                ("B", "{{dflt}}"),
                ("C", "{{pr_number}}"),
            ]))],
        );
        let inputs = std::collections::HashMap::from([("dflt".to_string(), "x".to_string())]);
        let got = unresolved_env_inputs(&wf, &inputs);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].input, "opt");
        assert_eq!(got[0].env_var, "A");
        assert!(got[0].describe().contains("'opt'"));
    }

    #[test]
    fn env_scan_walks_nested_script_steps() {
        let inner = WorkflowNode::Script(script(&[("A", "{{opt}}")]));
        let wf = env_wf(
            vec![("opt", None)],
            vec![WorkflowNode::If(IfNode {
                condition: Condition::BoolInput {
                    input: "opt".to_string(),
                },
                body: vec![inner],
            })],
        );
        let got = unresolved_env_inputs(&wf, &std::collections::HashMap::new());
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn scan_reports_a_stored_run_missing_a_declared_input() {
        let conn = crate::test_helpers::setup_db();
        let wf = env_wf(
            vec![("opt", None)],
            vec![WorkflowNode::Script(script(&[("A", "{{opt}}")]))],
        );
        let snapshot = serde_json::to_string(&wf).unwrap();
        let parent = crate::agent::AgentManager::new(&conn)
            .create_run(Some("w1"), "workflow", None)
            .unwrap();
        let run = crate::workflow::create_workflow_run(
            &conn,
            "wf",
            Some("w1"),
            &parent.id,
            false,
            "manual",
            Some(&snapshot),
        )
        .unwrap();
        let mut out = Vec::new();
        let mut errors = Vec::new();
        scan_env_inputs(&conn, &run.id, 0, &mut out, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            out[0].describe(),
            "script step 's' env A uses input 'opt', which has no value"
        );
    }

    #[test]
    fn scan_errors_are_reported_not_swallowed() {
        let conn = crate::test_helpers::create_test_conn();
        let mut out = Vec::new();
        let mut errors = Vec::new();
        scan_env_inputs(&conn, "no-such-run", 0, &mut out, &mut errors);
        assert!(out.is_empty());
        assert!(
            errors.iter().any(|e| e.contains("no-such-run")),
            "{errors:?}"
        );
    }
}
