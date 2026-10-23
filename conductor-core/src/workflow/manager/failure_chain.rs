//! Read-only walk from a failed run down to the step that actually failed.
//!
//! runkon-flow records `Sub-workflow 'X' failed` on a failed `call workflow` step and
//! stores the child run ID in `child_run_id`. The real cause sits one or more levels
//! down. [`run_failure_chain`] and [`child_failure_chain`] follow `child_run_id`
//! through failed `workflow:` steps and return one [`FailureLink`] per level, outermost
//! first, ending at the leaf failing step.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::error::Result;
use crate::text_util::cap_with_suffix;
use crate::workflow::{WorkflowRun, WorkflowRunStatus, WorkflowRunStep, WorkflowStepStatus};

use super::queries::{get_last_failed_step, get_workflow_run};

/// Maximum number of levels followed before the walk stops.
const MAX_DEPTH: usize = 8;
/// Maximum length in bytes of an error in the one-line forms.
const MAX_ERROR_LEN: usize = 300;
/// Persisted `role` of `call workflow` steps.
const WORKFLOW_ROLE: &str = "workflow";
const NO_ERROR: &str = "(no error recorded)";

/// One level of a failure chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureLink {
    /// The run this step belongs to.
    pub run_id: String,
    pub workflow_name: String,
    pub step_name: String,
    /// Child workflow run for `call workflow` steps that were followed.
    pub child_run_id: Option<String>,
    /// `step_error`, else `result_text`, else `(no error recorded)`; collapsed to one line.
    pub error: String,
}

/// True when the step is a `call workflow` step (its `child_run_id` is a workflow run ID).
pub fn is_workflow_call_step(step: &WorkflowRunStep) -> bool {
    step.role == WORKFLOW_ROLE
}

fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cap_with_suffix(&joined, MAX_ERROR_LEN, "…")
}

fn step_error_text(step: &WorkflowRunStep) -> String {
    step.step_error
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(step.result_text.as_deref().filter(|s| !s.trim().is_empty()))
        .map(one_line)
        .unwrap_or_else(|| NO_ERROR.to_string())
}

fn walk(
    conn: &Connection,
    mut run_id: String,
    chain: &mut Vec<FailureLink>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    while chain.len() < MAX_DEPTH && seen.insert(run_id.clone()) {
        let Some(run) = get_workflow_run(conn, &run_id)? else {
            return Ok(());
        };
        let name = run.workflow_name.clone();
        let Some(step) = get_last_failed_step(conn, &run_id)? else {
            // Cancelled or reaped runs have no failed step; use the run's own error.
            let msg = run
                .error
                .as_deref()
                .or(run.result_summary.as_deref())
                .filter(|s| !s.trim().is_empty());
            if let Some(msg) = msg {
                chain.push(FailureLink {
                    run_id,
                    workflow_name: name,
                    step_name: "(run)".to_string(),
                    child_run_id: None,
                    error: one_line(msg),
                });
            }
            return Ok(());
        };
        let next = step
            .child_run_id
            .clone()
            .filter(|_| is_workflow_call_step(&step));
        chain.push(FailureLink {
            run_id: run_id.clone(),
            workflow_name: name,
            step_name: step.step_name.clone(),
            child_run_id: next.clone(),
            error: step_error_text(&step),
        });
        match next {
            Some(child) => run_id = child,
            None => return Ok(()),
        }
    }
    Ok(())
}

/// Chain starting from a run: its failing step, then down into its child runs.
///
/// Empty when the run doesn't exist or has nothing to report.
pub fn run_failure_chain(conn: &Connection, run_id: &str) -> Result<Vec<FailureLink>> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    walk(conn, run_id.to_string(), &mut chain, &mut seen)?;
    Ok(chain)
}

/// For a failed `call workflow` step, follow `child_run_id` into the child run and return
/// the chain of failing steps below it, outermost first, ending at the leaf.
///
/// Empty for any other kind of step, so an agent run ID is never looked up as a workflow run.
pub fn child_failure_chain(conn: &Connection, step: &WorkflowRunStep) -> Result<Vec<FailureLink>> {
    let mut chain = Vec::new();
    if !is_workflow_call_step(step) {
        return Ok(chain);
    }
    let Some(child) = step.child_run_id.as_ref() else {
        return Ok(chain);
    };
    let mut seen = HashSet::new();
    seen.insert(step.workflow_run_id.clone());
    walk(conn, child.clone(), &mut chain, &mut seen)?;
    Ok(chain)
}

