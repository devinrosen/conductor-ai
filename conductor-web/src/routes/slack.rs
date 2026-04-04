use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Form;
use serde::{Deserialize, Serialize};

use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};

use crate::state::AppState;

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

/// Handle Slack slash commands.
///
/// Supports:
///   /conductor active   — list active workflow runs
///   /conductor help     — show available commands
pub async fn handle_slash_command(
    State(state): State<AppState>,
    Form(payload): Form<SlackSlashCommand>,
) -> impl IntoResponse {
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
            return SlackResponse {
                response_type: "ephemeral",
                text: format!("Error querying workflows: {e}"),
            };
        }
    };

    if runs.is_empty() {
        return SlackResponse {
            response_type: "in_channel",
            text: "No active workflow runs.".to_string(),
        };
    }

    let mut lines = vec![format!("*Active workflow runs ({}):*", runs.len())];
    for run in &runs {
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

    SlackResponse {
        response_type: "in_channel",
        text: lines.join("\n"),
    }
}
