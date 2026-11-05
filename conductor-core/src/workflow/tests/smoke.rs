//! Smoke mode (`workflow validate --dry-run`, #3292): script steps run, the run
//! stops cancelled at the first agent or gate step, and no agent ever starts.

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
        config: Config::default(),
        workflow: def,
        worktree_id: None,
        working_dir: ws.clone(),
        repo_path: ws,
        ticket_id: None,
        repo_id: None,
        model: None,
        runtime: None,
        exec_config: WorkflowExecConfig::default(),
        inputs: HashMap::new(),
        target_label: None,
        run_id_notify: None,
        triggered_by_hook: false,
        conductor_bin_dir: None,
        force: false,
        extra_plugin_dirs: vec![],
        db_path: Some(f.db_path.clone()),
        parent_workflow_run_id: None,
        depth: 0,
        parent_step_id: None,
        default_bot_name: None,
        iteration: 0,
        smoke: None,
    }
}

fn run_status(f: &Fixture, run_id: &str) -> String {
    let conn = crate::db::open_database(&f.db_path).unwrap();
    crate::workflow::get_workflow_run_status(&conn, run_id)
        .unwrap()
        .expect("run row")
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
    assert!(ran(&f, "a"), "the script before the agent runs for real");

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
fn leading_gate_stops_at_the_gate_and_runs_nothing_after_it() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!(
        "gate human_review {{ prompt = \"ok?\" timeout = \"1h\" }}\nscript a {{ run = \"{a}\" }}"
    ));
    let outcome = smoke_run_workflow(params(&f, def));
    let SmokeResult::Passed {
        stopped_at: Some(point),
    } = &outcome.result
    else {
        panic!("expected a pass at the gate, got {:?}", outcome.result);
    };
    assert_eq!(point.kind, SmokeStopKind::Gate);
    assert!(!ran(&f, "a"), "a script after a gate must not run");
    assert_eq!(run_status(&f, &outcome.run_id.unwrap()), "cancelled");
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
    assert!(ran(&f, "a"));
    assert_eq!(run_status(&f, &outcome.run_id.unwrap()), "cancelled");
}

#[test]
fn failing_script_reports_its_step() {
    let f = fixture();
    let bad = script(&f, "bad", true);
    let def = parse(&format!(
        "script broken-step {{ run = \"{bad}\" }}\ncall some-agent"
    ));
    let outcome = smoke_run_workflow(params(&f, def));
    match &outcome.result {
        SmokeResult::Failed { step, .. } => {
            assert_eq!(step.as_deref(), Some("broken-step"));
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(!outcome.passed());
}

#[test]
fn scripts_only_workflow_completes() {
    let f = fixture();
    let a = script(&f, "a", false);
    let def = parse(&format!("script a {{ run = \"{a}\" }}"));
    let outcome = smoke_run_workflow(params(&f, def));
    assert_eq!(outcome.result, SmokeResult::Passed { stopped_at: None });
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