/// Chain below a failed `call workflow` step, reusing `known` (the chain already walked
/// from the step's own run) when its first link is this step, so a render walks each
/// child run once.
///
/// Empty for steps that are not failed `call workflow` steps.
pub fn failed_call_step_chain(
    conn: &Connection,
    step: &WorkflowRunStep,
    known: &[FailureLink],
) -> Result<Vec<FailureLink>> {
    if step.status != WorkflowStepStatus::Failed || !is_workflow_call_step(step) {
        return Ok(Vec::new());
    }
    if let (Some(first), Some(child)) = (known.first(), step.child_run_id.as_deref()) {
        if first.run_id == step.workflow_run_id && first.child_run_id.as_deref() == Some(child) {
            return Ok(known[1..].to_vec());
        }
    }
    child_failure_chain(conn, step)
}

/// Unwrap a failure-chain lookup for display, logging a failure instead of hiding it.
///
/// A broken lookup must not block run-show, run detail, wait or notification output, but
/// it should not look the same as "no nested cause" either.
pub fn chain_or_empty(run_id: &str, chain: Result<Vec<FailureLink>>) -> Vec<FailureLink> {
    chain.unwrap_or_else(|e| {
        tracing::warn!(run_id, error = %e, "failure chain lookup failed");
        Vec::new()
    })
}

/// Chain for a run, empty unless the run is `Failed`. Lookup errors are logged.
pub fn run_chain_if_failed(conn: &Connection, run: &WorkflowRun) -> Vec<FailureLink> {
    if run.status != WorkflowRunStatus::Failed {
        return Vec::new();
    }
    chain_or_empty(&run.id, run_failure_chain(conn, &run.id))
}

/// One-line cause of a failed run, or `None` when the chain is empty. Lookup errors are
/// logged and yield `None`.
pub fn run_failure_cause(conn: &Connection, run_id: &str) -> Option<String> {
    format_failure_cause(&chain_or_empty(run_id, run_failure_chain(conn, run_id)))
}

/// Indented tree of the failure chain below a step (see [`failed_call_step_chain`]),
/// empty for steps that are not failed `call workflow` steps. Lookup errors are logged.
pub fn render_step_failure_tree(
    conn: &Connection,
    step: &WorkflowRunStep,
    known: &[FailureLink],
    indent: usize,
) -> String {
    let below = chain_or_empty(
        &step.workflow_run_id,
        failed_call_step_chain(conn, step, known),
    );
    render_failure_tree(&below, indent)
}

/// Render a chain as an indented tree, one line per level, each ending in a newline.
///
/// Levels that were followed into a child run show `(child <id>)`; the leaf shows its error.
pub fn render_failure_tree(chain: &[FailureLink], indent: usize) -> String {
    let mut out = String::new();
    for (depth, link) in chain.iter().enumerate() {
        let pad = " ".repeat(indent + depth * 2);
        match &link.child_run_id {
            Some(child) => out.push_str(&format!(
                "{pad}└ [FAIL] {} (child {child})\n",
                link.step_name
            )),
            None => out.push_str(&format!(
                "{pad}└ [FAIL] {}: {}\n",
                link.step_name, link.error
            )),
        }
    }
    out
}

