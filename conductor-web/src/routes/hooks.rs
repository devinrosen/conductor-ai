use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use conductor_core::config::{hooks_dir, save_config, HookConfig};
use conductor_core::notification_event::{NotificationEvent, ALL_EVENTS};
use conductor_core::notification_hooks::HookRunner;

use crate::error::ApiError;
use crate::state::AppState;

/// Slimmed response type for `GET /api/config/hooks`.
///
/// Avoids exposing raw `headers` map values (which may contain `$ENV_VAR` references)
/// and keeps the API surface minimal and stable even if `HookConfig` fields change.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HookSummary {
    pub index: usize,
    pub on: String,
    /// `"shell"` when a `run` command is configured, `"http"` otherwise.
    pub kind: &'static str,
    /// Short display name: filename (for file-path commands) or truncated command/URL.
    pub label: String,
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
    /// `true` for workflow events that support the `:root` modifier.
    pub is_workflow: bool,
}

fn hook_to_summary(index: usize, hook: &HookConfig) -> HookSummary {
    let kind = if hook.run.is_some() { "shell" } else { "http" };
    let raw = hook.run.as_deref().or(hook.url.as_deref());
    let label = raw.map(extract_label).unwrap_or_default();
    let command = raw.map(truncate_command);
    let on = hook.on.clone();
    let is_wildcard = on.split(',').any(|p| p.trim().contains('*'));
    HookSummary {
        index,
        on,
        kind,
        label,
        command,
        is_wildcard,
    }
}

/// Extract a short display label from a command or URL string.
///
/// - File-like paths (`/foo/bar/notify.sh`, `~/.conductor/hooks/slack.py`) → filename (`notify.sh`, `slack.py`)
/// - URLs (`https://hooks.example.com/conductor`) → hostname + first path segment
/// - Inline commands (`echo hello && curl ...`) → first 30 chars
fn extract_label(s: &str) -> String {
    // URL: extract hostname
    if s.starts_with("http://") || s.starts_with("https://") {
        if let Some(after_scheme) = s.split("://").nth(1) {
            let host_and_path = after_scheme
                .split('/')
                .take(2)
                .collect::<Vec<_>>()
                .join("/");
            return truncate_label(&host_and_path);
        }
    }
    // File path: extract filename
    if let Some(filename) = std::path::Path::new(s).file_name() {
        let name = filename.to_string_lossy();
        if name.contains('.') || s.contains('/') {
            return name.to_string();
        }
    }
    // Inline command: truncate
    truncate_label(s)
}

fn truncate_label(s: &str) -> String {
    let max = 30;
    if s.chars().count() > max {
        let boundary = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}…", &s[..boundary])
    } else {
        s.to_string()
    }
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

/// Scan `~/.conductor/hooks/` for script files and add any that aren't already
/// referenced by an existing `[[notify.hooks]]` entry. Returns `true` if new
/// hooks were added (caller should save config).
fn discover_hook_scripts(hooks: &mut Vec<HookConfig>) -> bool {
    let dir = hooks_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return false, // directory doesn't exist yet — nothing to discover
    };

    let known_runs: std::collections::HashSet<String> = hooks
        .iter()
        .filter_map(|h| h.run.as_ref())
        .cloned()
        .collect();

    let mut added = false;
    let mut scripts: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            let is_file = path.is_file();
            let has_ext = matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("sh" | "py" | "rb" | "js" | "ts" | "bash" | "zsh" | "fish")
            );
            is_file && has_ext
        })
        .collect();
    scripts.sort_by_key(|e| e.file_name());

    for entry in scripts {
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();
        // Also check by filename in case the run field uses a relative path
        let file_name = entry.file_name().to_string_lossy().to_string();
        let already_registered = known_runs
            .iter()
            .any(|r| r == &path_str || r.ends_with(&format!("/{file_name}")));
        if !already_registered {
            hooks.push(HookConfig {
                on: String::new(),
                run: Some(path_str),
                ..Default::default()
            });
            added = true;
        }
    }
    added
}

