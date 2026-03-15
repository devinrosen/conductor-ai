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