/// One-line rendering: `submit-review: exited 1: …`, or `review-pr › submit-review: …`
/// for multi-level chains (intermediate levels shown by step name).
pub fn format_failure_cause(chain: &[FailureLink]) -> Option<String> {
    let (leaf, rest) = chain.split_last()?;
    let mut parts: Vec<&str> = rest.iter().map(|l| l.step_name.as_str()).collect();
    parts.push(leaf.step_name.as_str());
    Some(format!("{}: {}", parts.join(" › "), leaf.error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::get_workflow_steps;

    fn setup() -> Connection {
        let conn = crate::test_helpers::setup_db();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn
    }

    fn insert_run(conn: &Connection, id: &str, name: &str, status: &str, error: Option<&str>) {
        crate::test_helpers::insert_test_wf_run(conn, id, name, status, error);
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_step(
        conn: &Connection,
        id: &str,
        run: &str,
        name: &str,
        role: &str,
        position: i64,
        started: &str,
        child: Option<&str>,
        error: Option<&str>,
    ) {
        crate::test_helpers::insert_test_wf_step(
            conn, id, run, name, role, "failed", position, started, child, error,
        );
    }

    fn top_step(conn: &Connection, run: &str) -> WorkflowRunStep {
        get_workflow_steps(conn, run).unwrap().remove(0)
    }

    #[test]
    fn walks_three_levels_to_leaf() {
        let conn = setup();
        insert_run(&conn, "r1", "iterate-pr", "failed", None);
        insert_run(&conn, "r2", "review-pr", "failed", None);
        insert_run(&conn, "r3", "submit", "failed", None);
        insert_step(
            &conn,
            "s1",
            "r1",
            "workflow:review-pr",
            "workflow",
            0,
            "2026-01-01T00:00:01Z",
            Some("r2"),
            Some("Sub-workflow 'review-pr' failed"),
        );
        insert_step(
            &conn,
            "s2",
            "r2",
            "workflow:submit",
            "workflow",
            0,
            "2026-01-01T00:00:02Z",
            Some("r3"),
            Some("Sub-workflow 'submit' failed"),
        );
        insert_step(
            &conn,
            "s3",
            "r3",
            "submit-review",
            "script",
            0,
            "2026-01-01T00:00:03Z",
            None,
            Some("exited 1: mktemp: File exists"),
        );

        let chain = run_failure_chain(&conn, "r1").unwrap();
        let names: Vec<_> = chain.iter().map(|l| l.step_name.as_str()).collect();
        assert_eq!(
            names,
            ["workflow:review-pr", "workflow:submit", "submit-review"]
        );
        assert_eq!(chain[0].child_run_id.as_deref(), Some("r2"));
        assert_eq!(chain[2].error, "exited 1: mktemp: File exists");

        let below = child_failure_chain(&conn, &top_step(&conn, "r1")).unwrap();
        assert_eq!(below.len(), 2);
        assert_eq!(below[0].workflow_name, "review-pr");
        assert_eq!(
            format_failure_cause(&below).unwrap(),
            "workflow:submit › submit-review: exited 1: mktemp: File exists"
        );
    }

    #[test]
    fn agent_child_is_not_followed() {
        let conn = setup();
        insert_run(&conn, "r1", "wf", "failed", None);
        insert_run(&conn, "agent-child", "other", "failed", None);
        insert_step(
            &conn,
            "s1",
            "r1",
            "build",
            "actor",
            0,
            "2026-01-01T00:00:01Z",
            Some("agent-child"),
            Some("boom"),
        );
        let chain = run_failure_chain(&conn, "r1").unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].child_run_id, None);
        assert!(child_failure_chain(&conn, &top_step(&conn, "r1"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn latest_retry_attempt_wins() {
        let conn = setup();
        insert_run(&conn, "r1", "wf", "failed", None);
        insert_step(
            &conn,
            "s1",
            "r1",
            "flaky",
            "actor",
            0,
            "2026-01-01T00:00:01Z",
            None,
            Some("first"),
        );
        insert_step(
            &conn,
            "s2",
            "r1",
            "flaky",
            "actor",
            1,
            "2026-01-01T00:00:05Z",
            None,
            Some("second"),
        );
        let chain = run_failure_chain(&conn, "r1").unwrap();
        assert_eq!(chain[0].error, "second");
    }

    #[test]
    fn child_without_failed_step_uses_run_error() {
        let conn = setup();
        insert_run(&conn, "r1", "wf", "failed", None);
        insert_run(&conn, "r2", "child", "failed", Some("reaped: engine died"));
        insert_step(
            &conn,
            "s1",
            "r1",
            "workflow:child",
            "workflow",
            0,
            "2026-01-01T00:00:01Z",
            Some("r2"),
            Some("Sub-workflow 'child' failed"),
        );
        let chain = run_failure_chain(&conn, "r1").unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1].error, "reaped: engine died");
    }

    #[test]
    fn missing_child_run_ends_chain() {
        let conn = setup();
        insert_run(&conn, "r1", "wf", "failed", None);
        insert_step(
            &conn,
            "s1",
            "r1",
            "workflow:gone",
            "workflow",
            0,
            "2026-01-01T00:00:01Z",
            Some("purged"),
            None,
        );
        let chain = run_failure_chain(&conn, "r1").unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].error, NO_ERROR);
        assert!(run_failure_chain(&conn, "nope").unwrap().is_empty());
    }

    #[test]
    fn cycle_and_depth_are_bounded() {
        let conn = setup();
        insert_run(&conn, "a", "wf", "failed", None);
        insert_run(&conn, "b", "wf", "failed", None);
        insert_step(
            &conn,
            "s1",
            "a",
            "workflow:b",
            "workflow",
            0,
            "2026-01-01T00:00:01Z",
            Some("b"),
            Some("x"),
        );
        insert_step(
            &conn,
            "s2",
            "b",
            "workflow:a",
            "workflow",
            0,
            "2026-01-01T00:00:01Z",
            Some("a"),
            Some("y"),
        );
        assert_eq!(run_failure_chain(&conn, "a").unwrap().len(), 2);

        for i in 0..12 {
            let id = format!("d{i}");
            insert_run(&conn, &id, "wf", "failed", None);
            insert_step(
                &conn,
                &format!("ds{i}"),
                &id,
                "workflow:n",
                "workflow",
                0,
                "2026-01-01T00:00:01Z",
                Some(&format!("d{}", i + 1)),
                Some("x"),
            );
        }
        assert_eq!(run_failure_chain(&conn, "d0").unwrap().len(), MAX_DEPTH);
    }

    #[test]
    fn errors_are_one_line_and_capped() {
        let conn = setup();
        insert_run(&conn, "r1", "wf", "failed", None);
        let long = format!("line1\nline2 {}", "x".repeat(500));
        insert_step(
            &conn,
            "s1",
            "r1",
            "s",
            "actor",
            0,
            "2026-01-01T00:00:01Z",
            None,
            Some(&long),
        );
        let chain = run_failure_chain(&conn, "r1").unwrap();
        assert!(!chain[0].error.contains('\n'));
        assert!(chain[0].error.starts_with("line1 line2 "));
        assert!(chain[0].error.len() <= MAX_ERROR_LEN + "…".len());
    }

    #[test]
    fn failed_call_step_chain_reuses_known_chain() {
        let conn = setup();
        insert_run(&conn, "r1", "parent", "failed", None);
        insert_run(&conn, "r2", "child", "failed", None);
        insert_step(
            &conn,
            "s1",
            "r1",
            "workflow:child",
            "workflow",
            0,
            "2026-01-01T00:00:01Z",
            Some("r2"),
            Some("Sub-workflow 'child' failed"),
        );
        insert_step(
            &conn,
            "s2",
            "r2",
            "leaf",
            "script",
            0,
            "2026-01-01T00:00:02Z",
            None,
            Some("boom"),
        );
        let step = top_step(&conn, "r1");
        let known = run_failure_chain(&conn, "r1").unwrap();
        let below = failed_call_step_chain(&conn, &step, &known).unwrap();
        assert_eq!(below, known[1..].to_vec());
        // Without a known chain the walk is done from the DB and agrees.
        assert_eq!(failed_call_step_chain(&conn, &step, &[]).unwrap(), below);
        assert_eq!(
            run_failure_cause(&conn, "r1").as_deref(),
            Some("workflow:child › leaf: boom")
        );
    }

    #[test]
    fn failed_call_step_chain_empty_for_non_failed_or_non_call() {
        let conn = setup();
        insert_run(&conn, "r1", "parent", "failed", None);
        insert_run(&conn, "r2", "child", "failed", None);
        insert_step(
            &conn,
            "s1",
            "r1",
            "agent-step",
            "actor",
            0,
            "2026-01-01T00:00:01Z",
            Some("r2"),
            Some("x"),
        );
        insert_step(
            &conn,
            "s2",
            "r2",
            "leaf",
            "script",
            0,
            "2026-01-01T00:00:02Z",
            None,
            Some("boom"),
        );
        let mut step = top_step(&conn, "r1");
        assert!(failed_call_step_chain(&conn, &step, &[])
            .unwrap()
            .is_empty());
        step.role = "workflow".into();
        step.status = WorkflowStepStatus::Completed;
        assert!(failed_call_step_chain(&conn, &step, &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn render_failure_tree_nests_levels() {
        let link = |step: &str, child: Option<&str>, error: &str| FailureLink {
            run_id: "r".into(),
            workflow_name: "wf".into(),
            step_name: step.into(),
            child_run_id: child.map(Into::into),
            error: error.into(),
        };
        let chain = vec![
            link("workflow:submit", Some("C2"), "Sub-workflow failed"),
            link("submit-review", None, "exited 1: mktemp failed"),
        ];
        assert_eq!(
            render_failure_tree(&chain, 8),
            "        └ [FAIL] workflow:submit (child C2)\n          └ [FAIL] submit-review: exited 1: mktemp failed\n"
        );
        assert_eq!(render_failure_tree(&[], 8), "");
    }
}
