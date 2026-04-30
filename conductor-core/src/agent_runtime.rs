//! Shared runtime helpers for spawning and polling agent runs.
//!
//! Backward-compatible wrappers around `runkon-runtimes` headless primitives
//! that preserve the old conductor-core-specific signatures (e.g.
//! `drain_stream_json` with `AgentManager` + callback).

use std::borrow::Cow;

// Re-export unchanged headless primitives from runkon-runtimes.
pub use runkon_runtimes::headless::{
    build_headless_agent_args, resolve_conductor_bin, DrainOutcome, HeadlessHandle,
    SpawnHeadlessParams,
};

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

struct CombinedSink<'a, F> {
    mgr: &'a crate::agent::AgentManager<'a>,
    on_event_cb: F,
}

impl<'a, F: Fn(&crate::agent::types::AgentEvent)> runkon_runtimes::tracker::EventSink
    for CombinedSink<'a, F>
{
    fn on_event(&self, run_id: &str, event: runkon_runtimes::tracker::RuntimeEvent) {
        use runkon_runtimes::tracker::RuntimeEvent;
        match event {
            RuntimeEvent::Init { model, session_id } => {
                if let Err(e) = self.mgr.update_run_model_and_session(
                    run_id,
                    model.as_deref(),
                    session_id.as_deref(),
                ) {
                    tracing::warn!("[drain_stream_json] failed to update model/session: {e}");
                }
            }
            RuntimeEvent::Tokens {
                input,
                output,
                cache_read,
                cache_create,
            } => {
                if let Err(e) = self.mgr.update_run_tokens_partial(
                    run_id,
                    input,
                    output,
                    cache_read,
                    cache_create,
                ) {
                    tracing::warn!("[drain_stream_json] failed to update tokens: {e}");
                }
            }
            RuntimeEvent::Completed {
                result_text,
                session_id,
                cost_usd,
                num_turns,
                duration_ms,
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                let log_result = crate::agent::types::LogResult {
                    result_text,
                    session_id,
                    cost_usd,
                    num_turns,
                    duration_ms,
                    is_error: false,
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                };
                if let Err(e) =
                    self.mgr.update_run_completed_if_running_full(run_id, &log_result)
                {
                    tracing::warn!("[drain_stream_json] failed to mark run completed: {e}");
                }
            }
            RuntimeEvent::Failed { error, session_id } => {
                if let Err(e) = self.mgr.update_run_failed_with_session(
                    run_id,
                    &error,
                    session_id.as_deref(),
                ) {
                    tracing::warn!("[drain_stream_json] failed to mark run failed: {e}");
                }
            }
        }
    }

    fn on_raw_value(&self, _run_id: &str, value: &serde_json::Value) {
        let events = crate::agent::log_parsing::parse_events_from_value(value);
        for event in &events {
            (self.on_event_cb)(event);
        }
    }
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
    let sink = CombinedSink {
        mgr,
        on_event_cb: on_event,
    };
    runkon_runtimes::headless::drain_stream_json(stdout, run_id, log_file, &sink)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (rusqlite::Connection, String) {
        let conn = crate::test_helpers::setup_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "test prompt", None).unwrap();
        (conn, run.id)
    }

    fn temp_log() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "test-agent-runtime-{:?}.log",
            std::thread::current().id()
        ))
    }

    fn drain(
        conn: &rusqlite::Connection,
        run_id: &str,
        json_lines: &[&str],
    ) -> (DrainOutcome, Vec<crate::agent::types::AgentEvent>) {
        let input = json_lines.join("\n");
        let log = temp_log();
        let mgr = crate::agent::AgentManager::new(conn);
        let captured = std::cell::RefCell::new(Vec::new());
        let outcome = drain_stream_json(input.as_bytes(), run_id, &log, &mgr, |ev| {
            captured.borrow_mut().push(ev.clone());
        });
        let _ = std::fs::remove_file(&log);
        (outcome, captured.into_inner())
    }

    #[test]
    fn combined_sink_init_calls_mgr() {
        let (conn, run_id) = setup();
        drain(
            &conn,
            &run_id,
            &[r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-6","session_id":"sess-abc"}"#],
        );
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.get_run(&run_id).unwrap().unwrap();
        assert_eq!(run.model, Some("claude-sonnet-4-6".to_string()));
        assert_eq!(run.session_id, Some("sess-abc".to_string()));
    }

    #[test]
    fn combined_sink_tokens_calls_mgr() {
        let (conn, run_id) = setup();
        drain(
            &conn,
            &run_id,
            &[r#"{"type":"assistant","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}}"#],
        );
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.get_run(&run_id).unwrap().unwrap();
        assert_eq!(run.input_tokens, Some(10));
        assert_eq!(run.output_tokens, Some(20));
        assert_eq!(run.cache_read_input_tokens, Some(5));
        assert_eq!(run.cache_creation_input_tokens, Some(3));
    }

    #[test]
    fn combined_sink_completed_returns_completed() {
        let (conn, run_id) = setup();
        let (outcome, _) = drain(
            &conn,
            &run_id,
            &[r#"{"type":"result","result":"all done","total_cost_usd":0.42,"num_turns":3}"#],
        );
        assert_eq!(outcome, DrainOutcome::Completed);
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.get_run(&run_id).unwrap().unwrap();
        assert_eq!(
            run.status,
            crate::agent::status::AgentRunStatus::Completed
        );
        assert_eq!(run.result_text, Some("all done".to_string()));
        assert_eq!(run.cost_usd, Some(0.42));
    }

    #[test]
    fn combined_sink_failed_returns_completed() {
        let (conn, run_id) = setup();
        let (outcome, _) = drain(
            &conn,
            &run_id,
            &[r#"{"type":"result","is_error":true,"result":"something went wrong","session_id":"sess-fail"}"#],
        );
        assert_eq!(outcome, DrainOutcome::Completed);
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.get_run(&run_id).unwrap().unwrap();
        assert_eq!(run.status, crate::agent::status::AgentRunStatus::Failed);
        assert_eq!(run.session_id, Some("sess-fail".to_string()));
    }

    #[test]
    fn combined_sink_fires_display_events() {
        let (conn, run_id) = setup();
        let (_, events) = drain(
            &conn,
            &run_id,
            &[r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello world"}],"usage":{"input_tokens":5,"output_tokens":3}}}"#],
        );
        assert!(
            events.iter().any(|e| e.kind == "text"),
            "expected at least one text display event, got: {events:?}"
        );
    }
}
