//! Shared runtime helpers for spawning agent runs and bridging
//! `runkon-runtimes` events into `AgentManager` DB writes.
//!
//! - [`conductor_headless`]: owns the `conductor agent run` argv shape
//!   (`SpawnHeadlessParams`, `build_headless_agent_args`, `try_spawn_headless_run`).
//!   Lives here (not in `runkon-runtimes`) because the argv shape is conductor-CLI-specific.
//! - [`conductor_argv_builder`]: factory that creates the [`runkon_runtimes::runtime::claude::ArgvBuilder`]
//!   closure wired into `RuntimeOptions` at all conductor call sites.
//! - [`CombinedSink`]: the [`runkon_runtimes::tracker::EventSink`] used by callers
//!   to persist runtime events into `AgentManager` while also forwarding parsed
//!   `AgentEvent`s to a UI/WebSocket callback. Construct it via
//!   [`CombinedSink::new`] and pass it to [`drain_stream_json`].

pub mod conductor_headless;

use std::borrow::Cow;

/// Default stall-detection threshold for agent JSONL streams.
///
/// If no output is received for this duration, the drain loop returns
/// `DrainOutcome::StalledOut` and the run is marked failed with reason
/// `"stall_timeout"`. Conservative (5 min) to avoid false positives from
/// legitimate long-running tool calls or prompt-cache writes.
pub const DEFAULT_STALL_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(300);

// Re-export runtime-agnostic headless primitives from runkon-runtimes.
pub use runkon_runtimes::headless::{DrainOutcome, HeadlessHandle};
// Re-export conductor-CLI-specific argv builder from the local submodule.
pub use conductor_headless::{build_headless_agent_args, SpawnHeadlessParams};
pub use runkon_runtimes::tracker::EventSink;

/// Resolve the path to the `conductor` binary.
///
/// Looks for a sibling `conductor` next to the current executable first,
/// then falls back to the bare name (relying on `$PATH`).
pub fn resolve_conductor_bin() -> String {
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
/// and delegates to [`conductor_headless::try_spawn_headless_run`].
#[cfg(unix)]
pub fn try_spawn_headless_run(
    params: &SpawnHeadlessParams<'_>,
) -> std::result::Result<(HeadlessHandle, std::path::PathBuf), String> {
    let binary_path = resolve_conductor_bin();
    conductor_headless::try_spawn_headless_run(params, &binary_path)
}

/// Create the [`runkon_runtimes::runtime::claude::ArgvBuilder`] closure that
/// translates a [`runkon_runtimes::runtime::claude::ClaudeArgvRequest`] into
/// `conductor agent run` argv via [`build_headless_agent_args`].
///
/// Wire this into `RuntimeOptions::argv_builder` at every conductor call site.
pub fn conductor_argv_builder() -> runkon_runtimes::runtime::claude::ArgvBuilder {
    std::sync::Arc::new(
        |req: &runkon_runtimes::runtime::claude::ClaudeArgvRequest<'_>| {
            let params = conductor_headless::SpawnHeadlessParams {
                run_id: req.run_id,
                working_dir: req.working_dir,
                prompt: req.prompt,
                resume_session_id: req.resume_session_id,
                model: req.model,
                extra_cli_args: req.extra_cli_args,
                permission_mode: req.permission_mode,
                plugin_dirs: req.plugin_dirs,
            };
            let (args, pf) = conductor_headless::build_headless_agent_args(&params)?;
            Ok((args, Some(pf)))
        },
    )
}

/// Drain a streaming JSON output from a headless agent subprocess.
///
/// Thin wrapper over `runkon_runtimes::headless::drain_stream_json` so that
/// callers in conductor-tui and conductor-web stay within the conductor-core
/// abstraction layer rather than reaching into runkon-runtimes directly.
pub fn drain_stream_json(
    reader: impl std::io::Read + Send + 'static,
    run_id: &str,
    log_path: &std::path::Path,
    sink: &impl runkon_runtimes::tracker::EventSink,
    stall_threshold: Option<std::time::Duration>,
) -> DrainOutcome {
    runkon_runtimes::headless::drain_stream_json(reader, run_id, log_path, sink, stall_threshold)
}

