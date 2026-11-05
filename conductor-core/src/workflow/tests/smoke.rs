//! Smoke mode (`workflow validate --dry-run`, #3292): the run is an engine dry
//! run, so no script step ever executes and gates are auto-approved; it stops
//! cancelled at the first agent step, and no agent ever starts. The engine's
//! start-of-run validation still runs, so an unknown gate type fails (#3281).

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::workflow::{smoke_run_workflow, SmokeResult, SmokeStopKind};

struct Fixture {
    _tmp: tempfile::NamedTempFile,
    db_path: std::path::PathBuf,
    ws: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let (tmp, db_path) = make_standalone_db();
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join(".conductor/agents")).unwrap();
    std::fs::create_dir_all(ws.path().join(".conductor/workflows")).unwrap();
    std::fs::write(
        ws.path().join(".conductor/agents/some-agent.md"),
        "---\nrole: actor\n---\n\nDo the thing.\n",
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        db_path,
        ws,
    }
}

/// Write an executable script that touches `<name>.ran` and emits clean output
/// (or exits 1 when `fail`). Returns its path.
fn script(f: &Fixture, name: &str, fail: bool) -> String {
    let dir = f.ws.path().to_str().unwrap();
    let body = if fail {
        "exit 1\n".to_string()
    } else {
        format!(
            "touch {dir}/{name}.ran\necho '<<<FLOW_OUTPUT>>>'\n\
             echo '{{\"markers\": [], \"context\": \"ok\"}}'\n\
             echo '<<<END_FLOW_OUTPUT>>>'\n"
        )
    };
    let path = f.ws.path().join(format!("{name}.sh"));
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_string()
}

fn ran(f: &Fixture, name: &str) -> bool {
    f.ws.path().join(format!("{name}.ran")).exists()
}

fn parse(body: &str) -> runkon_flow::dsl::WorkflowDef {
    runkon_flow::dsl::parse_workflow_str(
        &format!(
            "workflow smoke-wf {{\n meta {{ trigger = \"manual\" targets = [\"worktree\"] }}\n{body}\n}}"
        ),
        "<test>",
    )
    .expect("test workflow should parse")
}

fn params(
    f: &Fixture,
    def: runkon_flow::dsl::WorkflowDef,
) -> crate::workflow::types::WorkflowExecStandalone {
    let ws = f.ws.path().to_str().unwrap().to_string();
    crate::workflow::types::WorkflowExecStandalone {
        db_path: Some(f.db_path.clone()),
        ..WorkflowExecStandalone::new(
            Config::default(),
            def,
            ws.clone(),
            ws,
            WorkflowExecConfig::default(),
        )
    }
}

fn run_status(f: &Fixture, run_id: &str) -> String {
    let conn = crate::db::open_database(&f.db_path).unwrap();
    super::run_status_of(&conn, run_id).expect("run row")
}

#[test]
fn script_then_agent_stops_at_the_agent() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!(
        "script a {{ run = \"{a}\" }}\ncall some-agent\nscript after {{ run = \"{a}\" }}"
    ));
    let outcome = smoke_run_workflow(params(&f, def));
    let SmokeResult::Passed {
        stopped_at: Some(point),
    } = &outcome.result
    else {
        panic!("expected a pass at the agent, got {:?}", outcome.result);
    };
    assert_eq!(point.kind, SmokeStopKind::Agent);
    assert_eq!(point.step, "some-agent");
    assert!(!ran(&f, "a"), "smoke mode must never execute a script step");

    let run_id = outcome.run_id.expect("run id");
    assert_eq!(run_status(&f, &run_id), "cancelled");
    let conn = crate::db::open_database(&f.db_path).unwrap();
    // The stub is the only executor: no agent run was ever spawned for a step.
    let steps = crate::workflow::get_workflow_steps(&conn, &run_id).unwrap();
    assert!(
        steps.iter().all(|s| s.step_name != "after"),
        "nothing after the agent step may run"
    );
}

