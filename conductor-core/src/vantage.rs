use std::process::Command;

use crate::error::{ConductorError, Result};
use crate::tickets::TicketInput;

/// Conductor pipeline statuses that should be synced into Conductor.
/// Pre-ready states (pending_audit, audited, enriching) are excluded —
/// those deliverables aren't actionable yet.
const ACTIONABLE_CONDUCTOR_STATUSES: &[&str] =
    &["ready", "dispatched", "running", "completed", "failed"];

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

    let mut tickets = Vec::with_capacity(items.len());
    let mut skipped = 0usize;
    for item in &items {
        let id = item["id"].as_str().unwrap_or("");
        if id.is_empty() {
            continue;
        }
        // Only sync deliverables whose codebase matches this repo
        let codebase = item["codebase"].as_str().unwrap_or("");
        if codebase != repo_slug {
            skipped += 1;
            tracing::debug!("Vantage sync: skipping {id} (codebase={codebase:?} != {repo_slug:?})");
            continue;
        }
        // Only sync conductor-mode deliverables in actionable pipeline states
        let exec_mode = item["execution_mode"].as_str().unwrap_or("");
        let conductor_status = item["conductor"]["status"].as_str().unwrap_or("");
        if exec_mode != "conductor" || !ACTIONABLE_CONDUCTOR_STATUSES.contains(&conductor_status) {
            skipped += 1;
            tracing::debug!(
                "Vantage sync: skipping {id} (execution_mode={exec_mode:?}, conductor.status={conductor_status:?})"
            );
            continue;
        }
        let status = item["status"].as_str().unwrap_or("");
        tracing::debug!("Vantage sync: matched {id} (codebase={codebase:?}, status={status:?}, conductor.status={conductor_status:?})");
        // Fetch full detail for each deliverable (list output lacks body)
        match fetch_vantage_deliverable(id, sdlc_root) {
            Ok(ticket) => tickets.push(ticket),
            Err(e) => {
                tracing::warn!("Failed to fetch Vantage deliverable {id}: {e}");
            }
        }
    }

    tracing::info!(
        "Vantage sync: matched {} deliverables, skipped {skipped} (filtered out)",
        tickets.len(),
    );

    Ok(tickets)
}

/// Fetch a single Vantage deliverable by ID.
pub fn fetch_vantage_deliverable(deliverable_id: &str, sdlc_root: &str) -> Result<TicketInput> {
    let output = run_sdlc(sdlc_root, &["deliverable", "get", deliverable_id, "--json"])?;

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
        return Err(ConductorError::TicketSync(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(output)
}

/// Update Vantage conductor status to "in_progress" when work begins.
pub fn notify_in_progress(
    deliverable_id: &str,
    sdlc_root: &str,
    workflow_run_id: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    run_sdlc(
        sdlc_root,
        &[
            "deliverable",
            "set",
            deliverable_id,
            "conductor.status=in_progress",
            &format!("conductor.in_progress_at={now}"),
            &format!("conductor.workflow_run_id={workflow_run_id}"),
        ],
    )?;
    tracing::info!("Vantage: marked {deliverable_id} as in_progress (run={workflow_run_id})");
    Ok(())
}

/// Update Vantage conductor status to "pr_approved" when a review step
/// completes with the `pr_review_approved` marker.
pub fn notify_pr_approved(
    deliverable_id: &str,
    sdlc_root: &str,
    pr_url: Option<&str>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut args = vec![
        "deliverable",
        "set",
        deliverable_id,
        "conductor.status=pr_approved",
    ];
    let ts_arg = format!("conductor.pr_approved_at={now}");
    args.push(&ts_arg);
    let pr_arg;
    if let Some(url) = pr_url {
        pr_arg = format!("conductor.pr_url={url}");
        args.push(&pr_arg);
    }
    run_sdlc(sdlc_root, &args)?;
    tracing::info!("Vantage: marked {deliverable_id} as pr_approved");
    Ok(())
}

/// Update Vantage conductor status to "merged" when the PR is merged.
pub fn notify_merged(deliverable_id: &str, sdlc_root: &str, pr_url: Option<&str>) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut args = vec![
        "deliverable",
        "set",
        deliverable_id,
        "conductor.status=merged",
    ];
    let ts_arg = format!("conductor.merged_at={now}");
    args.push(&ts_arg);
    let pr_arg;
    if let Some(url) = pr_url {
        pr_arg = format!("conductor.pr_url={url}");
        args.push(&pr_arg);
    }
    run_sdlc(sdlc_root, &args)?;
    tracing::info!("Vantage: marked {deliverable_id} as merged");
    Ok(())
}

/// Update Vantage conductor status to "failed" when a workflow fails.
pub fn notify_failed(deliverable_id: &str, sdlc_root: &str, reason: &str) -> Result<()> {
    let escaped_reason = reason.replace('"', "'");
    let reason_arg = format!("conductor.failed_reason={escaped_reason}");
    run_sdlc(
        sdlc_root,
        &[
            "deliverable",
            "set",
            deliverable_id,
            "conductor.status=failed",
            &reason_arg,
        ],
    )?;
    tracing::info!("Vantage: marked {deliverable_id} as failed");
    Ok(())
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
        raw_json: serde_json::to_string(d).unwrap_or_else(|_| "{}".to_string()),
        label_details: vec![],
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
    fn test_parse_vantage_raw_json_preserved() {
        let json = serde_json::json!({
            "id": "D-099",
            "title": "Raw test",
            "status": "complete",
            "custom_field": "preserved",
        });

        let ticket = parse_vantage_deliverable(&json);
        let raw: serde_json::Value = serde_json::from_str(&ticket.raw_json).unwrap();
        assert_eq!(raw["custom_field"], "preserved");
    }
}
