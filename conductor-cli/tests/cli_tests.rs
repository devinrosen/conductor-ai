use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

/// Build a `conductor` command with `CONDUCTOR_HOME` pointed at `home` so
/// each test gets an isolated temp database and config.
fn conductor_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("conductor").unwrap();
    cmd.env("CONDUCTOR_HOME", home);
    cmd
}

#[test]
fn repo_list_empty() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args(["repo", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No repos registered"));
}

#[test]
fn repo_register_and_list() {
    let dir = tempfile::tempdir().unwrap();

    // Register a repo using a fake remote URL
    conductor_cmd(dir.path())
        .args(["repo", "register", "https://github.com/example/myrepo.git"])
        .assert()
        .success()
        .stdout(predicate::str::contains("myrepo"));

    // A follow-up list should show the registered repo
    conductor_cmd(dir.path())
        .args(["repo", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("myrepo"));
}

#[test]
fn worktree_list_empty() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args(["worktree", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No worktrees"));
}

#[test]
fn ticket_list_empty() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args(["tickets", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No tickets"));
}

#[test]
fn invalid_subcommand_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args(["foobar"])
        .assert()
        .failure();
}

// Note: `worktree create` and `worktree delete` are not tested here because
// they require a real git repository fixture (they call `git worktree add`
// and `git branch` via subprocess). Those operations are integration-tested
// manually or via dedicated repo fixtures in the future.

#[test]
fn worktree_create_stack_rejects_empty_tickets() {
    let dir = tempfile::tempdir().unwrap();
    // Omitting --tickets produces an empty vec and triggers the "at least one ticket" guard
    conductor_cmd(dir.path())
        .args([
            "worktree",
            "create-stack",
            "some-repo",
            "--root-branch",
            "main",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("at least one ticket"));
}

#[test]
fn worktree_create_stack_fails_on_unknown_repo() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args([
            "worktree",
            "create-stack",
            "nonexistent-repo",
            "--root-branch",
            "main",
            "--tickets",
            "123",
        ])
        .assert()
        .failure();
}

#[test]
fn workflow_validate_requires_name_or_all() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "either <NAME> or --all must be provided",
        ));
}

#[test]
fn workflow_validate_all_no_workflows_found() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "--all",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No workflow files found"));
}

/// Create a temp dir with a valid `hello` workflow + script fixture.
fn setup_valid_workflow_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let wf_dir = dir.path().join(".conductor").join("workflows");
    let scripts_dir = dir.path().join(".conductor").join("scripts");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::create_dir_all(&scripts_dir).unwrap();
    let script = scripts_dir.join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        wf_dir.join("hello.wf"),
        "workflow hello {\n  meta {\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n  script greet {\n    run = \".conductor/scripts/greet.sh\"\n  }\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn workflow_validate_all_pass() {
    let dir = setup_valid_workflow_fixture();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "--all",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS  hello"));
}

#[test]
fn workflow_validate_single_pass() {
    let dir = setup_valid_workflow_fixture();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "hello",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS  hello"));
}

#[test]
fn workflow_validate_fail_missing_agent() {
    let dir = tempfile::tempdir().unwrap();
    let wf_dir = dir.path().join(".conductor").join("workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("bad.wf"),
        "workflow bad {\n  meta {\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n  call nonexistent-agent {}\n}\n",
    )
    .unwrap();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "bad",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAIL  bad"))
        .stdout(predicate::str::contains("missing agent"));
}

#[test]
fn workflow_validate_all_failure_exit_code_and_summary() {
    let dir = setup_valid_workflow_fixture();
    // Add a second workflow that references a missing agent.
    let wf_dir = dir.path().join(".conductor").join("workflows");
    std::fs::write(
        wf_dir.join("broken.wf"),
        "workflow broken {\n  meta {\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n  call nonexistent-agent {}\n}\n",
    )
    .unwrap();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "--all",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAIL  broken"))
        .stdout(predicate::str::contains("1/2 workflow(s) passed"));
}

