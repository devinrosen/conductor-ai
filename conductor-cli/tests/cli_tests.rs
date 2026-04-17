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
