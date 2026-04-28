//! Shared runtime helpers for spawning and polling agent runs.
//!
//! Backward-compatible wrappers around `runkon-runtimes` headless primitives
//! that preserve the old conductor-core-specific signatures (e.g.
//! `drain_stream_json` with `AgentManager` + callback).

use std::borrow::Cow;

// Re-export unchanged headless primitives from runkon-runtimes.
pub use runkon_runtimes::headless::{
    build_agent_args, build_agent_args_with_mode, build_headless_agent_args,
    DrainOutcome, HeadlessHandle, SpawnHeadlessParams,
};

/// Resolve the path to the `conductor` binary.
///
/// Looks for a sibling `conductor` next to the current executable first,
/// then falls back to the bare name (relying on `$PATH`).
fn resolve_conductor_bin() -> String {
    let resolved = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let sibling = p.parent()?.join("conductor");
            sibling
                .exists()
                .then(|| sibling.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "conductor".to_string());
    tracing::debug!("[conductor] resolved binary: {resolved}");
    resolved
}

/// Spawn a headless conductor subprocess.
///
/// Backward-compatible wrapper that resolves the conductor binary internally
/// and delegates to `runkon_runtimes::headless::spawn_headless`.
#[cfg(unix)]
pub fn spawn_headless(
    args: &[Cow<'static, str>],
    working_dir: &std::path::Path,
) -> std::result::Result<HeadlessHandle, String> {
    let binary_path = resolve_conductor_bin();
    runkon_runtimes::headless::spawn_headless(args, working_dir, &binary_path)
}

/// Build headless args and spawn the conductor subprocess in one step.
///
/// Backward-compatible wrapper that resolves the conductor binary internally
/// and delegates to `runkon_runtimes::headless::try_spawn_headless_run`.
#[cfg(unix)]
pub fn try_spawn_headless_run(
    params: &SpawnHeadlessParams<'_>,
) -> std::result::Result<(HeadlessHandle, std::path::PathBuf), String> {
    let binary_path = resolve_conductor_bin();
    runkon_runtimes::headless::try_spawn_headless_run(params, &binary_path)
}

/// Drain the stdout of a headless subprocess, persisting events to the DB.
///
/// Reads `stdout` line-by-line via `BufReader`, writes each line to `log_file`,
/// calls `parse_events_from_value()` to produce `AgentEvent` values for the
/// `on_event` callback, and makes eager DB writes:
/// - `system/init` → `update_run_model_and_session`
/// - `assistant` with usage → `update_run_tokens_partial`
/// - `result` → `update_run_completed_if_running` or `update_run_failed_with_session`
///   and returns [`DrainOutcome::Completed`]
///
/// Returns [`DrainOutcome::NoResult`] on EOF without a `result` event (e.g. SIGTERM).
///
/// **Blocking** — must not be called from the TUI main thread or an async context.
/// Use `std::thread::spawn` to run this in a background thread.
pub fn drain_stream_json(
    stdout: impl std::io::Read,
    run_id: &str,
    log_file: &std::path::Path,
    mgr: &crate::agent::AgentManager<'_>,
    on_event: impl Fn(&crate::agent::types::AgentEvent),
) -> DrainOutcome {
    use std::io::{BufRead, BufReader, Write};

    let mut log_writer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|e| {
            tracing::warn!(
                "[drain_stream_json] failed to open log file {}: {e}",
                log_file.display()
            );
        })
        .ok();

    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };

        // Persist to log file (best-effort; I/O errors don't abort the drain)
        if let Some(ref mut w) = log_writer {
            if let Err(e) = writeln!(w, "{line}") {
                tracing::warn!("[drain_stream_json] failed to write log line: {e}");
            }
        }

        // Parse once for both display events and DB writes
        let value = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Fire display-event callback
        let events = crate::agent::log_parsing::parse_events_from_value(&value);
        for event in &events {
            on_event(event);
        }

        let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "system" => {
                let subtype = value.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
                if subtype == "init" {
                    let model = value.get("model").and_then(|v| v.as_str());
                    let session_id = value.get("session_id").and_then(|v| v.as_str());
                    if let Err(e) = mgr.update_run_model_and_session(run_id, model, session_id) {
                        tracing::warn!("[drain_stream_json] failed to update model/session: {e}");
                    }
                }
            }
            "assistant" => {
                let usage = value
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .or_else(|| value.get("usage"));
                if let Some(usage) = usage {
                    let input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let cache_create = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    if let Err(e) = mgr.update_run_tokens_partial(
                        run_id,
                        input,
                        output,
                        cache_read,
                        cache_create,
                    ) {
                        tracing::warn!("[drain_stream_json] failed to update tokens: {e}");
                    }
                }
            }
            "result" => {
                let log_result = crate::agent::log_parsing::parse_result_event(&value);
                if log_result.is_error {
                    let error_msg = log_result
                        .result_text
                        .as_deref()
                        .unwrap_or(crate::agent::status::DEFAULT_AGENT_ERROR_MSG);
                    if let Err(e) = mgr.update_run_failed_with_session(
                        run_id,
                        error_msg,
                        log_result.session_id.as_deref(),
                    ) {
                        tracing::warn!("[drain_stream_json] failed to mark run failed: {e}");
                    }
                } else {
                    // Use the if_running variant to avoid clobbering a value already written
                    // by the subprocess itself (double-write safety). Persist all result-event
                    // fields (cost_usd, num_turns, duration_ms, final token counts).
                    if let Err(e) = mgr.update_run_completed_if_running_full(run_id, &log_result) {
                        tracing::warn!("[drain_stream_json] failed to mark run completed: {e}");
                    }
                }
                return DrainOutcome::Completed;
            }
            _ => {}
        }
    }

    DrainOutcome::NoResult
}
