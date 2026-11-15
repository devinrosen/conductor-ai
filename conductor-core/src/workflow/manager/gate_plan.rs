//! Read the plan a waiting gate asks a person to review (#3385).
//!
//! `plan_path` comes from the structured output of the step before the gate, as in
//! `workflow wait`. That path is untrusted step output, so the file is read only if it
//! resolves inside the run's worktree or that worktree's own git dir.

use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{named_params, Connection, OptionalExtension};
use serde::Serialize;

use crate::error::{ConductorError, Result};

use super::queries::{find_waiting_gate, get_workflow_run};
use super::wait::preceding_plan_path;

/// Largest part of a plan the route returns.
pub const GATE_PLAN_MAX_BYTES: u64 = 256 * 1024;

/// What the DB says about a waiting gate's plan; input to [`read_gate_plan`].
#[derive(Debug, Clone)]
pub struct GatePlanTarget {
    pub run_id: String,
    pub step_id: String,
    pub step_name: String,
    pub plan_path: String,
    /// The run's worktree path (or the repo's local path for a repo-targeted run).
    pub root: PathBuf,
}

/// A waiting gate's plan file.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct GatePlan {
    pub run_id: String,
    pub step_id: String,
    pub step_name: String,
    pub plan_path: String,
    pub content: String,
    /// Size of the file on disk.
    pub size_bytes: u64,
    /// `content` holds only the first `GATE_PLAN_MAX_BYTES` of the file.
    pub truncated: bool,
}

/// A 404-class "there is no plan to show" error carrying a human-readable reason.
fn unavailable(reason: impl Into<String>) -> ConductorError {
    ConductorError::GatePlanUnavailable {
        reason: reason.into(),
    }
}

/// An I/O failure that keeps the `io::ErrorKind`. The path and OS cause go to the log
/// only; the message is what the web layer sends to the client, so it names no path.
fn io_failure(what: &str, path: &Path, e: std::io::Error) -> ConductorError {
    tracing::warn!(path = %path.display(), error = %e, "gate plan: {what}");
    ConductorError::Io(std::io::Error::new(e.kind(), what.to_string()))
}

/// Find the run's waiting gate, its `plan_path` and the directory the path must sit in.
/// Database work only; no filesystem access.
pub fn resolve_gate_plan_target(conn: &Connection, run_id: &str) -> Result<GatePlanTarget> {
    let step = find_waiting_gate(conn, run_id)?
        .ok_or_else(|| unavailable(format!("run {run_id} has no waiting gate")))?;
    let plan_path = preceding_plan_path(conn, &step)?.ok_or_else(|| {
        unavailable(format!(
            "the step before gate {} in run {run_id} emitted no plan_path",
            step.step_name
        ))
    })?;
    let run = get_workflow_run(conn, run_id)?
        .ok_or_else(|| ConductorError::WorkflowRunNotFound { id: run_id.into() })?;
    let root: Option<String> = if let Some(wt) = run.worktree_id.as_deref() {
        conn.query_row(
            "SELECT path FROM worktrees WHERE id = :id",
            named_params![":id": wt],
            |r| r.get(0),
        )
        .optional()?
    } else if let Some(repo) = run.repo_id.as_deref() {
        conn.query_row(
            "SELECT local_path FROM repos WHERE id = :id",
            named_params![":id": repo],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    let root = root.ok_or_else(|| {
        unavailable(format!(
            "run {run_id} has no worktree or repo on record to read from"
        ))
    })?;
    Ok(GatePlanTarget {
        run_id: run_id.to_string(),
        step_id: step.id,
        step_name: step.step_name,
        plan_path,
        root: PathBuf::from(root),
    })
}

/// The root's own git dir (`<main>/.git/worktrees/<name>` for a linked worktree), never
/// the common dir, which holds the repo config and every other worktree's files.
///
/// A failure is logged, not returned: the worktree itself stays readable, but a plan in
/// the git dir then reads as outside the allow-list, so the log is how to find out why.
fn own_git_dir(root: &Path) -> Option<PathBuf> {
    let out = match std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(root = %root.display(), error = %e, "gate plan: could not run git to find the git dir");
            return None;
        }
    };
    if !out.status.success() {
        tracing::warn!(
            root = %root.display(),
            status = %out.status,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "gate plan: git rev-parse --absolute-git-dir failed"
        );
        return None;
    }
    let dir = match String::from_utf8(out.stdout) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(root = %root.display(), error = %e, "gate plan: git dir is not UTF-8");
            return None;
        }
    };
    match std::fs::canonicalize(dir.trim()) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(root = %root.display(), git_dir = dir.trim(), error = %e, "gate plan: could not canonicalize the git dir");
            None
        }
    }
}

