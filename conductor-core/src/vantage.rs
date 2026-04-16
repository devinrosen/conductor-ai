use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;

use crate::error::{ConductorError, Result};
use crate::issue_source::{IssueSourceManager, VantageConfig};
use crate::tickets::TicketInput;

/// Conductor pipeline statuses that should be synced into Conductor.
/// Pre-ready states (pending_audit, audited, enriching) are excluded —
/// those deliverables aren't actionable yet.
const ACTIONABLE_CONDUCTOR_STATUSES: &[&str] =
    &["ready", "dispatched", "running", "completed", "failed"];

/// Vantage conductor statuses that represent a terminal (done) state.
/// A blocked ticket whose parent has one of these statuses is considered
/// "approved" and can be unlocked in the dependency tree.
///
/// This is the single source of truth — expose it via the REST API
/// (`GET /api/vantage/terminal-statuses`) so the frontend never needs a
/// hardcoded duplicate.
const TERMINAL_CONDUCTOR_STATUSES: &[&str] = &["merged", "pr_approved", "released"];

/// Returns the list of terminal Vantage conductor statuses.
///
/// These statuses indicate that a deliverable has reached a final approved
/// state. Use this instead of hardcoding the list in any other layer.
pub fn terminal_conductor_statuses() -> &'static [&'static str] {
    TERMINAL_CONDUCTOR_STATUSES
}

/// Sync deliverables from a Vantage SDLC project, filtered to those whose
/// `codebase` field matches the given `repo_slug`.
/// Returns a list of normalized TicketInputs ready for upsert.
pub fn sync_vantage_deliverables(
    project_id: &str,
    sdlc_root: &str,
    repo_slug: &str,
) -> Result<Vec<TicketInput>> {
    let output = run_sdlc(
        sdlc_root,
        &["deliverable", "list", "--project", project_id, "--json"],
    )?;

    let json_str = String::from_utf8_lossy(&output.stdout);
    let items: Vec<serde_json::Value> = serde_json::from_str(&json_str).map_err(|e| {
        ConductorError::TicketSync(format!("failed to parse sdlc list output: {e}"))
    })?;

    tracing::info!(
        "Vantage sync: {} total deliverables in project {}, filtering for codebase={repo_slug:?}",
        items.len(),
        project_id,
    );

    // Pre-read all deliverable frontmatter files once to avoid N file reads in the loop.
    let frontmatter_cache = preload_deliverable_frontmatter(sdlc_root);

    let mut tickets = Vec::with_capacity(items.len());
    let mut skipped_codebase = 0usize;
    let mut skipped_mode_or_status = 0usize;
    for item in &items {
        let id = item["id"].as_str().unwrap_or("");
        if id.is_empty() {
            continue;
        }
        // Only sync deliverables whose codebase matches this repo
        let codebase = item["codebase"].as_str().unwrap_or("");
        if codebase != repo_slug {
            skipped_codebase += 1;
            tracing::debug!("Vantage sync: skipping {id} (codebase={codebase:?} != {repo_slug:?})");
            continue;
        }
        // Only sync conductor-mode deliverables in actionable pipeline states.
        // The sdlc CLI may not include these fields in JSON output, so fall back
        // to the pre-loaded YAML frontmatter cache.
        let (exec_mode, conductor_status) = resolve_conductor_fields(item, id, &frontmatter_cache);
        if exec_mode != "conductor"
            || !ACTIONABLE_CONDUCTOR_STATUSES.contains(&conductor_status.as_str())
        {
            skipped_mode_or_status += 1;
            tracing::debug!(
                "Vantage sync: skipping {id} (execution_mode={exec_mode:?}, conductor.status={conductor_status:?})"
            );
            continue;
        }
        let status = item["status"].as_str().unwrap_or("");
        tracing::debug!("Vantage sync: matched {id} (codebase={codebase:?}, status={status:?}, conductor.status={conductor_status:?})");
        // Use list data directly when body is present; fetch full detail only if missing.
        let ticket = if item["body"].as_str().is_some_and(|b| !b.is_empty()) {
            parse_vantage_deliverable(item)
        } else {
            match fetch_vantage_deliverable(id, sdlc_root) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Failed to fetch Vantage deliverable {id}: {e}");
                    continue;
                }
            }
        };
        tickets.push(ticket);
    }

    let total_skipped = skipped_codebase + skipped_mode_or_status;
    if tickets.is_empty() && total_skipped > 0 {
        tracing::warn!(
            project_id,
            repo_slug,
            skipped_codebase,
            skipped_mode_or_status,
            "Vantage sync: 0 deliverables matched for repo {repo_slug:?} — \
             {skipped_codebase} skipped (codebase mismatch), \
             {skipped_mode_or_status} skipped (execution_mode != 'conductor' or pre-ready status). \
             Check that deliverables have codebase={repo_slug:?} and execution_mode='conductor'."
        );
    } else {
        tracing::info!(
            "Vantage sync: matched {} deliverables, skipped {total_skipped} (filtered out)",
            tickets.len(),
        );
    }

    Ok(tickets)
}

