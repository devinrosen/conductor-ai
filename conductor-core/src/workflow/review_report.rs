//! `workflow review-show` / MCP `conductor_get_review`: one `review-pr` run's review as
//! structured data (#3092).
//!
//! The report is built from the database alone: the run row, its latest step rows and
//! the definition snapshot. The `review-aggregator` output is passed through as JSON so
//! a new aggregator field needs no change here. The posted review's URL comes from the
//! `submit-review` step's output, which `submit-review.sh` records after posting.

use std::collections::HashMap;
use std::fmt::Write as _;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ConductorError, Result};
use crate::workflow::run_stats::parse_output;

const REVIEW_WORKFLOW: &str = "review-pr";
const AGGREGATOR_STEP: &str = "review-aggregator";
const SUBMIT_STEP: &str = "submit-review";
const VALIDATE_STEP: &str = "validate-workflows";
/// The parallel block's output name that marks the reviewer fan-out.
const REVIEWERS_OUTPUT: &str = "review-findings";
/// Steps that start with `review-` but are not reviewers (snapshot fallback only).
const NON_REVIEWER_STEPS: &[&str] = &[AGGREGATOR_STEP, "review-reply-check"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewScope {
    /// `full` or `incremental`.
    pub kind: String,
    /// The sha an incremental review started from.
    pub since: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewerStatus {
    pub name: String,
    /// The step's status (`completed`, `failed`, `skipped`, ...), or `not_started`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostedReview {
    pub url: Option<String>,
    pub id: Option<i64>,
    /// `APPROVE`, `COMMENT` or `REQUEST_CHANGES`.
    pub event: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    pub dry_run: bool,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub repo_id: Option<String>,
    pub pr_number: Option<u64>,
    pub parent_workflow_run_id: Option<String>,
    pub scope: ReviewScope,
    pub reviewers: Vec<ReviewerStatus>,
    /// The aggregator's output (`blocking_findings`, `answered_findings`,
    /// `off_diff_findings`, `overall_approved`, ...) without `markers` and `context`.
    pub review: Option<Value>,
    pub aggregator_status: Option<String>,
    pub posted_review: Option<PostedReview>,
    pub notes: Vec<String>,
}

fn not_found(msg: String) -> ConductorError {
    ConductorError::Workflow(msg)
}

/// Resolve `run_id` to a `review-pr` run: the run itself, or, for any other workflow
/// (a ticket-to-pr or iterate-pr id), the most recent `review-pr` descendant.
pub fn resolve_review_run(conn: &Connection, run_id: &str) -> Result<String> {
    let name: Option<String> = conn
        .query_row(
            "SELECT workflow_name FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()?;
    let name = name.ok_or_else(|| ConductorError::WorkflowRunNotFound {
        id: run_id.to_string(),
    })?;
    if name == REVIEW_WORKFLOW {
        return Ok(run_id.to_string());
    }
    let found: Option<String> = conn
        .query_row(
            "WITH RECURSIVE tree(id) AS (
                 SELECT id FROM workflow_runs WHERE parent_workflow_run_id = ?1
                 UNION
                 SELECT w.id FROM workflow_runs w JOIN tree t ON w.parent_workflow_run_id = t.id
             )
             SELECT w.id FROM workflow_runs w JOIN tree t ON w.id = t.id
             WHERE w.workflow_name = ?2 ORDER BY w.started_at DESC, w.id DESC LIMIT 1",
            params![run_id, REVIEW_WORKFLOW],
            |r| r.get(0),
        )
        .optional()?;
    found.ok_or_else(|| {
        not_found(format!(
            "run {run_id} is a '{name}' run with no review-pr run under it"
        ))
    })
}

/// The newest `review-pr` run (any status) for a PR in one repo.
pub fn latest_review_run_for_pr(
    conn: &Connection,
    repo_id: &str,
    pr_number: u64,
) -> Result<Option<String>> {
    // Inputs store the PR number as a string, so compare as text.
    let id = conn
        .query_row(
            "SELECT id FROM workflow_runs WHERE workflow_name = ?1 AND repo_id = ?2 \
             AND CAST(json_extract(inputs, '$.pr_number') AS TEXT) = ?3 \
             ORDER BY started_at DESC, id DESC LIMIT 1",
            params![REVIEW_WORKFLOW, repo_id, pr_number.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id)
}

/// Resolve a caller's selector to a `review-pr` run id: either a run id (a review, or a
/// parent run), or a repo (by id) plus a PR number. Exactly one form is accepted.
pub fn find_review_run(
    conn: &Connection,
    run_id: Option<&str>,
    repo_id: Option<&str>,
    pr_number: Option<u64>,
) -> Result<String> {
    match (run_id, repo_id, pr_number) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(not_found(
            "pass either a run id or a repo with a PR number, not both".into(),
        )),
        (Some(id), None, None) => resolve_review_run(conn, id),
        (None, Some(repo), Some(pr)) => latest_review_run_for_pr(conn, repo, pr)?
            .ok_or_else(|| not_found(format!("no review-pr run found for PR #{pr} in that repo"))),
        (None, None, Some(_)) => Err(not_found(
            "a PR number needs a repo: PR numbers are per repo".into(),
        )),
        (None, Some(_), None) => Err(not_found("a repo needs a PR number".into())),
        (None, None, None) => Err(not_found(
            "pass a run id, or a repo with a PR number".into(),
        )),
    }
}

struct StepInfo {
    status: String,
    output: Option<Value>,
}

/// Latest row per step name (highest iteration, then latest start): resume and retries
/// can leave several rows.
fn latest_steps(conn: &Connection, run_id: &str) -> Result<HashMap<String, StepInfo>> {
    let mut stmt = conn.prepare(
        "SELECT step_name, status, structured_output FROM workflow_run_steps \
         WHERE workflow_run_id = ?1 ORDER BY iteration, started_at, position",
    )?;
    let rows = stmt
        .query_map(params![run_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = HashMap::new();
    for (name, status, raw) in rows {
        let output = parse_output(raw, run_id, &name);
        out.insert(name, StepInfo { status, output });
    }
    Ok(out)
}

/// Reviewer names from the snapshot's parallel block that outputs `review-findings`.
/// `None` when the snapshot is missing, unparseable, or has no such block.
fn snapshot_reviewers(snapshot: Option<&str>) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(snapshot?).ok()?;
    let body = v.get("body")?.as_array()?;
    let block = body.iter().find(|n| {
        n.get("type").and_then(Value::as_str) == Some("parallel")
            && n.get("output").and_then(Value::as_str) == Some(REVIEWERS_OUTPUT)
    })?;
    let names: Vec<String> = block
        .get("calls")?
        .as_array()?
        .iter()
        .filter_map(|c| c.get("value").and_then(Value::as_str).map(String::from))
        .collect();
    (!names.is_empty()).then_some(names)
}

fn strip_markers(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("markers");
        o.remove("context");
    }
    v
}

fn posted_review(output: &Value) -> Option<PostedReview> {
    let url = output
        .get("review_url")
        .and_then(Value::as_str)
        .map(String::from);
    let id = output.get("review_id").and_then(Value::as_i64);
    let event = output
        .get("event")
        .and_then(Value::as_str)
        .map(String::from);
    (url.is_some() || id.is_some() || event.is_some()).then_some(PostedReview { url, id, event })
}

/// Build the report for one `review-pr` run.
pub fn load_review_report(conn: &Connection, run_id: &str) -> Result<ReviewReport> {
    type RunCols = (
        String,
        String,
        bool,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<RunCols> = conn
        .query_row(
            "SELECT workflow_name, status, dry_run, started_at, ended_at, repo_id, \
             parent_workflow_run_id, inputs, definition_snapshot \
             FROM workflow_runs WHERE id = ?1",
            params![run_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()?;
    let (workflow_name, status, dry_run, started_at, ended_at, repo_id, parent, inputs, snapshot) =
        row.ok_or_else(|| ConductorError::WorkflowRunNotFound {
            id: run_id.to_string(),
        })?;
    if workflow_name != REVIEW_WORKFLOW {
        return Err(not_found(format!(
            "run {run_id} is a '{workflow_name}' run, not review-pr"
        )));
    }

    let mut notes = Vec::new();
    let inputs: Value = inputs
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let input_str = |k: &str| {
        inputs.get(k).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    let pr_number = input_str("pr_number").and_then(|s| s.parse::<u64>().ok());
    let since = input_str("since").filter(|s| !s.is_empty());
    let scope = if input_str("scope").as_deref() == Some("incremental") && since.is_some() {
        ReviewScope {
            kind: "incremental".into(),
            since,
        }
    } else {
        ReviewScope {
            kind: "full".into(),
            since: None,
        }
    };

    let steps = latest_steps(conn, run_id)?;

    let names = match snapshot_reviewers(snapshot.as_deref()) {
        Some(mut names) => {
            if steps.contains_key(VALIDATE_STEP) {
                names.push(VALIDATE_STEP.to_string());
            }
            names
        }
        None => {
            notes.push(
                "reviewer list taken from step names: the definition snapshot has no review-findings block"
                    .into(),
            );
            let mut names: Vec<String> = steps
                .keys()
                .filter(|n| {
                    (n.starts_with("review-") && !NON_REVIEWER_STEPS.contains(&n.as_str()))
                        || n.as_str() == VALIDATE_STEP
                })
                .cloned()
                .collect();
            names.sort();
            names
        }
    };
    let reviewers = names
        .into_iter()
        .map(|name| {
            let status = steps
                .get(&name)
                .map(|s| s.status.clone())
                .unwrap_or_else(|| "not_started".into());
            ReviewerStatus { name, status }
        })
        .collect();

    let agg = steps.get(AGGREGATOR_STEP);
    let aggregator_status = agg.map(|s| s.status.clone());
    let review = agg.and_then(|s| s.output.clone()).map(strip_markers);
    if review.is_none() {
        notes.push(match status.as_str() {
            "running" | "pending" | "waiting" => {
                format!("run is {status}: the review-aggregator has not produced output yet")
            }
            _ => format!("run is {status} and the review-aggregator produced no output"),
        });
    }

    let posted = steps.get(SUBMIT_STEP).and_then(|s| {
        if s.status == "completed" {
            s.output.as_ref().and_then(posted_review)
        } else {
            None
        }
    });
    if posted.is_none() {
        notes.push(if dry_run {
            "dry run: no review was posted".into()
        } else {
            "posted review not recorded: the run has not submitted a review, or it predates the submit-review output (#3092)"
                .into()
        });
    } else if posted.as_ref().is_some_and(|p| p.url.is_none()) {
        notes.push("the review was posted but its URL could not be looked up".into());
    }

    Ok(ReviewReport {
        run_id: run_id.to_string(),
        workflow_name,
        status,
        dry_run,
        started_at,
        ended_at,
        repo_id,
        pr_number,
        parent_workflow_run_id: parent,
        scope,
        reviewers,
        review,
        aggregator_status,
        posted_review: posted,
        notes,
    })
}

fn finding_list(review: &Value, key: &str) -> Vec<Value> {
    review
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn format_finding(f: &Value) -> String {
    let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("");
    let line = match f.get("line") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(l)) => l.clone(),
        _ => String::new(),
    };
    let loc = if line.is_empty() {
        s("file").to_string()
    } else {
        format!("{}:{}", s("file"), line)
    };
    let msg = if s("message").is_empty() {
        s("title")
    } else {
        s("message")
    };
    format!("  - {} {} ({}) — {}", s("severity"), loc, s("reviewer"), msg)
}

/// Human-readable form of a report.
pub fn format_review_report(r: &ReviewReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Review run:  {}", r.run_id);
    let _ = writeln!(out, "Status:      {}", r.status);
    if let Some(pr) = r.pr_number {
        let _ = writeln!(out, "PR:          #{pr}");
    }
    match &r.scope.since {
        Some(since) => {
            let _ = writeln!(out, "Scope:       {} since {since}", r.scope.kind);
        }
        None => {
            let _ = writeln!(out, "Scope:       {}", r.scope.kind);
        }
    }
    let approved = r
        .review
        .as_ref()
        .and_then(|v| v.get("overall_approved"))
        .and_then(Value::as_bool);
    let _ = writeln!(
        out,
        "Approved:    {}",
        match approved {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        }
    );
    if let Some(p) = &r.posted_review {
        let _ = writeln!(
            out,
            "Posted:      {} {}",
            p.event.as_deref().unwrap_or("?"),
            p.url.as_deref().unwrap_or("(url not recorded)")
        );
    }
    let list = |status_is: bool| {
        r.reviewers
            .iter()
            .filter(|x| (x.status == "skipped") == status_is)
            .map(|x| {
                if status_is || x.status == "completed" {
                    x.name.clone()
                } else {
                    format!("{} ({})", x.name, x.status)
                }
            })
            .collect::<Vec<_>>()
    };
    let _ = writeln!(out, "Reviewers:   ran: {}", list(false).join(", "));
    let skipped = list(true);
    if !skipped.is_empty() {
        let _ = writeln!(out, "             skipped: {}", skipped.join(", "));
    }
    if let Some(review) = &r.review {
        if let Some(summary) = review.get("summary").and_then(Value::as_str) {
            let _ = writeln!(out, "\nSummary: {summary}");
        }
        for (key, title) in [
            ("blocking_findings", "Blocking findings"),
            ("answered_findings", "Answered findings"),
            ("off_diff_findings", "Off-diff findings"),
        ] {
            let items = finding_list(review, key);
            let _ = writeln!(out, "\n{title} ({}):", items.len());
            for f in &items {
                let _ = writeln!(out, "{}", format_finding(f));
            }
        }
    }
    if !r.notes.is_empty() {
        let _ = writeln!(out, "\nNotes:");
        for n in &r.notes {
            let _ = writeln!(out, "  - {n}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;

    fn db() -> Connection {
        let conn = test_helpers::setup_db();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        conn: &Connection,
        id: &str,
        name: &str,
        status: &str,
        parent: Option<&str>,
        repo: Option<&str>,
        started: &str,
        inputs: Option<&str>,
        snapshot: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, parent_run_id, status, dry_run, \
             trigger, started_at, iteration, parent_workflow_run_id, repo_id, inputs, \
             definition_snapshot) VALUES (?1, ?2, 'PA', ?3, 0, 'manual', ?4, 0, ?5, ?6, ?7, ?8)",
            params![id, name, status, started, parent, repo, inputs, snapshot],
        )
        .unwrap();
    }

    fn step(conn: &Connection, run: &str, name: &str, status: &str, iter: i64, out: Option<&str>) {
        conn.execute(
            "INSERT INTO workflow_run_steps (id, workflow_run_id, step_name, role, status, \
             position, started_at, iteration, structured_output) \
             VALUES (?1, ?2, ?3, 'actor', ?4, 0, ?5, ?6, ?7)",
            params![
                format!("{run}-{name}-{iter}"),
                run,
                name,
                status,
                format!("2026-01-01T00:0{iter}:00Z"),
                iter,
                out
            ],
        )
        .unwrap();
    }

    const SNAPSHOT: &str = r#"{"body":[{"type":"call","kind":"name","value":"resolve"},{"type":"parallel","output":"review-findings","calls":[{"kind":"name","value":"review-architecture"},{"kind":"name","value":"review-security"},{"kind":"name","value":"review-db"}]}]}"#;
    const AGG: &str = r#"{"markers":["x"],"context":"c","reviewed_by":"a","blocking_findings":[{"severity":"high","file":"a.rs","line":3,"reviewer":"Security","message":"bad"}],"answered_findings":[],"off_diff_findings":[],"summary":"s","overall_approved":false}"#;
    const SUBMIT: &str = r#"{"markers":[],"context":"c","review_id":42,"review_url":"https://github.com/o/r/pull/1#pullrequestreview-42","event":"REQUEST_CHANGES","overall_approved":false}"#;

    fn full_fixture(conn: &Connection, inputs: &str) {
        run(
            conn,
            "R",
            "review-pr",
            "completed",
            None,
            Some("repo1"),
            "2026-01-01T00:00:00Z",
            Some(inputs),
            Some(SNAPSHOT),
        );
        step(conn, "R", "review-architecture", "completed", 0, Some("{}"));
        step(conn, "R", "review-security", "completed", 0, Some("{}"));
        step(conn, "R", "review-db", "skipped", 0, None);
        step(conn, "R", "validate-workflows", "completed", 0, Some("{}"));
        step(conn, "R", "review-aggregator", "completed", 0, Some(AGG));
        step(conn, "R", "submit-review", "completed", 0, Some(SUBMIT));
    }

    #[test]
    fn full_report() {
        let conn = db();
        full_fixture(&conn, r#"{"pr_number":"7","scope":"full","since":""}"#);
        let r = load_review_report(&conn, "R").unwrap();
        assert_eq!(r.pr_number, Some(7));
        assert_eq!(r.scope.kind, "full");
        assert_eq!(r.scope.since, None);
        let names: Vec<_> = r
            .reviewers
            .iter()
            .map(|x| (x.name.as_str(), x.status.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("review-architecture", "completed"),
                ("review-security", "completed"),
                ("review-db", "skipped"),
                ("validate-workflows", "completed"),
            ]
        );
        let review = r.review.as_ref().unwrap();
        assert!(review.get("markers").is_none() && review.get("context").is_none());
        assert_eq!(review["overall_approved"], false);
        assert_eq!(review["blocking_findings"][0]["file"], "a.rs");
        let p = r.posted_review.unwrap();
        assert_eq!(p.id, Some(42));
        assert_eq!(p.event.as_deref(), Some("REQUEST_CHANGES"));
        assert!(p.url.unwrap().ends_with("pullrequestreview-42"));
        assert!(r.notes.is_empty(), "{:?}", r.notes);
    }

    #[test]
    fn incremental_scope_has_since() {
        let conn = db();
        full_fixture(
            &conn,
            r#"{"pr_number":7,"scope":"incremental","since":"abc123"}"#,
        );
        let r = load_review_report(&conn, "R").unwrap();
        assert_eq!(r.pr_number, Some(7));
        assert_eq!(r.scope.kind, "incremental");
        assert_eq!(r.scope.since.as_deref(), Some("abc123"));
    }

    #[test]
    fn incremental_without_since_is_full() {
        let conn = db();
        full_fixture(&conn, r#"{"scope":"incremental","since":""}"#);
        let r = load_review_report(&conn, "R").unwrap();
        assert_eq!(r.scope.kind, "full");
    }

    #[test]
    fn string_wrapped_output_is_parsed() {
        let conn = db();
        run(
            &conn,
            "R",
            "review-pr",
            "completed",
            None,
            None,
            "2026-01-01T00:00:00Z",
            None,
            Some(SNAPSHOT),
        );
        let wrapped = serde_json::to_string(AGG).unwrap();
        step(&conn, "R", "review-aggregator", "completed", 0, Some(&wrapped));
        let r = load_review_report(&conn, "R").unwrap();
        assert_eq!(r.review.unwrap()["summary"], "s");
    }

    #[test]
    fn latest_iteration_wins() {
        let conn = db();
        run(
            &conn,
            "R",
            "review-pr",
            "completed",
            None,
            None,
            "2026-01-01T00:00:00Z",
            None,
            Some(SNAPSHOT),
        );
        step(&conn, "R", "review-aggregator", "completed", 0, Some(r#"{"summary":"old"}"#));
        step(&conn, "R", "review-aggregator", "completed", 1, Some(r#"{"summary":"new"}"#));
        let r = load_review_report(&conn, "R").unwrap();
        assert_eq!(r.review.unwrap()["summary"], "new");
    }

    #[test]
    fn running_run_has_no_review_and_a_note() {
        let conn = db();
        run(
            &conn,
            "R",
            "review-pr",
            "running",
            None,
            None,
            "2026-01-01T00:00:00Z",
            None,
            Some(SNAPSHOT),
        );
        step(&conn, "R", "review-architecture", "running", 0, None);
        let r = load_review_report(&conn, "R").unwrap();
        assert!(r.review.is_none() && r.posted_review.is_none());
        assert!(r.notes.iter().any(|n| n.contains("running")), "{:?}", r.notes);
        let sec = r
            .reviewers
            .iter()
            .find(|x| x.name == "review-security")
            .unwrap();
        assert_eq!(sec.status, "not_started");
    }

    #[test]
    fn missing_snapshot_falls_back_to_step_names() {
        let conn = db();
        run(
            &conn,
            "R",
            "review-pr",
            "completed",
            None,
            None,
            "2026-01-01T00:00:00Z",
            None,
            None,
        );
        step(&conn, "R", "review-security", "completed", 0, None);
        step(&conn, "R", "review-aggregator", "completed", 0, Some(AGG));
        step(&conn, "R", "review-reply-check", "completed", 0, None);
        step(&conn, "R", "resolve-pr-base", "completed", 0, None);
        let r = load_review_report(&conn, "R").unwrap();
        assert_eq!(r.reviewers.len(), 1);
        assert_eq!(r.reviewers[0].name, "review-security");
        assert!(r.notes.iter().any(|n| n.contains("step names")));
        // No submit-review row: the posted review is unknown, with a note.
        assert!(r.posted_review.is_none());
        assert!(r.notes.iter().any(|n| n.contains("not recorded")));
    }

    #[test]
    fn resolves_latest_review_through_parent_chain() {
        let conn = db();
        run(&conn, "T", "ticket-to-pr", "completed", None, None, "2026-01-01T00:00:00Z", None, None);
        run(&conn, "I", "iterate-pr", "completed", Some("T"), None, "2026-01-01T01:00:00Z", None, None);
        run(&conn, "R1", "review-pr", "completed", Some("T"), None, "2026-01-01T02:00:00Z", None, None);
        run(&conn, "R2", "review-pr", "completed", Some("I"), None, "2026-01-01T03:00:00Z", None, None);
        assert_eq!(resolve_review_run(&conn, "T").unwrap(), "R2");
        assert_eq!(resolve_review_run(&conn, "I").unwrap(), "R2");
        assert_eq!(resolve_review_run(&conn, "R1").unwrap(), "R1");
    }

    #[test]
    fn resolve_errors_without_review_or_run() {
        let conn = db();
        run(&conn, "T", "ticket-to-pr", "completed", None, None, "2026-01-01T00:00:00Z", None, None);
        let e = resolve_review_run(&conn, "T").unwrap_err().to_string();
        assert!(e.contains("no review-pr run"), "{e}");
        assert!(matches!(
            resolve_review_run(&conn, "nope"),
            Err(ConductorError::WorkflowRunNotFound { .. })
        ));
        assert!(load_review_report(&conn, "T").is_err());
    }

    #[test]
    fn latest_review_run_for_pr_is_per_repo() {
        let conn = db();
        let inp = r#"{"pr_number":"9"}"#;
        run(&conn, "A1", "review-pr", "completed", None, Some("repoA"), "2026-01-01T00:00:00Z", Some(inp), None);
        run(&conn, "A2", "review-pr", "failed", None, Some("repoA"), "2026-01-01T02:00:00Z", Some(inp), None);
        run(&conn, "B1", "review-pr", "completed", None, Some("repoB"), "2026-01-01T03:00:00Z", Some(inp), None);
        run(&conn, "A3", "review-pr", "completed", None, Some("repoA"), "2026-01-01T04:00:00Z", Some(r#"{"pr_number":"90"}"#), None);
        assert_eq!(
            latest_review_run_for_pr(&conn, "repoA", 9).unwrap().as_deref(),
            Some("A2")
        );
        assert_eq!(
            latest_review_run_for_pr(&conn, "repoB", 9).unwrap().as_deref(),
            Some("B1")
        );
        assert_eq!(latest_review_run_for_pr(&conn, "repoA", 5).unwrap(), None);
    }

    #[test]
    fn find_review_run_validates_selector() {
        let conn = db();
        full_fixture(&conn, r#"{"pr_number":"7"}"#);
        assert_eq!(find_review_run(&conn, Some("R"), None, None).unwrap(), "R");
        assert_eq!(
            find_review_run(&conn, None, Some("repo1"), Some(7)).unwrap(),
            "R"
        );
        assert!(find_review_run(&conn, None, None, None).is_err());
        assert!(find_review_run(&conn, Some("R"), Some("repo1"), Some(7)).is_err());
        assert!(find_review_run(&conn, None, None, Some(7)).is_err());
        assert!(find_review_run(&conn, None, Some("repo1"), None).is_err());
        assert!(find_review_run(&conn, None, Some("repo1"), Some(8)).is_err());
    }

    #[test]
    fn format_includes_findings_and_url() {
        let conn = db();
        full_fixture(&conn, r#"{"pr_number":"7","scope":"incremental","since":"abc"}"#);
        let text = format_review_report(&load_review_report(&conn, "R").unwrap());
        assert!(text.contains("incremental since abc"), "{text}");
        assert!(text.contains("Approved:    no"), "{text}");
        assert!(text.contains("high a.rs:3 (Security) — bad"), "{text}");
        assert!(text.contains("skipped: review-db"), "{text}");
        assert!(text.contains("pullrequestreview-42"), "{text}");
    }
}
