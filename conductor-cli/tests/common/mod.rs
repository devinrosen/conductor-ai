//! Shared helpers for tests that spawn the `conductor` binary.
//!
//! Rule: a test that spawns the binary builds the command with
//! [`conductor_cmd`] and sets any variable it depends on explicitly. The
//! environment starts empty, so output never depends on which tools or
//! variables the developer's shell has (#3240, after #3235 passed locally and
//! failed only on CI).
#![allow(dead_code)]

use assert_cmd::prelude::*;
use std::path::Path;
use std::process::Command;

/// System directories only: `gh`, `tmux` and `claude` are absent, as on CI
/// runners, while `git`, `sh` and `sleep` remain.
pub const PINNED_PATH: &str = "/usr/bin:/bin";

/// Variables copied from the test process when set. Anything else must be set
/// explicitly by the test.
const PASSTHROUGH: &[&str] = &[
    // Scratch space for the binary; without it macOS falls back to /tmp.
    "TMPDIR",
    // Coverage runs (`cargo llvm-cov`) need the child binary's profile output.
    "LLVM_PROFILE_FILE",
];

/// Clear `cmd`'s environment and set the fixed test environment: `PATH` is
/// `extra_path` followed by [`PINNED_PATH`], and `HOME` and `CONDUCTOR_HOME`
/// are `home`.
pub fn apply_fixed_env<'a>(
    cmd: &'a mut Command,
    home: &Path,
    extra_path: &[&Path],
) -> &'a mut Command {
    let mut path: Vec<String> = extra_path.iter().map(|p| p.display().to_string()).collect();
    path.push(PINNED_PATH.to_string());
    cmd.env_clear()
        .env("PATH", path.join(":"))
        .env("HOME", home)
        .env("CONDUCTOR_HOME", home);
    for (k, v) in std::env::vars_os() {
        let name = k.to_string_lossy();
        if PASSTHROUGH.contains(&name.as_ref()) || name.starts_with("CARGO_LLVM_COV") {
            cmd.env(k, v);
        }
    }
    cmd
}

/// Build a `conductor` command with the fixed environment and `CONDUCTOR_HOME`
/// pointed at `home`, so each test gets an isolated temp database and config.
pub fn conductor_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("conductor").unwrap();
    apply_fixed_env(&mut cmd, home, &[]);
    cmd
}