/// Fetch a single Vantage deliverable by ID.
pub fn fetch_vantage_deliverable(deliverable_id: &str, sdlc_root: &str) -> Result<TicketInput> {
    // `--json` flag before `--` terminator; `--` prevents deliverable_id being parsed as a flag.
    let output = run_sdlc(
        sdlc_root,
        &["deliverable", "get", "--json", "--", deliverable_id],
    )?;

    let json_str = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| ConductorError::TicketSync(format!("failed to parse sdlc get output: {e}")))?;

    if value.is_null() || value["id"].is_null() {
        return Err(ConductorError::TicketNotFound {
            id: deliverable_id.to_string(),
        });
    }

    Ok(parse_vantage_deliverable(&value))
}

/// Pre-read all `.md` files from `{sdlc_root}/deliverables/` and parse their
/// frontmatter into `(execution_mode, conductor.status)` pairs, keyed by
/// deliverable ID (filename stem). This avoids N individual file reads inside
/// the sync loop.
fn preload_deliverable_frontmatter(sdlc_root: &str) -> HashMap<String, (String, String)> {
    let mut cache = HashMap::new();
    if sdlc_root.is_empty() {
        return cache;
    }
    let dir = Path::new(sdlc_root).join("deliverables");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(
                "Vantage sync: could not read deliverables dir {}: {e}",
                dir.display()
            );
            return cache;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Some(fields) = parse_conductor_frontmatter(&contents) {
                cache.insert(id, fields);
            }
        }
    }
    cache
}

/// Resolve `execution_mode` and `conductor.status` for a deliverable.
///
/// First checks the CLI JSON output; if the fields are missing (the sdlc CLI
/// may not serialize them), falls back to the pre-loaded frontmatter cache.
fn resolve_conductor_fields(
    item: &serde_json::Value,
    id: &str,
    frontmatter_cache: &HashMap<String, (String, String)>,
) -> (String, String) {
    let exec_mode = item["execution_mode"].as_str().unwrap_or("").to_string();
    let conductor_status = item["conductor"]["status"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if !exec_mode.is_empty() && !conductor_status.is_empty() {
        return (exec_mode, conductor_status);
    }

    // Fall back to pre-loaded frontmatter cache.
    if let Some((cached_mode, cached_status)) = frontmatter_cache.get(id) {
        let mode = if exec_mode.is_empty() {
            cached_mode.clone()
        } else {
            exec_mode
        };
        let status = if conductor_status.is_empty() {
            cached_status.clone()
        } else {
            conductor_status
        };
        return (mode, status);
    }

    (exec_mode, conductor_status)
}

/// Extract `execution_mode` and `conductor.status` from YAML frontmatter.
///
/// Expects `---` delimited frontmatter at the start of the file.
fn parse_conductor_frontmatter(contents: &str) -> Option<(String, String)> {
    let trimmed = contents.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_open = &trimmed[3..];
    let end = after_open.find("\n---")?;
    let frontmatter = &after_open[..end];

    let yaml: serde_json::Value = serde_yml::from_str(frontmatter).ok()?;
    let exec_mode = yaml["execution_mode"].as_str().unwrap_or("").to_string();
    let conductor_status = yaml["conductor"]["status"]
        .as_str()
        .unwrap_or("")
        .to_string();
    Some((exec_mode, conductor_status))
}

/// Run the `sdlc` CLI with the given arguments, optionally setting --sdlc-root.
fn run_sdlc(sdlc_root: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("sdlc");
    if !sdlc_root.is_empty() {
        cmd.args(["--sdlc-root", sdlc_root]);
    }
    cmd.args(args);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ConductorError::TicketSync(
                "sdlc not found. Install the Vantage SDLC CLI and ensure it is on your PATH."
                    .to_string(),
            )
        } else {
            ConductorError::TicketSync(format!("failed to run sdlc: {e}"))
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let code = output
            .status
            .code()
            .map_or("signal".to_string(), |c| c.to_string());
        let detail = match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
            (false, _) => stderr.trim().to_string(),
            (true, false) => stdout.trim().to_string(),
            (true, true) => format!("sdlc exited with status {code} and no output"),
        };
        return Err(ConductorError::TicketSync(format!(
            "sdlc exited with status {code}: {detail}"
        )));
    }

    Ok(output)
}

/// Update Vantage conductor status to "dispatched" when a workflow starts.
fn notify_dispatched(deliverable_id: &str, sdlc_root: &str, workflow_run_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    run_sdlc(
        sdlc_root,
        &[
            "deliverable",
            "set",
            "--",
            deliverable_id,
            "conductor.status=dispatched",
            &format!("conductor.dispatched_at={now}"),
            &format!("conductor.workflow_run_id={workflow_run_id}"),
        ],
    )?;
    tracing::info!("Vantage: marked {deliverable_id} as dispatched (run={workflow_run_id})");
    Ok(())
}

