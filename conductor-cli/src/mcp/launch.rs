//! Starting workflow runs in their own process (#3124).
//!
//! `conductor_run_workflow` and `conductor_resume_run` validate and resolve the
//! target in the server, then exec the installed `conductor` through its hidden
//! `workflow launch-detached` command. The run is owned by that process, so it
//! uses the binary installed when it started and survives the server restarting.

use anyhow::Context;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::background::{spawn_detached, DetachedChild, LaunchSpec};

static RUN_EXE: OnceLock<PathBuf> = OnceLock::new();

/// Pin the path runs are started from, at server start. On Linux
/// `current_exe()` reads `/proc/self/exe`, which resolves to `"... (deleted)"`
/// once a reinstall replaced the file; the path captured at start still names
/// the installed binary.
pub(super) fn pin_run_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = RUN_EXE.set(exe);
    }
}

/// The `conductor` binary runs are started with.
#[cfg_attr(test, allow(dead_code))]
fn run_exe() -> anyhow::Result<PathBuf> {
    match RUN_EXE.get() {
        Some(exe) => Ok(exe.clone()),
        None => std::env::current_exe()
            .context("failed to locate the conductor executable to launch the run"),
    }
}

/// Start the run `spec` describes in a detached process and wait for it to
/// report its run ID. A thread reaps the child when it exits, so the long-lived
/// server doesn't collect zombies.
pub(super) fn launch(spec: &LaunchSpec) -> anyhow::Result<LaunchedRun> {
    #[cfg(not(test))]
    let exe = run_exe()?;
    // Unit tests run in the test harness binary, which must never be exec'd as
    // `conductor`. Pointing at a missing path makes any test that reaches the
    // spawn fail with a clear error instead.
    #[cfg(test)]
    let exe = PathBuf::from("/nonexistent/conductor-under-test");
    launch_with(&exe, spec)
}

fn launch_with(exe: &std::path::Path, spec: &LaunchSpec) -> anyhow::Result<LaunchedRun> {
    let payload = serde_json::to_vec(spec).context("failed to serialize the launch spec")?;
    let DetachedChild {
        run_id,
        build,
        mut child,
    } = spawn_detached(
        exe,
        |fd| {
            vec![
                "workflow".into(),
                "launch-detached".into(),
                "--notify-fd".into(),
                fd.to_string().into(),
            ]
        },
        Some(&payload),
    )?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(LaunchedRun { run_id, build })
}

pub(super) struct LaunchedRun {
    pub run_id: String,
    /// The child's `<version> <sha>`, if it reported one.
    pub build: Option<String>,
}

impl LaunchedRun {
    /// A `build:` line for the tool result, or nothing when the child didn't report one.
    pub fn build_line(&self) -> String {
        match &self.build {
            Some(b) => format!("\nbuild: conductor {b}"),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec::Resume {
            run_id: "r".into(),
            from_step: None,
            model: None,
            db_path: None,
        }
    }

    #[test]
    fn launch_reports_a_missing_binary_instead_of_running_in_process() {
        let err = launch(&spec()).err().expect("must fail").to_string();
        assert!(err.contains("Failed to start"), "{err}");
    }

    #[test]
    fn build_line_is_empty_without_a_reported_build() {
        let r = LaunchedRun {
            run_id: "x".into(),
            build: None,
        };
        assert_eq!(r.build_line(), "");
        let r = LaunchedRun {
            run_id: "x".into(),
            build: Some("0.19.0 abc1234".into()),
        };
        assert_eq!(r.build_line(), "\nbuild: conductor 0.19.0 abc1234");
    }
}