#[test]
fn workflow_validate_all_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let wf_dir = dir.path().join(".conductor").join("workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    // Write an unparseable workflow file.
    std::fs::write(wf_dir.join("garbage.wf"), "this is not valid syntax {{{\n").unwrap();
    conductor_cmd(dir.path())
        .args([
            "workflow",
            "validate",
            "--all",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAIL"));
}

/// Register `wt_dir` as repo `r` with an active worktree `wt` in the isolated
/// database under `home`, so `workflow run hello r wt` can resolve its target.
#[cfg(unix)]
fn register_worktree_fixture(home: &std::path::Path, wt_dir: &std::path::Path) {
    let conn = conductor_core::db::open_database(&home.join("conductor.db")).unwrap();
    let config = conductor_core::config::Config::default();
    let wt_path = wt_dir.to_str().unwrap();
    let repo = conductor_core::repo::RepoManager::new(&conn, &config)
        .register("r", wt_path, "https://github.com/example/r.git", None)
        .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES (?1, ?2, 'wt', 'feat/wt', ?3, 'active', '2026-01-01T00:00:00Z')",
        rusqlite::params![conductor_core::new_id(), repo.id, wt_path],
    )
    .unwrap();
}

/// `workflow run --background` starts the workflow in a separate, re-executed
/// process, prints only the run ID, and the run completes.
///
/// This covers the re-exec path from #3051 end to end. It does not reproduce the
/// original "locking protocol" failure (a forked child reusing the parent's
/// SQLite WAL state), which needed several long-running conductor processes
/// sharing the WAL and did not show up with an in-test writer.
#[cfg(unix)]
#[test]
fn workflow_run_background_prints_run_id_and_completes() {
    let home = tempfile::tempdir().unwrap();
    let wt = setup_valid_workflow_fixture();
    register_worktree_fixture(home.path(), wt.path());

    let out = conductor_cmd(home.path())
        .args(["workflow", "run", "hello", "r", "wt", "--background"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_id = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(
        run_id.len(),
        26,
        "stdout must be just the run ID, got {run_id:?}"
    );

    let conn = conductor_core::db::open_database(&home.path().join("conductor.db")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        let status: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                [&run_id],
                |r| r.get(0),
            )
            .unwrap();
        if !matches!(status.as_str(), "pending" | "running") || std::time::Instant::now() > deadline
        {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert_eq!(status, "completed");
}

/// An unknown worktree is reported with its cause, not as a silent child exit.
#[cfg(unix)]
#[test]
fn workflow_run_background_reports_unknown_worktree() {
    let home = tempfile::tempdir().unwrap();
    let wt = setup_valid_workflow_fixture();
    register_worktree_fixture(home.path(), wt.path());

    conductor_cmd(home.path())
        .args([
            "workflow",
            "run",
            "hello",
            "r",
            "no-such-wt",
            "--background",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worktree not found"));
}

/// Insert a workflow run with the given status/timestamps into the test DB; returns its id.
fn insert_wait_run(
    home: &std::path::Path,
    status: &str,
    started_offset: &str,
    lease: &str,
) -> String {
    let conn = conductor_core::db::open_database(&home.join("conductor.db")).unwrap();
    let parent = conductor_core::agent::AgentManager::new(&conn)
        .create_run(None, "workflow", None)
        .unwrap();
    let run = conductor_core::workflow::create_workflow_run(
        &conn, "wait-wf", None, &parent.id, false, "manual", None,
    )
    .unwrap();
    conn.execute(
        &format!(
            "UPDATE workflow_runs SET status = ?1, \
             started_at = strftime('%Y-%m-%dT%H:%M:%SZ','now','{started_offset}'), \
             lease_until = {lease} WHERE id = ?2"
        ),
        rusqlite::params![status, run.id],
    )
    .unwrap();
    if status == "running" {
        // Keep the startup reaper from failing a running run that has no active step.
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, position, status, started_at) \
             VALUES (?1, ?2, 'busy', 'actor', 0, 'running', \
                     strftime('%Y-%m-%dT%H:%M:%SZ','now','-1 hours'))",
            rusqlite::params![conductor_core::new_id(), run.id],
        )
        .unwrap();
    }
    run.id.clone()
}

#[test]
fn workflow_wait_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let cases = [
        (
            "completed",
            "NULL",
            "-1 hours",
            vec![],
            Some(0),
            "outcome: completed",
        ),
        (
            "failed",
            "NULL",
            "-1 hours",
            vec![],
            Some(1),
            "outcome: failed",
        ),
        (
            "cancelled",
            "NULL",
            "-1 hours",
            vec![],
            Some(1),
            "outcome: cancelled",
        ),
        (
            "running",
            "datetime('now','+60 seconds')",
            "-1 hours",
            vec!["--timeout", "0"],
            Some(2),
            "outcome: timeout",
        ),
        (
            "running",
            "datetime('now','-300 seconds')",
            "-1 hours",
            vec!["--timeout", "30", "--stall-after", "10"],
            Some(3),
            "outcome: stalled",
        ),
    ];
    for (status, lease, started, extra, code, want) in cases {
        let id = insert_wait_run(home.path(), status, started, lease);
        conductor_cmd(home.path())
            .args(["workflow", "wait", &id])
            .args(&extra)
            .assert()
            .code(code.unwrap())
            .stdout(predicate::str::contains(want));
    }
}

#[test]
fn workflow_wait_json_output() {
    let home = tempfile::tempdir().unwrap();
    let id = insert_wait_run(home.path(), "failed", "-1 hours", "NULL");
    let out = conductor_cmd(home.path())
        .args(["workflow", "wait", &id, "--json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["outcome"], "failed");
    assert_eq!(v["run_id"], id);
}

#[test]
fn workflow_wait_unknown_run_fails() {
    let home = tempfile::tempdir().unwrap();
    conductor_cmd(home.path())
        .args(["workflow", "wait", "nope"])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// `conductor session hook`
// ---------------------------------------------------------------------------

/// Run `conductor session hook` with `stdin` as the payload. `claude_pid` sets
/// `CLAUDE_PID` (None removes it).
fn run_hook(
    home: &std::path::Path,
    attended: &str,
    claude_pid: Option<u32>,
    stdin: &[u8],
) -> std::process::Output {
    let mut cmd = assert_cmd::Command::from_std(conductor_cmd(home));
    cmd.args(["session", "hook"])
        .env("CLAUDE_CODE_SESSION_ATTENDED", attended)
        .env_remove("CONDUCTOR_SESSION_HOOK_DEBUG")
        .env_remove("CONDUCTOR_DB_PATH")
        .write_stdin(stdin.to_vec());
    match claude_pid {
        Some(pid) => cmd.env("CLAUDE_PID", pid.to_string()),
        None => cmd.env_remove("CLAUDE_PID"),
    };
    cmd.output().unwrap()
}

fn payload(name: &str, extra: serde_json::Value) -> Vec<u8> {
    let mut v = serde_json::json!({
        "hook_event_name": name,
        "session_id": "sess-1",
        "cwd": "/tmp/ws/hook-wt",
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    serde_json::to_vec(&v).unwrap()
}

fn assert_silent_success(out: &std::process::Output) {
    assert!(out.status.success(), "exit status: {:?}", out.status);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
}

fn run_status(conn: &rusqlite::Connection) -> String {
    conn.query_row(
        "SELECT status FROM agent_runs WHERE session_id = 'sess-1'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn session_hook_malformed_json_exits_zero_silently() {
    let dir = tempfile::tempdir().unwrap();
    conductor_core::test_helpers::seed_hook_db(dir.path());
    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        b"{not json SessionStart",
    );
    assert_silent_success(&out);
}

#[test]
fn session_hook_unattended_creates_no_database() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_hook(
        dir.path(),
        "0",
        Some(std::process::id()),
        &payload("SessionStart", serde_json::json!({})),
    );
    assert_silent_success(&out);
    assert!(!dir.path().join("conductor.db").exists());
}

#[test]
fn session_hook_without_database_creates_none() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload("SessionStart", serde_json::json!({})),
    );
    assert_silent_success(&out);
    assert!(!dir.path().join("conductor.db").exists());
}

#[test]
fn session_hook_records_start_and_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(dir.path());

    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload("SessionStart", serde_json::json!({"source": "startup"})),
    );
    assert_silent_success(&out);
    assert_eq!(run_status(&conn), "idle");

    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload(
            "UserPromptSubmit",
            serde_json::json!({"prompt": "hello", "prompt_id": "p1"}),
        ),
    );
    assert_silent_success(&out);
    let (status, wt, pid, runtime): (String, String, i64, String) = conn
        .query_row(
            "SELECT status, worktree_id, subprocess_pid, runtime FROM agent_runs WHERE session_id = 'sess-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(status, "running");
    assert_eq!(wt, "w1");
    assert_eq!(pid, i64::from(std::process::id()));
    assert_eq!(runtime, "interactive");
}

#[test]
fn session_hook_oversized_post_tool_use_still_flips_to_running() {
    let dir = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(dir.path());
    run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload("SessionStart", serde_json::json!({})),
    );
    run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload(
            "PermissionRequest",
            serde_json::json!({"tool_name": "Bash"}),
        ),
    );
    assert_eq!(run_status(&conn), "waiting_for_feedback");

    // tool_response comes last and is several MiB, so the 1 MiB cap cuts the JSON.
    let mut big = r#"{"hook_event_name":"PostToolUse","session_id":"sess-1","cwd":"/tmp/ws/hook-wt","tool_name":"Bash","tool_response":{"stdout":""#
        .as_bytes()
        .to_vec();
    big.extend(std::iter::repeat_n(b'x', 3 * 1024 * 1024));
    big.extend_from_slice(br#""}}"#);

    let out = run_hook(dir.path(), "1", Some(std::process::id()), &big);
    assert_silent_success(&out);
    assert_eq!(run_status(&conn), "running");
    let fb: String = conn
        .query_row("SELECT status FROM feedback_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fb, "responded");
}

#[test]
fn session_hook_debug_env_reports_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    conductor_core::test_helpers::seed_hook_db(dir.path());
    let mut cmd = assert_cmd::Command::from_std(conductor_cmd(dir.path()));
    cmd.args(["session", "hook"])
        .env("CLAUDE_CODE_SESSION_ATTENDED", "1")
        .env("CONDUCTOR_SESSION_HOOK_DEBUG", "1")
        .write_stdin(&b"SessionStart {not json"[..]);
    let out = cmd.output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[test]
fn session_open_is_listed_but_session_hook_is_hidden_from_help() {
    let dir = tempfile::tempdir().unwrap();
    conductor_cmd(dir.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("session"));
    conductor_cmd(dir.path())
        .args(["session", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("open"))
        .stdout(predicate::str::contains("take-over"))
        .stdout(predicate::str::contains("hook").not());
}

#[test]
fn session_hook_spools_when_database_is_locked_then_drains_on_next_hook() {
    let dir = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(dir.path());
    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload("SessionStart", serde_json::json!({})),
    );
    assert_silent_success(&out);

    // Another connection holds the write lock.
    let holder = rusqlite::Connection::open(dir.path().join("conductor.db")).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = std::time::Instant::now();
    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload(
            "PermissionRequest",
            serde_json::json!({"tool_name": "Bash", "prompt_id": "p1"}),
        ),
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "hook blocked for {:?}",
        started.elapsed()
    );
    assert_silent_success(&out);
    let spool = dir.path().join("session-spool/sess-1.jsonl");
    let text = std::fs::read_to_string(&spool).unwrap();
    assert_eq!(text.lines().count(), 1);
    let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(line["v"], 1);
    assert_eq!(line["hook_event_name"], "PermissionRequest");

    holder.execute_batch("ROLLBACK").unwrap();

    // The next hook drains the spooled event first, then applies its own.
    let out = run_hook(
        dir.path(),
        "1",
        Some(std::process::id()),
        &payload(
            "PostToolUse",
            serde_json::json!({"tool_name": "Bash", "prompt_id": "p1"}),
        ),
    );
    assert_silent_success(&out);
    assert_eq!(run_status(&conn), "running");
    let fb: Vec<String> = conn
        .prepare("SELECT status FROM feedback_requests")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(fb, vec!["responded".to_string()]);
    assert!(!spool.exists());
}

conductor_core::isolate_test_home!();

#[test]
fn session_hook_ignores_main_checkout_and_inactive_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(dir.path());
    conductor_core::test_helpers::insert_test_worktree(
        &conn,
        "w2",
        "r1",
        "merged-wt",
        "/tmp/ws/merged-wt",
    );
    conn.execute("UPDATE worktrees SET status = 'merged' WHERE id = 'w2'", [])
        .unwrap();

    for cwd in ["/tmp/hook-repo", "/tmp/ws/merged-wt"] {
        for name in ["SessionStart", "UserPromptSubmit"] {
            let out = run_hook(
                dir.path(),
                "1",
                Some(std::process::id()),
                &payload(name, serde_json::json!({"cwd": cwd})),
            );
            assert_silent_success(&out);
        }
    }
    assert_eq!(
        conductor_core::test_helpers::count_rows(&conn, "agent_runs"),
        0
    );
    assert_eq!(
        conductor_core::test_helpers::count_rows(&conn, "agent_run_events"),
        0
    );
}

#[test]
fn session_hook_uses_claude_pid_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(dir.path());
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let out = run_hook(
        dir.path(),
        "1",
        Some(child.id()),
        &payload("SessionStart", serde_json::json!({})),
    );
    let pid: i64 = conn
        .query_row(
            "SELECT subprocess_pid FROM agent_runs WHERE session_id = 'sess-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert_silent_success(&out);
    assert_eq!(pid, i64::from(child.id()));
}

#[test]
fn session_hook_falls_back_to_parent_pid() {
    let dir = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(dir.path());
    let out = run_hook(
        dir.path(),
        "1",
        None,
        &payload("SessionStart", serde_json::json!({})),
    );
    assert_silent_success(&out);
    let pid: i64 = conn
        .query_row(
            "SELECT subprocess_pid FROM agent_runs WHERE session_id = 'sess-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pid, i64::from(std::process::id()));
}