/// `EventSink` that persists runtime events into [`AgentManager`] (model/session,
/// token deltas, completion/failure) and fans `AgentEvent`s out to a UI callback.
///
/// Construct with [`CombinedSink::new`] and pass to [`drain_stream_json`]:
///
/// ```ignore
/// let sink = CombinedSink::new(&mgr, |event| { /* forward to UI */ });
/// conductor_core::agent_runtime::drain_stream_json(stdout, run_id, &log_path, &sink);
/// ```
///
/// [`AgentManager`]: crate::agent::AgentManager
pub struct CombinedSink<'a, F> {
    mgr: &'a crate::agent::AgentManager<'a>,
    on_event_cb: F,
}

impl<'a, F: Fn(&crate::agent::types::AgentEvent)> CombinedSink<'a, F> {
    /// Build a sink that updates `mgr` from runtime events and forwards
    /// parsed display events to `on_event_cb`.
    pub fn new(mgr: &'a crate::agent::AgentManager<'a>, on_event_cb: F) -> Self {
        Self { mgr, on_event_cb }
    }
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
                if let Err(e) = self
                    .mgr
                    .update_run_completed_if_running_full(run_id, &log_result)
                {
                    tracing::warn!("[drain_stream_json] failed to mark run completed: {e}");
                }
            }
            RuntimeEvent::Failed { error, session_id } => {
                if let Err(e) =
                    self.mgr
                        .update_run_failed_with_session(run_id, &error, session_id.as_deref())
                {
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
        let sink = CombinedSink::new(&mgr, |ev| {
            captured.borrow_mut().push(ev.clone());
        });
        let outcome = runkon_runtimes::headless::drain_stream_json(
            std::io::Cursor::new(input.into_bytes()),
            run_id,
            &log,
            &sink,
            None,
        );
        let _ = std::fs::remove_file(&log);
        (outcome, captured.into_inner())
    }

    #[test]
    fn combined_sink_init_calls_mgr() {
        let (conn, run_id) = setup();
        drain(
            &conn,
            &run_id,
            &[
                r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-6","session_id":"sess-abc"}"#,
            ],
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
            &[
                r#"{"type":"assistant","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":3}}"#,
            ],
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
        assert_eq!(run.status, crate::agent::status::AgentRunStatus::Completed);
        assert_eq!(run.result_text, Some("all done".to_string()));
        assert_eq!(run.cost_usd, Some(0.42));
    }

    #[test]
    fn combined_sink_failed_returns_completed() {
        let (conn, run_id) = setup();
        let (outcome, _) = drain(
            &conn,
            &run_id,
            &[
                r#"{"type":"result","is_error":true,"result":"something went wrong","session_id":"sess-fail"}"#,
            ],
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
            &[
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello world"}],"usage":{"input_tokens":5,"output_tokens":3}}}"#,
            ],
        );
        assert!(
            events.iter().any(|e| e.kind == "text"),
            "expected at least one text display event, got: {events:?}"
        );
    }

    #[test]
    fn drain_no_result_returns_no_result() {
        let (conn, run_id) = setup();
        let (outcome, _) = drain(
            &conn,
            &run_id,
            &[
                r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-6","session_id":"sess-abc"}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"thinking..."}]}}"#,
            ],
        );
        assert_eq!(outcome, DrainOutcome::NoResult);
    }

    #[test]
    fn combined_sink_manager_errors_do_not_abort_drain() {
        let (conn, run_id) = setup();
        // Force all manager DB writes to fail so the tracing::warn! branches execute.
        conn.execute_batch("ALTER TABLE agent_runs RENAME TO agent_runs_bak")
            .unwrap();
        let (outcome, _) = drain(
            &conn,
            &run_id,
            &[r#"{"type":"result","result":"done","total_cost_usd":0.1,"num_turns":1}"#],
        );
        // DrainOutcome is determined by the event stream, not DB write success.
        assert_eq!(outcome, DrainOutcome::Completed);
        conn.execute_batch("ALTER TABLE agent_runs_bak RENAME TO agent_runs")
            .unwrap();
    }
}