/// Read the plan file, refusing a path outside the worktree and its git dir.
pub fn read_gate_plan(target: &GatePlanTarget) -> Result<GatePlan> {
    let root = std::fs::canonicalize(&target.root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            tracing::warn!(path = %target.root.display(), "gate plan: worktree is not on disk");
            unavailable("the run's worktree is not on disk")
        } else {
            io_failure("could not resolve the worktree", &target.root, e)
        }
    })?;
    let mut allowed = vec![root.clone()];
    if let Some(git_dir) = own_git_dir(&root) {
        allowed.push(git_dir);
    }
    read_plan_within(target, &root, &allowed)
}

fn read_plan_within(target: &GatePlanTarget, root: &Path, allowed: &[PathBuf]) -> Result<GatePlan> {
    let raw = Path::new(&target.plan_path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    // Resolves `..` and symlinks, so an escape through either is caught below.
    let resolved = std::fs::canonicalize(&joined).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            tracing::warn!(path = %joined.display(), "gate plan: plan file does not exist");
            unavailable("the plan file does not exist")
        } else {
            io_failure("could not resolve the plan file", &joined, e)
        }
    })?;
    if !allowed.iter().any(|a| resolved.starts_with(a)) {
        return Err(ConductorError::PathNotAllowed {
            path: target.plan_path.clone(),
        });
    }
    let file = std::fs::File::open(&resolved)
        .map_err(|e| io_failure("could not open the plan file", &resolved, e))?;
    let meta = file
        .metadata()
        .map_err(|e| io_failure("could not stat the plan file", &resolved, e))?;
    if !meta.is_file() {
        return Err(ConductorError::PathNotAllowed {
            path: target.plan_path.clone(),
        });
    }
    let size_bytes = meta.len();
    let mut buf = Vec::new();
    file.take(GATE_PLAN_MAX_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| io_failure("could not read the plan file", &resolved, e))?;
    Ok(GatePlan {
        run_id: target.run_id.clone(),
        step_id: target.step_id.clone(),
        step_name: target.step_name.clone(),
        plan_path: target.plan_path.clone(),
        content: String::from_utf8_lossy(&buf).into_owned(),
        size_bytes,
        truncated: size_bytes > GATE_PLAN_MAX_BYTES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(root: &Path, plan_path: &str) -> GatePlanTarget {
        GatePlanTarget {
            run_id: "r".into(),
            step_id: "g".into(),
            step_name: "human_review".into(),
            plan_path: plan_path.into(),
            root: root.to_path_buf(),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .unwrap();
        assert!(st.status.success(), "git {args:?}: {st:?}");
    }

    /// A main repo and a linked worktree; returns (tempdir, main, worktree).
    fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let wt = tmp.path().join("wt");
        git(
            &main,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "b"],
        );
        (tmp, main, wt)
    }

    #[test]
    fn plan_in_the_worktrees_git_dir_is_returned() {
        let (_tmp, main, wt) = repo_with_worktree();
        let git_dir = own_git_dir(&wt).unwrap();
        assert!(git_dir.starts_with(std::fs::canonicalize(main.join(".git")).unwrap()));
        let plan = git_dir.join("PLAN.md");
        std::fs::write(&plan, "# plan\n").unwrap();
        let got = read_gate_plan(&target(&wt, plan.to_str().unwrap())).unwrap();
        assert_eq!(got.content, "# plan\n");
        assert!(!got.truncated);
        assert_eq!(got.size_bytes, 7);
    }

    #[test]
    fn plan_in_the_worktree_is_returned_absolute_or_relative() {
        let (_tmp, _main, wt) = repo_with_worktree();
        std::fs::write(wt.join("PLAN.md"), "hi").unwrap();
        for p in [
            wt.join("PLAN.md").to_str().unwrap().to_string(),
            "PLAN.md".into(),
        ] {
            assert_eq!(read_gate_plan(&target(&wt, &p)).unwrap().content, "hi");
        }
    }

    #[test]
    fn paths_outside_are_refused() {
        let (tmp, main, wt) = repo_with_worktree();
        let outside = tmp.path().join("secret.txt");
        std::fs::write(&outside, "s").unwrap();
        // Sibling dir whose name starts with the worktree's name.
        let sibling = tmp.path().join("wt-evil");
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(sibling.join("PLAN.md"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, wt.join("link.md")).unwrap();
        let cases = [
            outside.to_str().unwrap().to_string(),
            "../secret.txt".into(),
            "link.md".into(),
            sibling.join("PLAN.md").to_str().unwrap().to_string(),
            main.join(".git/config").to_str().unwrap().to_string(),
        ];
        for p in cases {
            let err = read_gate_plan(&target(&wt, &p)).unwrap_err();
            assert!(
                matches!(err, ConductorError::PathNotAllowed { .. }),
                "{p}: {err:?}"
            );
        }
    }

    #[test]
    fn directory_and_missing_file() {
        let (_tmp, _main, wt) = repo_with_worktree();
        assert!(matches!(
            read_gate_plan(&target(&wt, ".")).unwrap_err(),
            ConductorError::PathNotAllowed { .. }
        ));
        assert!(matches!(
            read_gate_plan(&target(&wt, "nope.md")).unwrap_err(),
            ConductorError::GatePlanUnavailable { .. }
        ));
    }

    #[test]
    fn oversize_plan_is_truncated() {
        let (_tmp, _main, wt) = repo_with_worktree();
        let big = vec![b'a'; GATE_PLAN_MAX_BYTES as usize + 10];
        std::fs::write(wt.join("PLAN.md"), &big).unwrap();
        let got = read_gate_plan(&target(&wt, "PLAN.md")).unwrap();
        assert!(got.truncated);
        assert_eq!(got.content.len() as u64, GATE_PLAN_MAX_BYTES);
        assert_eq!(got.size_bytes, GATE_PLAN_MAX_BYTES + 10);
    }

    /// A run with a plan step (position 0, `structured_output`) and a waiting gate (position 1).
    fn run_waiting_at_gate(conn: &Connection, structured_output: Option<&str>) {
        crate::test_helpers::insert_test_wf_run(conn, "r", "wf", "waiting", None);
        conn.execute(
            "UPDATE workflow_runs SET worktree_id = 'w1' WHERE id = 'r'",
            [],
        )
        .unwrap();
        crate::test_helpers::insert_test_wf_step(
            conn,
            "plan",
            "r",
            "plan",
            "actor",
            "completed",
            0,
            "2026-01-01T00:00:00Z",
            None,
            None,
        );
        conn.execute(
            "UPDATE workflow_run_steps SET structured_output = ?1 WHERE id = 'plan'",
            rusqlite::params![structured_output],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_run_steps \
             (id, workflow_run_id, step_name, role, status, position, gate_type) \
             VALUES ('gate', 'r', 'human_review', 'gate', 'waiting', 1, 'human_review')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn gate_without_plan_path_is_not_found() {
        for output in [
            None,
            Some(r#"{"summary":"x"}"#),
            Some(r#"{"plan_path":""}"#),
        ] {
            let conn = crate::test_helpers::setup_db();
            run_waiting_at_gate(&conn, output);
            assert!(
                matches!(
                    resolve_gate_plan_target(&conn, "r").unwrap_err(),
                    ConductorError::GatePlanUnavailable { .. }
                ),
                "{output:?}"
            );
        }
    }

    #[test]
    fn deleted_worktree_is_not_found() {
        // The worktree row is gone: resolving finds no root.
        let conn = crate::test_helpers::setup_db();
        run_waiting_at_gate(&conn, Some(r#"{"plan_path":"PLAN.md"}"#));
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute("DELETE FROM worktrees WHERE id = 'w1'", [])
            .unwrap();
        assert!(matches!(
            resolve_gate_plan_target(&conn, "r").unwrap_err(),
            ConductorError::GatePlanUnavailable { .. }
        ));

        // The row is there but the directory was removed from disk.
        let conn = crate::test_helpers::setup_db();
        run_waiting_at_gate(&conn, Some(r#"{"plan_path":"PLAN.md"}"#));
        let t = resolve_gate_plan_target(&conn, "r").unwrap();
        assert_eq!(t.plan_path, "PLAN.md");
        let tmp = tempfile::tempdir().unwrap();
        let gone = GatePlanTarget {
            root: tmp.path().join("deleted"),
            ..t
        };
        assert!(matches!(
            read_gate_plan(&gone).unwrap_err(),
            ConductorError::GatePlanUnavailable { .. }
        ));
    }

    #[test]
    fn run_with_no_worktree_or_repo_is_unavailable() {
        let conn = crate::test_helpers::setup_db();
        run_waiting_at_gate(&conn, Some(r#"{"plan_path":"PLAN.md"}"#));
        conn.execute(
            "UPDATE workflow_runs SET worktree_id = NULL WHERE id = 'r'",
            [],
        )
        .unwrap();
        let err = resolve_gate_plan_target(&conn, "r").unwrap_err();
        assert!(
            matches!(err, ConductorError::GatePlanUnavailable { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn repo_targeted_run_uses_the_repos_local_path() {
        let conn = crate::test_helpers::setup_db();
        run_waiting_at_gate(&conn, Some(r#"{"plan_path":"PLAN.md"}"#));
        let local: String = conn
            .query_row("SELECT local_path FROM repos LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let repo_id: String = conn
            .query_row("SELECT id FROM repos LIMIT 1", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET worktree_id = NULL, repo_id = ?1 WHERE id = 'r'",
            [&repo_id],
        )
        .unwrap();
        let t = resolve_gate_plan_target(&conn, "r").unwrap();
        assert_eq!(t.root, PathBuf::from(local));

        // The repo row is gone: no root to read from.
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute("DELETE FROM repos WHERE id = ?1", [&repo_id])
            .unwrap();
        assert!(matches!(
            resolve_gate_plan_target(&conn, "r").unwrap_err(),
            ConductorError::GatePlanUnavailable { .. }
        ));
    }

    #[test]
    fn unreadable_plan_file_keeps_its_kind_and_hides_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, _main, wt) = repo_with_worktree();
        let plan = wt.join("PLAN.md");
        std::fs::write(&plan, "x").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&plan).is_ok() {
            return; // running as root: permissions don't apply
        }
        let err = read_gate_plan(&target(&wt, "PLAN.md")).unwrap_err();
        match &err {
            ConductorError::Io(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied)
            }
            other => panic!("expected Io, got {other:?}"),
        }
        let shown = err.to_string();
        assert!(!shown.contains(wt.to_str().unwrap()), "{shown}");
    }

    #[test]
    fn no_waiting_gate_is_not_found() {
        let conn = crate::test_helpers::setup_db();
        crate::test_helpers::insert_test_wf_run(&conn, "r", "wf", "running", None);
        assert!(matches!(
            resolve_gate_plan_target(&conn, "r").unwrap_err(),
            ConductorError::GatePlanUnavailable { .. }
        ));
    }
}