/// Update Vantage conductor status to "completed" when a workflow succeeds.
fn notify_completed(
    deliverable_id: &str,
    sdlc_root: &str,
    pr_url: Option<&str>,
    worktree_slug: Option<&str>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut args = vec![
        "deliverable",
        "set",
        "--",
        deliverable_id,
        "conductor.status=completed",
    ];
    let completed_at = format!("conductor.completed_at={now}");
    args.push(&completed_at);
    let pr_arg;
    if let Some(url) = pr_url {
        pr_arg = format!("conductor.pr_url={url}");
        args.push(&pr_arg);
    }
    let wt_arg;
    if let Some(slug) = worktree_slug {
        wt_arg = format!("conductor.worktree_slug={slug}");
        args.push(&wt_arg);
    }
    run_sdlc(sdlc_root, &args)?;
    tracing::info!("Vantage: marked {deliverable_id} as completed");
    Ok(())
}

/// Update Vantage conductor status to "failed" when a workflow fails.
fn notify_failed(deliverable_id: &str, sdlc_root: &str, reason: &str) -> Result<()> {
    let reason_arg = format!("conductor.failed_reason={reason}");
    run_sdlc(
        sdlc_root,
        &[
            "deliverable",
            "set",
            "--",
            deliverable_id,
            "conductor.status=failed",
            &reason_arg,
        ],
    )?;
    tracing::info!("Vantage: marked {deliverable_id} as failed");
    Ok(())
}

/// Extract the list of dependency deliverable IDs from a ticket's stored `raw_json`.
///
/// Returns an empty vec if the ticket is not a Vantage deliverable, if `raw_json` is
/// malformed, or if the `dependencies` field is absent.
pub fn get_parent_deliverable_ids(raw_json: &str) -> Vec<String> {
    let raw: serde_json::Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    raw.get("dependencies")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Lifecycle hooks for a workflow run backed by a Vantage deliverable.
///
/// Resolved once at workflow start via [`VantageLifecycle::resolve`]; methods write
/// back pipeline status to the Vantage SDLC backend at dispatch, completion, and
/// failure. All notification methods are best-effort: callers should log and continue
/// on error rather than aborting the workflow.
pub struct VantageLifecycle {
    pub deliverable_id: String,
    pub sdlc_root: String,
}

impl VantageLifecycle {
    /// Resolve the lifecycle context for a workflow run.
    ///
    /// Returns `Some(VantageLifecycle)` if the ticket is a Vantage deliverable and the
    /// repo has a configured Vantage issue source. Returns `None` (with a debug log) for
    /// any non-fatal resolution failure (wrong source type, missing config, DB error).
    pub fn resolve(conn: &Connection, ticket_id: &str, repo_id: &str) -> Option<Self> {
        let ticket = match crate::tickets::TicketSyncer::new(conn).get_by_id(ticket_id) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!("Vantage context: failed to look up ticket {ticket_id}: {e}");
                return None;
            }
        };
        if ticket.source_type != "vantage" {
            tracing::debug!(
                "Vantage context: ticket {ticket_id} is source_type={}, not vantage",
                ticket.source_type
            );
            return None;
        }
        let sources = match IssueSourceManager::new(conn).list(repo_id) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Vantage context: failed to list issue sources: {e}");
                return None;
            }
        };
        let vantage_source = match sources.iter().find(|s| s.source_type == "vantage") {
            Some(s) => s,
            None => {
                tracing::debug!("Vantage context: no vantage issue source for repo {repo_id}");
                return None;
            }
        };
        let cfg: VantageConfig = match serde_json::from_str(&vantage_source.config_json) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Vantage context: failed to parse config: {e}");
                return None;
            }
        };
        tracing::info!(
            "Vantage context resolved: deliverable={}, sdlc_root={}",
            ticket.source_id,
            cfg.sdlc_root
        );
        Some(VantageLifecycle {
            deliverable_id: ticket.source_id,
            sdlc_root: cfg.sdlc_root,
        })
    }

    /// Notify Vantage that the workflow has been dispatched (best-effort).
    pub fn on_dispatched(&self, workflow_run_id: &str) -> Result<()> {
        notify_dispatched(&self.deliverable_id, &self.sdlc_root, workflow_run_id)
    }

    /// Notify Vantage that the workflow completed successfully (best-effort).
    pub fn on_completed(&self, pr_url: Option<&str>, worktree_slug: Option<&str>) -> Result<()> {
        notify_completed(&self.deliverable_id, &self.sdlc_root, pr_url, worktree_slug)
    }

    /// Notify Vantage that the workflow failed (best-effort).
    pub fn on_failed(&self, reason: &str) -> Result<()> {
        notify_failed(&self.deliverable_id, &self.sdlc_root, reason)
    }
}