#[test]
fn gate_is_passed_without_running_scripts_and_stops_at_the_agent() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!(
        "gate human_review {{ prompt = \"ok?\" timeout = \"1h\" }}\nscript a {{ run = \"{a}\" }}\ncall some-agent"
    ));
    let outcome = smoke_run_workflow(params(&f, def));
    let SmokeResult::Passed {
        stopped_at: Some(point),
    } = &outcome.result
    else {
        panic!("expected a pass at the agent, got {:?}", outcome.result);
    };
    assert_eq!(point.kind, SmokeStopKind::Agent);
    assert!(!ran(&f, "a"), "smoke mode must never execute a script step");
    assert_eq!(run_status(&f, &outcome.run_id.unwrap()), "cancelled");
}

#[test]
fn unknown_gate_type_fails_before_any_step() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!(
        "script a {{ run = \"{a}\" }}\ngate not_a_type {{ prompt = \"x\" timeout = \"1h\" }}"
    ));
    let outcome = smoke_run_workflow(params(&f, def));
    match &outcome.result {
        SmokeResult::Failed { error, .. } => {
            assert!(error.contains("not_a_type"), "{error}")
        }
        other => panic!("expected a validation failure, got {other:?}"),
    }
    assert!(!ran(&f, "a"));
}

#[test]
fn child_workflow_stops_at_its_first_agent() {
    let f = fixture();
    let a = script(&f, "a", false);
    std::fs::write(
        f.ws.path().join(".conductor/workflows/smoke-child.wf"),
        format!(
            "workflow smoke-child {{\n meta {{ trigger = \"manual\" targets = [\"worktree\"] }}\n script c {{ run = \"{a}\" }}\n call some-agent\n}}\n"
        ),
    )
    .unwrap();
    let def = parse("call workflow smoke-child");
    let outcome = smoke_run_workflow(params(&f, def));
    let SmokeResult::Passed {
        stopped_at: Some(point),
    } = &outcome.result
    else {
        panic!("expected a pass in the child, got {:?}", outcome.result);
    };
    assert_eq!(point.workflow, "smoke-child");
    assert_eq!(point.step, "some-agent");
    assert!(!ran(&f, "a"), "child scripts don't run either");
    assert_eq!(run_status(&f, &outcome.run_id.unwrap()), "cancelled");
}

#[test]
fn failing_script_is_never_run() {
    let f = fixture();
    let bad = script(&f, "bad", true);
    let def = parse(&format!(
        "script broken-step {{ run = \"{bad}\" }}\ncall some-agent"
    ));
    let outcome = smoke_run_workflow(params(&f, def));
    assert!(
        outcome.passed(),
        "a script that would fail is skipped, not run: {:?}",
        outcome.result
    );
}

#[test]
fn scripts_only_workflow_completes() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!("script a {{ run = \"{a}\" }}"));
    let outcome = smoke_run_workflow(params(&f, def));
    assert_eq!(outcome.result, SmokeResult::Passed { stopped_at: None });
    assert!(!ran(&f, "a"), "smoke mode must never execute a script step");
    assert_eq!(run_status(&f, &outcome.run_id.unwrap()), "completed");
}

