use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use conductor_core::workflow::{WorkflowManager, WorkflowRun, WorkflowRunStatus};

use crate::state::AppState;

/// Maximum allowed age of a Slack request timestamp (5 minutes), to prevent replay attacks.
const SLACK_TIMESTAMP_TOLERANCE_SECS: i64 = 300;

/// Slack sends slash command payloads as application/x-www-form-urlencoded.
#[derive(Debug, Deserialize)]
pub struct SlackSlashCommand {
    pub command: Option<String>,
    pub text: Option<String>,
    #[allow(dead_code)]
    pub response_url: Option<String>,
    #[allow(dead_code)]
    pub user_name: Option<String>,
}

/// Slack expects a JSON response with a `text` field (and optional `response_type`).
#[derive(Serialize)]
struct SlackResponse {
    response_type: &'static str,
    text: String,
}

/// Verify the `X-Slack-Signature` header using HMAC-SHA256.
///
/// Returns `Ok(())` if the signature is valid or no signing secret is configured
/// (to allow gradual rollout). Returns `Err` with a status code if verification fails.
///
/// Uses constant-time comparison to prevent timing side-channel attacks, and
/// validates the request timestamp to prevent replay attacks.
fn verify_slack_signature(
    signing_secret: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), StatusCode> {
    let secret = match signing_secret {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()), // no secret configured — skip verification
    };

    let timestamp = headers
        .get("X-Slack-Request-Timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Reject requests with timestamps older than 5 minutes to prevent replays.
    let ts: i64 = timestamp.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if (now - ts).abs() > SLACK_TIMESTAMP_TOLERANCE_SECS {
        tracing::warn!("Slack request timestamp is too old or in the future");
        return Err(StatusCode::UNAUTHORIZED);
    }

    let sig_header = headers
        .get("X-Slack-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // sig_header must be "v0=<hex-digest>"
    let hex_digest = sig_header
        .strip_prefix("v0=")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let expected_bytes = hex::decode(hex_digest).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let base_string = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    mac.update(base_string.as_bytes());

    // verify_slice uses constant-time comparison to prevent timing attacks.
    mac.verify_slice(&expected_bytes).map_err(|_| {
        tracing::warn!("Slack signature verification failed");
        StatusCode::UNAUTHORIZED
    })?;

    Ok(())
}

/// Handle Slack slash commands.
///
/// Supports:
///   /conductor active   — list active workflow runs
///   /conductor help     — show available commands
#[utoipa::path(
    post,
    path = "/api/slack/commands",
    request_body(content = String, description = "URL-encoded Slack slash command payload", content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, description = "Slack slash command response"),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Signature verification failed"),
    ),
    tag = "slack",
)]
pub async fn handle_slash_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify Slack request signature when a signing secret is configured.
    {
        let config = state.config.read().await;
        let secret = config.notifications.slack.signing_secret.as_deref();
        if let Err(status) = verify_slack_signature(secret, &headers, &body) {
            return (
                status,
                axum::Json(SlackResponse {
                    response_type: "ephemeral",
                    text: "Request signature verification failed.".to_string(),
                }),
            );
        }
    }

    let payload: SlackSlashCommand = match serde_urlencoded::from_bytes(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to parse Slack slash command payload: {e}");
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(SlackResponse {
                    response_type: "ephemeral",
                    text: "Invalid request payload.".to_string(),
                }),
            );
        }
    };

    let subcommand = payload.text.as_deref().unwrap_or("").trim();

    let response = match subcommand.split_whitespace().next().unwrap_or("help") {
        "active" => handle_active(&state).await,
        _ => SlackResponse {
            response_type: "ephemeral",
            text: concat!(
                "*Available commands:*\n",
                "• `/conductor active` — list active workflow runs\n",
                "• `/conductor help` — show this message",
            )
            .to_string(),
        },
    };

    (StatusCode::OK, axum::Json(response))
}

async fn handle_active(state: &AppState) -> SlackResponse {
    let db = state.db.lock().await;
    let wf_mgr = WorkflowManager::new(&db);

    let runs = match wf_mgr.list_active_workflow_runs(&[]) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list active workflow runs for Slack command");
            return SlackResponse {
                response_type: "ephemeral",
                text: format!("Error querying workflows: {e}"),
            };
        }
    };

    SlackResponse {
        response_type: "in_channel",
        text: format_active_runs_for_slack(&runs),
    }
}