/// Parse a Vantage deliverable JSON object into a TicketInput.
fn parse_vantage_deliverable(d: &serde_json::Value) -> TicketInput {
    let id = d["id"].as_str().unwrap_or("").to_string();
    let title = d["title"].as_str().unwrap_or("").to_string();
    let body = d["body"].as_str().unwrap_or("").to_string();
    let status = d["status"].as_str().unwrap_or("draft");
    let state = map_vantage_status(status).to_string();
    let assignee = d["assigned_to"].as_str().map(|s| s.to_string());
    let priority = d["priority"].as_str().map(|s| s.to_string());

    let mut labels = Vec::new();
    if let Some(codebase) = d["codebase"].as_str() {
        if !codebase.is_empty() {
            labels.push(codebase.to_string());
        }
    }
    if let Some(dtype) = d["type"].as_str() {
        if !dtype.is_empty() {
            labels.push(dtype.to_string());
        }
    }

    let url = format!("vantage://deliverables/{id}");

    let blocked_by = d["dependencies"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let children = d["deliverables"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    TicketInput {
        source_type: "vantage".to_string(),
        source_id: id,
        title,
        body,
        state,
        labels,
        assignee,
        priority,
        url,
        raw_json: serde_json::to_string(d).ok(),
        label_details: vec![],
        blocked_by,
        children,
        parent: None,
    }
}

/// Map a Vantage deliverable status to a Conductor ticket state.
fn map_vantage_status(status: &str) -> &str {
    match status {
        "draft" | "planned" | "blocked" => "open",
        "in_progress" => "in_progress",
        "complete" | "closed" => "closed",
        _ => "open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::create_test_conn;

    // ── fake-sdlc helpers ─────────────────────────────────────────────────────
    //
    // Tests that exercise code paths depending on the `sdlc` binary use a fake
    // shell script written to a tempdir.  A process-wide mutex serialises all
    // PATH-mutating tests so concurrent test threads cannot interfere with each
    // other's PATH value.
    //
    // `set_var` is `unsafe` in Rust ≥ 1.81 because the underlying C function is
    // not re-entrant.  Holding the mutex while calling it is sufficient to make
    // it sound within our single-process test binary.

    #[cfg(unix)]
    mod sdlc_integration {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        static PATH_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

        /// Write a shell script named `sdlc` into `dir` and make it executable.
        fn write_fake_sdlc(dir: &std::path::Path, script: &str) {
            let path = dir.join("sdlc");
            fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        /// Run `f` with PATH prepended by `dir`.  Restores the original PATH
        /// afterwards even if `f` panics.
        fn with_sdlc_on_path<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
            let _guard = PATH_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let orig = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("{}:{}", dir.display(), orig);
            unsafe { std::env::set_var("PATH", &new_path) };
            // Use a catch_unwind-style guard via a local struct.
            struct Restore(String);
            impl Drop for Restore {
                fn drop(&mut self) {
                    unsafe { std::env::set_var("PATH", &self.0) };
                }
            }
            let _restore = Restore(orig);
            f()
        }

        /// Run `f` with PATH set to an empty tempdir so `sdlc` cannot be found.
        fn with_no_sdlc<T>(f: impl FnOnce() -> T) -> T {
            let dir = tempfile::tempdir().unwrap();
            let _guard = PATH_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            let orig = std::env::var("PATH").unwrap_or_default();
            unsafe { std::env::set_var("PATH", dir.path()) };
            struct Restore(String);
            impl Drop for Restore {
                fn drop(&mut self) {
                    unsafe { std::env::set_var("PATH", &self.0) };
                }
            }
            let _restore = Restore(orig);
            f()
        }

        // ── run_sdlc error paths ──────────────────────────────────────────────

        #[test]
        fn test_sync_sdlc_not_found_returns_helpful_error() {
            with_no_sdlc(|| {
                let err = sync_vantage_deliverables("PROJ-1", "", "my-repo")
                    .err()
                    .unwrap();
                let msg = err.to_string();
                assert!(
                    msg.contains("sdlc not found") || msg.contains("Install"),
                    "expected helpful sdlc-not-found message, got: {msg}"
                );
            });
        }

        #[test]
        fn test_fetch_sdlc_not_found_returns_helpful_error() {
            with_no_sdlc(|| {
                let err = fetch_vantage_deliverable("D-001", "").err().unwrap();
                let msg = err.to_string();
                assert!(
                    msg.contains("sdlc not found") || msg.contains("Install"),
                    "expected helpful sdlc-not-found message, got: {msg}"
                );
            });
        }

        #[test]
        fn test_sync_sdlc_exits_nonzero_returns_error() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "echo 'something went wrong' >&2; exit 1");
            with_sdlc_on_path(dir.path(), || {
                let err = sync_vantage_deliverables("PROJ-1", "", "my-repo")
                    .err()
                    .unwrap();
                let msg = err.to_string();
                assert!(
                    msg.contains("sdlc exited"),
                    "expected sdlc exit error, got: {msg}"
                );
            });
        }

        #[test]
        fn test_sync_sdlc_returns_invalid_json_returns_parse_error() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "echo 'not valid json {{'");
            with_sdlc_on_path(dir.path(), || {
                let err = sync_vantage_deliverables("PROJ-1", "", "my-repo")
                    .err()
                    .unwrap();
                let msg = err.to_string();
                assert!(
                    msg.contains("failed to parse"),
                    "expected parse error, got: {msg}"
                );
            });
        }

        #[test]
        fn test_fetch_sdlc_exits_nonzero_returns_error() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "echo 'not found' >&2; exit 2");
            with_sdlc_on_path(dir.path(), || {
                let err = fetch_vantage_deliverable("D-001", "").err().unwrap();
                assert!(err.to_string().contains("sdlc exited"));
            });
        }

        #[test]
        fn test_fetch_null_response_returns_not_found() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "echo 'null'");
            with_sdlc_on_path(dir.path(), || {
                let err = fetch_vantage_deliverable("D-999", "").err().unwrap();
                assert!(
                    matches!(err, crate::error::ConductorError::TicketNotFound { .. }),
                    "expected TicketNotFound, got: {err}"
                );
            });
        }

        #[test]
        fn test_fetch_invalid_json_returns_parse_error() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "echo 'oops'");
            with_sdlc_on_path(dir.path(), || {
                let err = fetch_vantage_deliverable("D-001", "").err().unwrap();
                assert!(err.to_string().contains("failed to parse"));
            });
        }

        // ── sync_vantage_deliverables filtering ───────────────────────────────

        #[test]
        fn test_sync_filters_out_wrong_codebase() {
            let list = serde_json::json!([
                {
                    "id": "D-001", "title": "A", "status": "draft",
                    "codebase": "other-repo",
                    "execution_mode": "conductor",
                    "conductor": { "status": "ready" },
                    "body": "body text",
                },
            ]);
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), &format!("echo '{list}'"));
            with_sdlc_on_path(dir.path(), || {
                let tickets = sync_vantage_deliverables("PROJ-1", "", "my-repo").unwrap();
                assert!(
                    tickets.is_empty(),
                    "deliverable with wrong codebase should be filtered out"
                );
            });
        }

        #[test]
        fn test_sync_filters_out_non_conductor_execution_mode() {
            let list = serde_json::json!([
                {
                    "id": "D-002", "title": "B", "status": "draft",
                    "codebase": "my-repo",
                    "execution_mode": "manual",
                    "conductor": { "status": "ready" },
                    "body": "body text",
                },
            ]);
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), &format!("echo '{list}'"));
            with_sdlc_on_path(dir.path(), || {
                let tickets = sync_vantage_deliverables("PROJ-1", "", "my-repo").unwrap();
                assert!(
                    tickets.is_empty(),
                    "non-conductor mode should be filtered out"
                );
            });
        }

        #[test]
        fn test_sync_filters_out_pre_ready_conductor_status() {
            let list = serde_json::json!([
                {
                    "id": "D-003", "title": "C", "status": "draft",
                    "codebase": "my-repo",
                    "execution_mode": "conductor",
                    "conductor": { "status": "pending_audit" },
                    "body": "body text",
                },
            ]);
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), &format!("echo '{list}'"));
            with_sdlc_on_path(dir.path(), || {
                let tickets = sync_vantage_deliverables("PROJ-1", "", "my-repo").unwrap();
                assert!(
                    tickets.is_empty(),
                    "pre-ready conductor status should be filtered out"
                );
            });
        }

        #[test]
        fn test_sync_returns_matching_deliverable() {
            let list = serde_json::json!([
                {
                    "id": "D-004", "title": "Matched", "status": "in_progress",
                    "codebase": "my-repo",
                    "execution_mode": "conductor",
                    "conductor": { "status": "ready" },
                    "body": "Implementation details here",
                    "type": "feature",
                },
            ]);
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), &format!("echo '{list}'"));
            with_sdlc_on_path(dir.path(), || {
                let tickets = sync_vantage_deliverables("PROJ-1", "", "my-repo").unwrap();
                assert_eq!(tickets.len(), 1);
                assert_eq!(tickets[0].source_id, "D-004");
                assert_eq!(tickets[0].title, "Matched");
                assert_eq!(tickets[0].state, "in_progress");
                assert!(tickets[0].labels.contains(&"my-repo".to_string()));
                assert!(tickets[0].labels.contains(&"feature".to_string()));
            });
        }

        #[test]
        fn test_sync_skips_items_missing_id() {
            let list = serde_json::json!([
                { "title": "No ID", "codebase": "my-repo", "execution_mode": "conductor", "conductor": { "status": "ready" }, "body": "x" },
                { "id": "D-005", "title": "Has ID", "status": "draft", "codebase": "my-repo", "execution_mode": "conductor", "conductor": { "status": "ready" }, "body": "y" },
            ]);
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), &format!("echo '{list}'"));
            with_sdlc_on_path(dir.path(), || {
                let tickets = sync_vantage_deliverables("PROJ-1", "", "my-repo").unwrap();
                assert_eq!(tickets.len(), 1);
                assert_eq!(tickets[0].source_id, "D-005");
            });
        }

        // ── fetch_vantage_deliverable happy path ──────────────────────────────

        #[test]
        fn test_fetch_returns_parsed_ticket() {
            let detail = serde_json::json!({
                "id": "D-010",
                "title": "Fetched deliverable",
                "status": "in_progress",
                "codebase": "my-repo",
                "body": "Full description here",
            });
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), &format!("echo '{detail}'"));
            with_sdlc_on_path(dir.path(), || {
                let ticket = fetch_vantage_deliverable("D-010", "").unwrap();
                assert_eq!(ticket.source_id, "D-010");
                assert_eq!(ticket.title, "Fetched deliverable");
                assert_eq!(ticket.body, "Full description here");
            });
        }

        // ── VantageLifecycle notification methods ─────────────────────────────

        fn make_lifecycle() -> VantageLifecycle {
            VantageLifecycle {
                deliverable_id: "D-001".to_string(),
                sdlc_root: String::new(),
            }
        }

        #[test]
        fn test_on_dispatched_succeeds_when_sdlc_exits_zero() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "exit 0");
            with_sdlc_on_path(dir.path(), || {
                let lc = make_lifecycle();
                assert!(
                    lc.on_dispatched("run-001").is_ok(),
                    "on_dispatched should succeed when sdlc exits 0"
                );
            });
        }

        #[test]
        fn test_on_dispatched_fails_when_sdlc_exits_nonzero() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "echo 'permission denied' >&2; exit 1");
            with_sdlc_on_path(dir.path(), || {
                let lc = make_lifecycle();
                assert!(
                    lc.on_dispatched("run-001").is_err(),
                    "on_dispatched should propagate sdlc failure"
                );
            });
        }

        #[test]
        fn test_on_completed_succeeds_with_optional_args() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "exit 0");
            with_sdlc_on_path(dir.path(), || {
                let lc = make_lifecycle();
                assert!(lc
                    .on_completed(Some("https://github.com/org/repo/pull/42"), Some("wt-slug"))
                    .is_ok());
                assert!(lc.on_completed(None, None).is_ok());
            });
        }

        #[test]
        fn test_on_failed_succeeds_when_sdlc_exits_zero() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "exit 0");
            with_sdlc_on_path(dir.path(), || {
                let lc = make_lifecycle();
                assert!(lc.on_failed("workflow step timed out").is_ok());
            });
        }

        #[test]
        fn test_on_failed_propagates_sdlc_error() {
            let dir = tempfile::tempdir().unwrap();
            write_fake_sdlc(dir.path(), "exit 3");
            with_sdlc_on_path(dir.path(), || {
                let lc = make_lifecycle();
                assert!(lc.on_failed("reason").is_err());
            });
        }
    }

    #[test]
    fn test_parse_vantage_deliverable_basic() {
        let json = serde_json::json!({
            "id": "D-042",
            "title": "Implement OAuth flow",
            "status": "in_progress",
            "type": "implementation",
            "assigned_to": "alice@example.com",
            "priority": "p1",
            "body": "Full OAuth2 PKCE implementation",
        });

        let ticket = parse_vantage_deliverable(&json);
        assert_eq!(ticket.source_type, "vantage");
        assert_eq!(ticket.source_id, "D-042");
        assert_eq!(ticket.title, "Implement OAuth flow");
        assert_eq!(ticket.state, "in_progress");
        assert_eq!(ticket.labels, vec!["implementation"]);
        assert_eq!(ticket.assignee, Some("alice@example.com".to_string()));
        assert_eq!(ticket.priority, Some("p1".to_string()));
        assert_eq!(ticket.body, "Full OAuth2 PKCE implementation");
        assert_eq!(ticket.url, "vantage://deliverables/D-042");
        assert!(ticket.label_details.is_empty());
        assert!(ticket.blocked_by.is_empty());
        assert!(ticket.children.is_empty());
    }

    #[test]
    fn test_parse_vantage_deliverable_missing_fields() {
        let json = serde_json::json!({
            "id": "D-001",
            "title": "Minimal",
            "status": "draft",
        });

        let ticket = parse_vantage_deliverable(&json);
        assert_eq!(ticket.source_id, "D-001");
        assert_eq!(ticket.title, "Minimal");
        assert_eq!(ticket.state, "open");
        assert_eq!(ticket.body, "");
        assert_eq!(ticket.assignee, None);
        assert_eq!(ticket.priority, None);
        assert!(ticket.labels.is_empty());
        assert!(ticket.blocked_by.is_empty());
        assert!(ticket.children.is_empty());
    }

    #[test]
    fn test_parse_vantage_deliverable_empty_type() {
        let json = serde_json::json!({
            "id": "D-002",
            "title": "Test",
            "status": "planned",
            "type": "",
        });

        let ticket = parse_vantage_deliverable(&json);
        assert!(ticket.labels.is_empty());
    }

    #[test]
    fn test_map_vantage_status_open_variants() {
        assert_eq!(map_vantage_status("draft"), "open");
        assert_eq!(map_vantage_status("planned"), "open");
        assert_eq!(map_vantage_status("blocked"), "open");
    }

    #[test]
    fn test_map_vantage_status_in_progress() {
        assert_eq!(map_vantage_status("in_progress"), "in_progress");
    }

    #[test]
    fn test_map_vantage_status_closed_variants() {
        assert_eq!(map_vantage_status("complete"), "closed");
        assert_eq!(map_vantage_status("closed"), "closed");
    }

    #[test]
    fn test_map_vantage_status_unknown_defaults_to_open() {
        assert_eq!(map_vantage_status("something_else"), "open");
        assert_eq!(map_vantage_status(""), "open");
    }

    #[test]
    fn test_parse_conductor_frontmatter_full() {
        let md =
            "---\nid: D-136\nexecution_mode: conductor\nconductor:\n  status: ready\n---\n# Body";
        let (em, cs) = parse_conductor_frontmatter(md).unwrap();
        assert_eq!(em, "conductor");
        assert_eq!(cs, "ready");
    }

    #[test]
    fn test_parse_conductor_frontmatter_missing_fields() {
        let md = "---\nid: D-001\nstatus: draft\n---\n# Body";
        let (em, cs) = parse_conductor_frontmatter(md).unwrap();
        assert_eq!(em, "");
        assert_eq!(cs, "");
    }

    #[test]
    fn test_parse_conductor_frontmatter_no_frontmatter() {
        let md = "# Just a heading\nNo frontmatter here.";
        assert!(parse_conductor_frontmatter(md).is_none());
    }

    #[test]
    fn test_resolve_conductor_fields_from_json() {
        let item = serde_json::json!({
            "execution_mode": "conductor",
            "conductor": { "status": "ready" }
        });
        let cache = std::collections::HashMap::new();
        let (em, cs) = resolve_conductor_fields(&item, "D-001", &cache);
        assert_eq!(em, "conductor");
        assert_eq!(cs, "ready");
    }

    #[test]
    fn test_parse_vantage_raw_json_preserved() {
        let json = serde_json::json!({
            "id": "D-099",
            "title": "Raw test",
            "status": "complete",
            "custom_field": "preserved",
        });

        let ticket = parse_vantage_deliverable(&json);
        let raw: serde_json::Value =
            serde_json::from_str(ticket.raw_json.as_deref().unwrap()).unwrap();
        assert_eq!(raw["custom_field"], "preserved");
    }

    // --- blocked_by / children extraction ---

    #[test]
    fn test_parse_vantage_deliverable_with_dependencies() {
        let json = serde_json::json!({
            "id": "D-010",
            "title": "Blocked ticket",
            "status": "draft",
            "dependencies": ["D-001", "D-002"],
        });

        let ticket = parse_vantage_deliverable(&json);
        assert_eq!(ticket.blocked_by, vec!["D-001", "D-002"]);
        assert!(ticket.children.is_empty());
    }

    #[test]
    fn test_parse_vantage_deliverable_with_children() {
        let json = serde_json::json!({
            "id": "S-001",
            "title": "Story with children",
            "status": "in_progress",
            "deliverables": ["D-010", "D-011", "D-012"],
        });

        let ticket = parse_vantage_deliverable(&json);
        assert!(ticket.blocked_by.is_empty());
        assert_eq!(ticket.children, vec!["D-010", "D-011", "D-012"]);
    }

    #[test]
    fn test_parse_vantage_deliverable_empty_dependencies() {
        let json = serde_json::json!({
            "id": "D-020",
            "title": "No deps",
            "status": "draft",
            "dependencies": [],
        });

        let ticket = parse_vantage_deliverable(&json);
        assert!(ticket.blocked_by.is_empty());
        assert!(ticket.children.is_empty());
    }

    #[test]
    fn test_parse_vantage_deliverable_no_dependency_fields() {
        let json = serde_json::json!({
            "id": "D-030",
            "title": "No dep fields at all",
            "status": "draft",
        });

        let ticket = parse_vantage_deliverable(&json);
        assert!(ticket.blocked_by.is_empty());
        assert!(ticket.children.is_empty());
    }

    // --- get_parent_deliverable_ids ---

    #[test]
    fn test_get_parent_deliverable_ids_with_deps() {
        let json = serde_json::json!({ "id": "D-001", "dependencies": ["D-002", "D-003"] });
        let ids = get_parent_deliverable_ids(&serde_json::to_string(&json).unwrap());
        assert_eq!(ids, vec!["D-002", "D-003"]);
    }

    #[test]
    fn test_get_parent_deliverable_ids_empty_array() {
        let json = serde_json::json!({ "id": "D-001", "dependencies": [] });
        let ids = get_parent_deliverable_ids(&serde_json::to_string(&json).unwrap());
        assert!(ids.is_empty());
    }

    #[test]
    fn test_get_parent_deliverable_ids_missing_field() {
        let json = serde_json::json!({ "id": "D-001" });
        let ids = get_parent_deliverable_ids(&serde_json::to_string(&json).unwrap());
        assert!(ids.is_empty());
    }

    #[test]
    fn test_get_parent_deliverable_ids_invalid_json() {
        let ids = get_parent_deliverable_ids("not valid json {{");
        assert!(ids.is_empty());
    }

    #[test]
    fn test_get_parent_deliverable_ids_filters_non_string_values() {
        let json = serde_json::json!({
            "id": "D-001",
            "dependencies": [42, "D-002", null, "D-003"]
        });
        let ids = get_parent_deliverable_ids(&serde_json::to_string(&json).unwrap());
        assert_eq!(ids, vec!["D-002", "D-003"]);
    }

    // --- VantageLifecycle::resolve ---

    #[test]
    fn test_resolve_returns_none_when_ticket_not_found() {
        let conn = create_test_conn();
        let result = VantageLifecycle::resolve(&conn, "nonexistent-ticket", "repo-1");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_returns_none_for_non_vantage_ticket() {
        let conn = create_test_conn();
        // Insert a github ticket
        crate::test_helpers::insert_test_repo(&conn, "r1", "my-repo", "/tmp/repo");
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', 'GH-42', 'Test', '', 'open', '[]', 'https://github.com', '2024-01-01', '{}')",
            [],
        ).unwrap();
        let result = VantageLifecycle::resolve(&conn, "t1", "r1");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_returns_none_when_no_vantage_issue_source() {
        let conn = create_test_conn();
        crate::test_helpers::insert_test_repo(&conn, "r1", "my-repo", "/tmp/repo");
        // Insert a vantage ticket but no issue source
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'vantage', 'D-001', 'Test', '', 'open', '[]', 'vantage://deliverables/D-001', '2024-01-01', '{}')",
            [],
        ).unwrap();
        let result = VantageLifecycle::resolve(&conn, "t1", "r1");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_returns_none_for_malformed_config() {
        let conn = create_test_conn();
        crate::test_helpers::insert_test_repo(&conn, "r1", "my-repo", "/tmp/repo");
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'vantage', 'D-001', 'Test', '', 'open', '[]', 'vantage://deliverables/D-001', '2024-01-01', '{}')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO repo_issue_sources (id, repo_id, source_type, config_json) \
             VALUES ('s1', 'r1', 'vantage', 'not-valid-json')",
            [],
        )
        .unwrap();
        let result = VantageLifecycle::resolve(&conn, "t1", "r1");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_returns_lifecycle_on_success() {
        let conn = create_test_conn();
        crate::test_helpers::insert_test_repo(&conn, "r1", "my-repo", "/tmp/repo");
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'vantage', 'D-042', 'Test', '', 'open', '[]', 'vantage://deliverables/D-042', '2024-01-01', '{}')",
            [],
        ).unwrap();
        let config = serde_json::json!({"project_id": "PROJ-001", "sdlc_root": "/path/to/sdlc"});
        conn.execute(
            "INSERT INTO repo_issue_sources (id, repo_id, source_type, config_json) \
             VALUES ('s1', 'r1', 'vantage', ?1)",
            rusqlite::params![config.to_string()],
        )
        .unwrap();
        let result = VantageLifecycle::resolve(&conn, "t1", "r1").unwrap();
        assert_eq!(result.deliverable_id, "D-042");
        assert_eq!(result.sdlc_root, "/path/to/sdlc");
    }

    // ── resolve_conductor_fields + frontmatter fallback ──────────────────

    #[test]
    fn resolve_conductor_fields_json_present_returns_json_values() {
        let item = serde_json::json!({
            "id": "D-100",
            "execution_mode": "conductor",
            "conductor": { "status": "ready" },
        });
        let cache = std::collections::HashMap::new();
        let (mode, status) = resolve_conductor_fields(&item, "D-100", &cache);
        assert_eq!(mode, "conductor");
        assert_eq!(status, "ready");
    }

    #[test]
    fn resolve_conductor_fields_falls_back_to_frontmatter_cache() {
        let item = serde_json::json!({
            "id": "D-200",
        });
        let mut cache = std::collections::HashMap::new();
        cache.insert(
            "D-200".to_string(),
            ("conductor".to_string(), "dispatched".to_string()),
        );
        let (mode, status) = resolve_conductor_fields(&item, "D-200", &cache);
        assert_eq!(mode, "conductor");
        assert_eq!(status, "dispatched");
    }

    #[test]
    fn preload_deliverable_frontmatter_reads_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let deliverables_dir = dir.path().join("deliverables");
        std::fs::create_dir_all(&deliverables_dir).unwrap();
        std::fs::write(
            deliverables_dir.join("D-300.md"),
            "---\nexecution_mode: conductor\nconductor:\n  status: ready\n---\n# Hello\n",
        )
        .unwrap();
        let cache = preload_deliverable_frontmatter(dir.path().to_str().unwrap());
        assert_eq!(cache.len(), 1);
        let (mode, status) = cache.get("D-300").unwrap();
        assert_eq!(mode, "conductor");
        assert_eq!(status, "ready");
    }

    #[test]
    fn preload_deliverable_frontmatter_empty_sdlc_root() {
        let cache = preload_deliverable_frontmatter("");
        assert!(cache.is_empty());
    }
}