#[test]
fn active_run_on_the_worktree_does_not_block_and_is_not_cancelled() {
    let f = fixture();
    let conn = crate::db::open_database(&f.db_path).unwrap();
    let agent_run = crate::agent::AgentManager::new(&conn)
        .create_run(Some("w1"), "other", None)
        .unwrap();
    let active = crate::workflow::create_workflow_run_with_targets(
        &conn,
        "other-wf",
        Some("w1"),
        None,
        None,
        &agent_run.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .unwrap();
    crate::workflow::update_workflow_status(
        &conn,
        &active.id,
        WorkflowRunStatus::Running,
        None,
        None,
    )
    .unwrap();

    let def = parse("call some-agent");
    let mut p = params(&f, def);
    p.worktree_id = Some("w1".into());
    let outcome = smoke_run_workflow(p);
    assert!(outcome.passed(), "got {:?}", outcome.result);
    assert_eq!(run_status(&f, &active.id), "running");
}

#[test]
fn unopenable_database_is_reported_not_swallowed() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!("script a {{ run = \"{a}\" }}"));
    let mut p = params(&f, def);
    p.db_path = Some(f.ws.path().join("no-such-dir/nested/db.sqlite"));
    let outcome = smoke_run_workflow(p);
    match outcome.result {
        SmokeResult::Failed { error, .. } => {
            assert!(error.contains("failed to open database"), "{error}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Seed a run chain `top -> c1 -> c2 -> ... -> c<depth>` (each a failed child of
/// the previous) and return the connection. Every run gets a failed step named
/// `step-<run id>`, except where `skip_step` names a run.
fn seed_failed_chain(f: &Fixture, depth: u32, skip_step: Option<&str>) -> rusqlite::Connection {
    let conn = crate::db::open_database(&f.db_path).unwrap();
    let mut ids = vec!["top".to_string()];
    ids.extend((1..=depth).map(|i| format!("c{i}")));
    for (i, id) in ids.iter().enumerate() {
        crate::test_helpers::insert_test_wf_run(&conn, id, &format!("wf-{id}"), "failed", None);
        if i > 0 {
            conn.execute(
                "UPDATE workflow_runs SET parent_workflow_run_id = ?1 WHERE id = ?2",
                rusqlite::params![ids[i - 1], id],
            )
            .unwrap();
        }
        if skip_step != Some(id.as_str()) {
            crate::test_helpers::insert_test_wf_step(
                &conn,
                &format!("s-{id}"),
                id,
                &format!("step-{id}"),
                "script",
                "failed",
                0,
                "2026-01-01T00:00:01Z",
                None,
                None,
            );
        }
    }
    conn
}

#[test]
fn find_failed_step_reports_a_step_of_the_run_itself() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 0, None);
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "top", 0).as_deref(),
        Some("step-top")
    );
}

#[test]
fn find_failed_step_attributes_to_the_failed_child_run() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 1, None);
    // The parent's own failed step only reports the child; the child's step wins.
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "top", 0).as_deref(),
        Some("step-c1 (in wf-c1)")
    );
}

#[test]
fn find_failed_step_nests_through_grandchildren() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 2, None);
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "top", 0).as_deref(),
        Some("step-c2 (in wf-c2) (in wf-c1)")
    );
}

#[test]
fn find_failed_step_falls_back_to_the_parent_step_when_the_child_has_none() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 1, Some("c1"));
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "top", 0).as_deref(),
        Some("step-top")
    );
}

#[test]
fn find_failed_step_ignores_children_that_did_not_fail() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 1, None);
    conn.execute(
        "UPDATE workflow_runs SET status = 'cancelled' WHERE id = 'c1'",
        [],
    )
    .unwrap();
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "top", 0).as_deref(),
        Some("step-top")
    );
}

#[test]
fn find_failed_step_stops_past_the_depth_cap() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 1, None);
    // Past the cap nothing is looked up, not even the run's own failed step.
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "top", 17),
        None
    );
    // At the cap the run's own step is still found.
    assert_eq!(
        crate::workflow::smoke::find_failed_step(&conn, "c1", 16).as_deref(),
        Some("step-c1")
    );
}

#[test]
fn find_failed_step_stops_recursing_in_a_chain_deeper_than_the_cap() {
    let f = fixture();
    let conn = seed_failed_chain(&f, 20, None);
    // c16 is the deepest run still searched (depth 16); c17 is past the cap, so
    // the failure is attributed to c16's own step instead of recursing further.
    let found = crate::workflow::smoke::find_failed_step(&conn, "top", 0).unwrap();
    assert!(found.starts_with("step-c16 (in wf-c16)"), "{found}");
    assert!(!found.contains("c17"), "{found}");
}
