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

#[test]
fn workflow_validate_all_pass() {
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
    // Minimal valid workflow in the custom DSL format
    std::fs::write(
        wf_dir.join("hello.wf"),
        "workflow hello {\n  meta {\n    trigger = \"manual\"\n    targets = [\"worktree\"]\n  }\n  script greet {\n    run = \".conductor/scripts/greet.sh\"\n  }\n}\n",
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
        .success()
        .stdout(predicate::str::contains("PASS  hello"));
}

#[test]
fn workflow_validate_single_pass() {
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
