use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use conductor_core::config::save_config;
use conductor_core::notification_event::{NotificationEvent, ALL_EVENTS};
use conductor_core::notification_hooks::HookRunner;

use crate::error::ApiError;
use crate::state::AppState;

/// Slimmed response type for `GET /api/config/hooks`.
///
/// Avoids exposing raw `headers` map values (which may contain `$ENV_VAR` references)
/// and keeps the API surface minimal and stable even if `HookConfig` fields change.
#[derive(Serialize, utoipa::ToSchema)]
pub struct HookSummary {
    pub index: usize,
    pub on: String,
    /// `"shell"` when a `run` command is configured, `"http"` otherwise.
    pub kind: &'static str,
    /// First 80 characters of `run` (shell) or `url` (HTTP), with `…` appended if truncated.
    pub command: Option<String>,
    /// `true` when `on` contains a wildcard `*` character (e.g. `"*"` or `"workflow_run.*"`).
    pub is_wildcard: bool,
}

/// Request body for `POST /api/config/hooks/test`.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct TestHookRequest {
    pub hook_index: usize,
}

/// Request body for `PATCH /api/config/hooks/:index/on`.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct PatchHookOnRequest {
    /// New event pattern for this hook (e.g. `"workflow_run.completed"`, `"*"`).
    pub on: String,
}

/// A single lifecycle event entry returned by `GET /api/config/hooks/events`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct HookEventEntry {
    pub name: &'static str,
    pub label: &'static str,
}

fn truncate_command(s: &str) -> String {
    let max = 80;
    if s.chars().count() > max {
        let boundary = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}…", &s[..boundary])
    } else {
        s.to_string()
    }
}

/// `GET /api/config/hooks` — return all configured notification hooks.
#[utoipa::path(
    get,
    path = "/api/config/hooks",
    responses(
        (status = 200, description = "List of configured notification hooks", body = Vec<HookSummary>),
    ),
    tag = "hooks",
)]
pub async fn list_hooks(State(state): State<AppState>) -> Result<Json<Vec<HookSummary>>, ApiError> {
    let config = state.config.read().await;
    let summaries = config
        .notify
        .hooks
        .iter()
        .enumerate()
        .map(|(index, hook)| {
            let kind = if hook.run.is_some() { "shell" } else { "http" };
            let command = hook
                .run
                .as_deref()
                .or(hook.url.as_deref())
                .map(truncate_command);
            let is_wildcard = hook.on.contains('*');
            HookSummary {
                index,
                on: hook.on.clone(),
                kind,
                command,
                is_wildcard,
            }
        })
        .collect();
    Ok(Json(summaries))
}

/// `POST /api/config/hooks/test` — fire a synthetic event through a single configured hook
/// identified by its index.
///
/// The synthetic event is chosen to match the hook's configured `on` pattern so the
/// hook actually fires rather than being silently skipped by the pattern filter.
///
/// Returns 204 immediately. The hook executes fire-and-forget in a background
/// OS thread; errors appear in hook output, not in the HTTP response.
#[utoipa::path(
    post,
    path = "/api/config/hooks/test",
    request_body(content = TestHookRequest, description = "Hook index to test"),
    responses(
        (status = 204, description = "Hook fired successfully"),
        (status = 404, description = "Hook index not found"),
    ),
    tag = "hooks",
)]
pub async fn test_hook(
    State(state): State<AppState>,
    Json(body): Json<TestHookRequest>,
) -> Result<StatusCode, ApiError> {
    let config = state.config.read().await;
    let hook = config
        .notify
        .hooks
        .get(body.hook_index)
        .ok_or_else(|| ApiError::NotFound(format!("hook index {} not found", body.hook_index)))?
        .clone();
    drop(config);

    let now = Utc::now().to_rfc3339();
    let event = NotificationEvent::synthetic_for_pattern(&hook.on, now);

    let runner = HookRunner::new(&[hook]);
    runner.fire(&event);

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/config/hooks/events` — return the static list of lifecycle event names
/// and display labels used to populate the hook × event matrix UI.
///
/// Threshold-based events (`cost_spike`, `duration_spike`, `gate.pending_too_long`)
/// are excluded because they require additional filter fields.
#[utoipa::path(
    get,
    path = "/api/config/hooks/events",
    responses(
        (status = 200, description = "List of lifecycle event names and labels", body = Vec<HookEventEntry>),
    ),
    tag = "hooks",
)]
pub async fn list_hook_events() -> Json<Vec<HookEventEntry>> {
    let events = ALL_EVENTS
        .iter()
        .map(|(name, label)| HookEventEntry { name, label })
        .collect();
    Json(events)
}

/// `PATCH /api/config/hooks/:index/on` — update the `on` pattern for a single
/// configured notification hook identified by its zero-based index.
///
/// Writes the updated pattern to `~/.conductor/config.toml` and returns the
/// updated `HookSummary`. Returns 404 if the index is out of range.
#[utoipa::path(
    patch,
    path = "/api/config/hooks/{index}/on",
    params(
        ("index" = usize, Path, description = "Zero-based hook index"),
    ),
    request_body(content = PatchHookOnRequest, description = "New event pattern"),
    responses(
        (status = 200, description = "Updated hook summary", body = HookSummary),
        (status = 404, description = "Hook index not found"),
    ),
    tag = "hooks",
)]
pub async fn patch_hook_on(
    State(state): State<AppState>,
    Path(index): Path<usize>,
    Json(body): Json<PatchHookOnRequest>,
) -> Result<Json<HookSummary>, ApiError> {
    let mut config = state.config.write().await;
    let hook = config
        .notify
        .hooks
        .get_mut(index)
        .ok_or_else(|| ApiError::NotFound(format!("hook index {index} not found")))?;
    hook.on = body.on;
    let kind: &'static str = if hook.run.is_some() { "shell" } else { "http" };
    let command = hook
        .run
        .as_deref()
        .or(hook.url.as_deref())
        .map(truncate_command);
    let on = hook.on.clone();
    let is_wildcard = on.contains('*');
    save_config(&config)?;
    Ok(Json(HookSummary {
        index,
        on,
        kind,
        command,
        is_wildcard,
    }))
}

#[cfg(test)]
mod tests {
    use super::truncate_command;

    #[test]
    fn short_string_unchanged() {
        assert_eq!(truncate_command("echo hello"), "echo hello");
    }

    #[test]
    fn exactly_80_chars_unchanged() {
        let s: String = "a".repeat(80);
        assert_eq!(truncate_command(&s), s);
    }

    #[test]
    fn over_80_chars_truncated_with_ellipsis() {
        let s: String = "a".repeat(81);
        let result = truncate_command(&s);
        assert!(result.ends_with('…'), "should end with ellipsis");
        // 80 'a' chars + '…' (3 bytes in UTF-8)
        assert_eq!(result.chars().count(), 81); // 80 + ellipsis char
    }

    #[test]
    fn multibyte_chars_truncated_at_char_boundary() {
        // Each '中' is 3 bytes — 81 chars would be 243 bytes, but we should
        // truncate on char boundary not byte boundary.
        let s: String = "中".repeat(81);
        let result = truncate_command(&s);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 81); // 80 '中' + '…'
        let _ = result.as_str(); // valid UTF-8, no panic on slice
    }

    #[test]
    fn empty_string_unchanged() {
        assert_eq!(truncate_command(""), "");
    }
}