/// `GET /api/config/hooks` — return all configured notification hooks.
///
/// Also discovers script files in `~/.conductor/hooks/` and auto-registers
/// any that aren't already referenced by an existing hook entry.
#[utoipa::path(
    get,
    path = "/api/config/hooks",
    responses(
        (status = 200, description = "List of configured notification hooks", body = Vec<HookSummary>),
    ),
    tag = "hooks",
)]
pub async fn list_hooks(State(state): State<AppState>) -> Result<Json<Vec<HookSummary>>, ApiError> {
    // Discover hook scripts from ~/.conductor/hooks/ and auto-register new ones.
    let needs_save = {
        let mut config = state.config.write().await;
        let added = discover_hook_scripts(&mut config.notify.hooks);
        if added {
            let snapshot = config.clone();
            drop(config);
            Some(snapshot)
        } else {
            None
        }
    };
    if let Some(snapshot) = needs_save {
        save_config(&snapshot)?;
    }

    let config = state.config.read().await;
    let summaries = config
        .notify
        .hooks
        .iter()
        .enumerate()
        .map(|(index, hook)| hook_to_summary(index, hook))
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
        .map(|(name, label, is_workflow)| HookEventEntry {
            name,
            label,
            is_workflow: *is_workflow,
        })
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
    let (summary, config_snapshot) = {
        let mut config = state.config.write().await;
        let hook = config
            .notify
            .hooks
            .get_mut(index)
            .ok_or_else(|| ApiError::NotFound(format!("hook index {index} not found")))?;
        hook.on = body.on;
        let summary = hook_to_summary(index, hook);
        let config_snapshot = config.clone();
        (summary, config_snapshot)
        // write lock released here
    };
    save_config(&config_snapshot)?;
    Ok(Json(summary))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::Json;
    use conductor_core::config::{Config, HookConfig};
    use tempfile::NamedTempFile;
    use tokio::sync::{Mutex, RwLock};

    use super::{patch_hook_on, truncate_command, PatchHookOnRequest};
    use crate::events::EventBus;
    use crate::state::AppState;

    fn state_with_hooks(hooks: Vec<HookConfig>) -> (AppState, NamedTempFile) {
        let tmp = NamedTempFile::new().expect("create temp db file");
        let conn = conductor_core::db::open_database(tmp.path()).expect("open temp db");
        let mut config = Config::default();
        config.notify.hooks = hooks;
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(config)),
            events: EventBus::new(1),
            db_path: tmp.path().to_path_buf(),
            workflow_done_notify: None,
        };
        (state, tmp)
    }

    #[tokio::test]
    async fn patch_hook_on_returns_404_for_out_of_range_index() {
        let (state, _tmp) = state_with_hooks(vec![]);
        let result = patch_hook_on(
            State(state),
            Path(0),
            Json(PatchHookOnRequest {
                on: "workflow_run.completed".into(),
            }),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{err:?}").contains("not found"),
            "expected NotFound, got {err:?}"
        );
    }

    #[tokio::test]
    async fn patch_hook_on_updates_on_field_and_returns_summary() {
        let hook = HookConfig {
            on: "workflow_run.completed".into(),
            run: Some("echo hello".into()),
            ..Default::default()
        };
        let (state, _tmp) = state_with_hooks(vec![hook]);
        let result = patch_hook_on(
            State(state.clone()),
            Path(0),
            Json(PatchHookOnRequest { on: "*".into() }),
        )
        .await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let Json(summary) = result.unwrap();
        assert_eq!(summary.index, 0);
        assert_eq!(summary.on, "*");
        assert!(summary.is_wildcard);
        assert_eq!(summary.kind, "shell");
        // Verify in-memory state was updated
        let config = state.config.read().await;
        assert_eq!(config.notify.hooks[0].on, "*");
    }

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