/// Format active workflow runs as a Slack mrkdwn message.
fn format_active_runs_for_slack(runs: &[WorkflowRun]) -> String {
    if runs.is_empty() {
        return "No active workflow runs.".to_string();
    }
    let mut lines = vec![format!("*Active workflow runs ({}):*", runs.len())];
    for run in runs {
        let label = run.target_label.as_deref().unwrap_or("-");
        let since = &run.started_at[..16.min(run.started_at.len())];
        let status_emoji = match run.status {
            WorkflowRunStatus::Running => ":arrows_counterclockwise:",
            WorkflowRunStatus::Waiting => ":hourglass_flowing_sand:",
            WorkflowRunStatus::Pending => ":clock3:",
            _ => ":grey_question:",
        };
        lines.push(format!(
            "{status_emoji} *{}* on `{label}` — {} (since {since})",
            run.workflow_name, run.status,
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn make_headers(timestamp: &str, signature: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Slack-Request-Timestamp",
            HeaderValue::from_str(timestamp).unwrap(),
        );
        headers.insert(
            "X-Slack-Signature",
            HeaderValue::from_str(signature).unwrap(),
        );
        headers
    }

    fn compute_signature(secret: &str, timestamp: &str, body: &[u8]) -> String {
        use hmac::Mac as _;
        let base = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn fresh_timestamp() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    }

    #[test]
    fn no_secret_skips_verification() {
        let ts = fresh_timestamp();
        let headers = make_headers(&ts, "v0=badhash");
        assert_eq!(verify_slack_signature(None, &headers, b"body"), Ok(()));
        assert_eq!(verify_slack_signature(Some(""), &headers, b"body"), Ok(()));
    }

    #[test]
    fn valid_signature_accepted() {
        let secret = "test-secret";
        let body = b"command=/conductor&text=active";
        let ts = fresh_timestamp();
        let sig = compute_signature(secret, &ts, body);
        let headers = make_headers(&ts, &sig);
        assert_eq!(verify_slack_signature(Some(secret), &headers, body), Ok(()));
    }

    #[test]
    fn wrong_signature_rejected() {
        let secret = "test-secret";
        let ts = fresh_timestamp();
        let headers = make_headers(&ts, "v0=deadbeef");
        assert_eq!(
            verify_slack_signature(Some(secret), &headers, b"body"),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn missing_timestamp_header_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Slack-Signature", HeaderValue::from_static("v0=deadbeef"));
        assert_eq!(
            verify_slack_signature(Some("secret"), &headers, b"body"),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn missing_signature_header_rejected() {
        let ts = fresh_timestamp();
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Slack-Request-Timestamp",
            HeaderValue::from_str(&ts).unwrap(),
        );
        assert_eq!(
            verify_slack_signature(Some("secret"), &headers, b"body"),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn stale_timestamp_rejected() {
        let secret = "test-secret";
        let body = b"command=/conductor";
        // Use a timestamp 10 minutes in the past
        let stale_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600)
            .to_string();
        let sig = compute_signature(secret, &stale_ts, body);
        let headers = make_headers(&stale_ts, &sig);
        assert_eq!(
            verify_slack_signature(Some(secret), &headers, body),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn format_empty_runs() {
        assert_eq!(
            format_active_runs_for_slack(&[]),
            "No active workflow runs."
        );
    }

    #[test]
    fn format_active_runs_includes_fields() {
        use conductor_core::workflow::WorkflowRun;
        use std::collections::HashMap;

        let runs = vec![WorkflowRun {
            id: "run-1".into(),
            workflow_name: "deploy".into(),
            worktree_id: None,
            parent_run_id: String::new(),
            status: WorkflowRunStatus::Running,
            dry_run: false,
            trigger: "manual".into(),
            started_at: "2025-01-15T10:30:00Z".into(),
            ended_at: None,
            result_summary: None,
            error: None,
            definition_snapshot: None,
            inputs: HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: Some("my-repo/main".into()),
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            feature_id: None,
            workflow_title: None,
            total_input_tokens: None,
            total_output_tokens: None,
            total_cache_read_input_tokens: None,
            total_cache_creation_input_tokens: None,
            total_turns: None,
            total_cost_usd: None,
            total_duration_ms: None,
            model: None,
        }];
        let output = format_active_runs_for_slack(&runs);
        assert!(output.contains("Active workflow runs (1)"));
        assert!(output.contains("deploy"));
        assert!(output.contains("my-repo/main"));
        assert!(output.contains(":arrows_counterclockwise:"));
    }
}
