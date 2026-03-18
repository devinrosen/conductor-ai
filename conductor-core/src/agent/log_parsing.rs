use std::fs;
use std::path::Path;

use super::status::DEFAULT_AGENT_ERROR_MSG;
use super::types::{AgentEvent, AgentRun, LogResult};

/// Extract the protocol fields from a `result` JSON event.
pub fn parse_result_event(event: &serde_json::Value) -> LogResult {
    let usage = event.get("usage");
    LogResult {
        result_text: event
            .get("result")
            .and_then(|v| v.as_str())
            .map(String::from),
        cost_usd: event.get("total_cost_usd").and_then(|v| v.as_f64()),
        num_turns: event.get("num_turns").and_then(|v| v.as_i64()),
        duration_ms: event.get("duration_ms").and_then(|v| v.as_i64()),
        is_error: event
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        input_tokens: usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_i64()),
        output_tokens: usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_i64()),
        cache_read_input_tokens: usage
            .and_then(|u| u.get("cache_read_input_tokens"))
            .and_then(|v| v.as_i64()),
        cache_creation_input_tokens: usage
            .and_then(|u| u.get("cache_creation_input_tokens"))
            .and_then(|v| v.as_i64()),
    }
}

/// Scan an agent log file at the given path for the `result` event.
///
/// Reads the last 4 KB of the file (the result event is always the final line),
/// keeping the scan O(1) regardless of log size.
pub(crate) fn scan_log_for_result_at(path: &std::path::Path) -> Option<LogResult> {
    use std::io::{Read as _, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();

    // Read at most the last 4 KB — generous for a single JSON line.
    const TAIL_BYTES: u64 = 4096;
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;

    // Walk lines in reverse so we find the result event quickly.
    for line in buf.lines().rev() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("result").is_some() {
            return Some(parse_result_event(&event));
        }
    }
    None
}

/// Try to recover a stuck run by scanning its log file for a result event.
/// If found, updates the DB and returns the refreshed run. Otherwise returns `None`.
pub(crate) fn try_recover_from_log(
    mgr: &super::manager::AgentManager<'_>,
    run_id: &str,
) -> Option<AgentRun> {
    try_recover_from_log_at(mgr, run_id, &crate::config::agent_log_dir())
}

/// Like [`try_recover_from_log`] but reads from `log_dir` instead of the default agent-log
/// directory. Useful in tests to avoid writing to the real `~/.conductor/agent-logs/`.
pub(crate) fn try_recover_from_log_at(
    mgr: &super::manager::AgentManager<'_>,
    run_id: &str,
    log_dir: &std::path::Path,
) -> Option<AgentRun> {
    let log_path = log_dir.join(format!("{run_id}.log"));
    let log_result = scan_log_for_result_at(&log_path)?;
    if log_result.is_error {
        let error_msg = log_result
            .result_text
            .as_deref()
            .unwrap_or(DEFAULT_AGENT_ERROR_MSG);
        if let Err(e) = mgr.update_run_failed(run_id, error_msg) {
            tracing::warn!("failed to mark run {run_id} as failed during log recovery: {e}");
            return None;
        }
    } else if let Err(e) = mgr.update_run_completed(
        run_id,
        None,
        log_result.result_text.as_deref(),
        log_result.cost_usd,
        log_result.num_turns,
        log_result.duration_ms,
        log_result.input_tokens,
        log_result.output_tokens,
        log_result.cache_read_input_tokens,
        log_result.cache_creation_input_tokens,
    ) {
        tracing::warn!("failed to mark run {run_id} as completed during log recovery: {e}");
        return None;
    }
    // DB update succeeded — read back the refreshed run. Warn explicitly on
    // failure so a DB error is never silently dropped after a state mutation.
    match mgr.get_run(run_id) {
        Ok(run) => run,
        Err(e) => {
            tracing::warn!("failed to fetch run {run_id} after log recovery: {e}");
            None
        }
    }
}

/// Parse a single stream-json log line into zero or more display events.
pub fn parse_events_from_line(line: &str) -> Vec<AgentEvent> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "system" => {
            let subtype = value.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            if subtype == "init" {
                let model = value
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                events.push(AgentEvent {
                    kind: "system".to_string(),
                    summary: format!("Session started (model: {model})"),
                });
            }
        }
        "assistant" => {
            let content = value
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());

            if let Some(blocks) = content {
                for block in blocks {
                    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match block_type {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                for text_line in text.lines() {
                                    let trimmed = text_line.trim();
                                    if !trimmed.is_empty() {
                                        events.push(AgentEvent {
                                            kind: "text".to_string(),
                                            summary: trimmed.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                        "tool_use" => {
                            let tool_name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let input = block.get("input");
                            let desc = tool_summary(tool_name, input);
                            events.push(AgentEvent {
                                kind: "tool".to_string(),
                                summary: desc,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        "result" => {
            let cost = value
                .get("total_cost_usd")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let turns = value.get("num_turns").and_then(|v| v.as_i64()).unwrap_or(0);
            let dur_ms = value
                .get("duration_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let dur_s = dur_ms as f64 / 1000.0;
            let is_error = value
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_error {
                let err_text = value
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                events.push(AgentEvent {
                    kind: "error".to_string(),
                    summary: format!("Error: {err_text}"),
                });
            } else {
                events.push(AgentEvent {
                    kind: "result".to_string(),
                    summary: format!("${cost:.4} · {turns} turns · {dur_s:.1}s"),
                });
            }
        }
        // Skip "user" and "rate_limit_event" — noise
        _ => {}
    }

    events
}

/// Parse a stream-json agent log file into displayable events.
/// Each line is a JSON object with a `type` field.
pub fn parse_agent_log(path: &str) -> Vec<AgentEvent> {
    let Ok(contents) = fs::read_to_string(Path::new(path)) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for line in contents.lines() {
        events.extend(parse_events_from_line(line));
    }
    events
}

/// Count the number of assistant turns in a stream-json agent log file.
/// Each JSON line with `"type": "assistant"` counts as one turn.
pub fn count_turns_in_log(path: &str) -> i64 {
    let (_, count) = count_turns_incremental(path, 0, 0);
    count
}

/// Incrementally count assistant turns starting from `prev_offset`.
///
/// Only reads bytes appended since `prev_offset`, avoiding a full-file scan on
/// every poll tick. If the file has been truncated (length < prev_offset), it
/// resets and recounts from the beginning.
///
/// Returns `(new_offset, new_count)` where `new_count = prev_count + newly found turns`.
pub fn count_turns_incremental(path: &str, prev_offset: u64, prev_count: i64) -> (u64, i64) {
    use std::io::{Read as _, Seek, SeekFrom};

    let mut file = match std::fs::File::open(Path::new(path)) {
        Ok(f) => f,
        Err(_) => return (prev_offset, prev_count),
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return (prev_offset, prev_count),
    };

    // Truncation detected — recount from scratch.
    let (offset, base_count) = if len < prev_offset {
        (0u64, 0i64)
    } else {
        (prev_offset, prev_count)
    };

    if offset >= len {
        return (offset, base_count);
    }

    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (offset, base_count);
    }

    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return (offset, base_count);
    }

    // Only process up to the last complete line (ending with '\n').
    // This avoids counting a partial line that is still being written.
    let complete_end = match buf.rfind('\n') {
        Some(pos) => pos + 1,                // include the '\n'
        None => return (offset, base_count), // no complete line yet
    };
    let complete = &buf[..complete_end];

    let mut new_turns: i64 = 0;
    for line in complete.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("assistant") {
            new_turns += 1;
        }
    }

    (offset + complete_end as u64, base_count + new_turns)
}

/// Extract a human-readable summary for a tool_use event.
fn tool_summary(tool_name: &str, input: Option<&serde_json::Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return format!("[{tool_name}]"),
    };

    // Try description first (Bash always has this)
    if let Some(d) = input.get("description").and_then(|v| v.as_str()) {
        return format!("[{tool_name}] {d}");
    }

    // Try command (Bash fallback)
    if let Some(c) = input.get("command").and_then(|v| v.as_str()) {
        // Commands can be multi-line; take just the first line
        let first = c.lines().next().unwrap_or(c);
        return format!("[{tool_name}] {first}");
    }

    // Tool-specific field extraction
    let detail = match tool_name {
        "Read" | "Write" => input.get("file_path").and_then(|v| v.as_str()),
        "Edit" => input.get("file_path").and_then(|v| v.as_str()),
        "Glob" => input.get("pattern").and_then(|v| v.as_str()),
        "Grep" => input.get("pattern").and_then(|v| v.as_str()),
        "Agent" => input
            .get("description")
            .or_else(|| input.get("prompt"))
            .and_then(|v| v.as_str()),
        "WebFetch" => input.get("url").and_then(|v| v.as_str()),
        "WebSearch" => input.get("query").and_then(|v| v.as_str()),
        _ => None,
    };

    match detail {
        Some(d) => format!("[{tool_name}] {d}"),
        None => format!("[{tool_name}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_events_from_line_system_init() {
        let line = r#"{"type":"system","subtype":"init","model":"claude-opus-4-5"}"#;
        let events = parse_events_from_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "system");
        assert!(events[0].summary.contains("claude-opus-4-5"));
    }

    #[test]
    fn test_parse_events_from_line_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"description":"run tests"}}]}}"#;
        let events = parse_events_from_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tool");
        assert!(events[0].summary.contains("Bash"));
        assert!(events[0].summary.contains("run tests"));
    }

    #[test]
    fn test_parse_events_from_line_unknown_type() {
        let line = r#"{"type":"rate_limit_event"}"#;
        let events = parse_events_from_line(line);
        assert!(events.is_empty());
    }

    #[test]
    fn test_count_turns_in_log_basic() {
        // Two assistant lines and one non-assistant line
        let content = concat!(
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
            r#"{"type":"system","subtype":"init","model":"claude-3"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        assert_eq!(count_turns_in_log(&path), 2);
    }

    #[test]
    fn test_count_turns_in_log_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        assert_eq!(count_turns_in_log(&path), 0);
    }

    #[test]
    fn test_count_turns_in_log_no_assistant_lines() {
        let content = concat!(
            r#"{"type":"result","num_turns":3}"#,
            "\n",
            r#"{"type":"system","subtype":"init","model":"claude-3"}"#,
            "\n",
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        assert_eq!(count_turns_in_log(&path), 0);
    }

    #[test]
    fn test_count_turns_in_log_missing_file() {
        // A path that does not exist should return 0 rather than panic
        assert_eq!(count_turns_in_log("/nonexistent/path/to/log.jsonl"), 0);
    }

    #[test]
    fn test_parse_agent_log_uses_from_line() {
        let line1 = r#"{"type":"system","subtype":"init","model":"claude-3"}"#;
        let line2 =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#;
        let content = format!("{line1}\n{line2}\n");

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let events = parse_agent_log(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "system");
        assert_eq!(events[1].kind, "text");
        assert_eq!(events[1].summary, "Hello");
    }

    // ---- count_turns_incremental tests ----

    #[test]
    fn test_count_turns_incremental_from_zero() {
        let content = concat!(
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
            r#"{"type":"system","subtype":"init","model":"claude-3"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let (offset, count) = count_turns_incremental(&path, 0, 0);
        assert_eq!(count, 2);
        assert_eq!(offset, content.len() as u64);
    }

    #[test]
    fn test_count_turns_incremental_resumes() {
        let line1 = r#"{"type":"assistant","message":{"content":[]}}"#;
        let line2 = r#"{"type":"system","subtype":"init","model":"claude-3"}"#;
        let line3 = r#"{"type":"assistant","message":{"content":[]}}"#;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        // Write first two lines
        let initial = format!("{line1}\n{line2}\n");
        std::fs::write(tmp.path(), &initial).unwrap();
        let (offset, count) = count_turns_incremental(&path, 0, 0);
        assert_eq!(count, 1);
        assert_eq!(offset, initial.len() as u64);

        // Append a third line
        let full = format!("{initial}{line3}\n");
        std::fs::write(tmp.path(), &full).unwrap();
        let (offset2, count2) = count_turns_incremental(&path, offset, count);
        assert_eq!(count2, 2);
        assert_eq!(offset2, full.len() as u64);
    }

    #[test]
    fn test_count_turns_incremental_truncation_resets() {
        let content = concat!(
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
        );
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        std::fs::write(tmp.path(), content).unwrap();
        let (offset, count) = count_turns_incremental(&path, 0, 0);
        assert_eq!(count, 2);

        // Truncate to a shorter file with only one assistant line.
        let short = concat!(r#"{"type":"assistant","message":{"content":[]}}"#, "\n",);
        std::fs::write(tmp.path(), short).unwrap();
        let (offset2, count2) = count_turns_incremental(&path, offset, count);
        assert_eq!(count2, 1, "should recount from zero after truncation");
        assert_eq!(offset2, short.len() as u64);
    }

    #[test]
    fn test_count_turns_incremental_no_new_data() {
        let content = concat!(r#"{"type":"assistant","message":{"content":[]}}"#, "\n",);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let (offset, count) = count_turns_incremental(&path, 0, 0);
        assert_eq!(count, 1);

        // Call again with same offset — no new data.
        let (offset2, count2) = count_turns_incremental(&path, offset, count);
        assert_eq!(count2, 1);
        assert_eq!(offset2, offset);
    }

    #[test]
    fn test_count_turns_incremental_missing_file() {
        let (offset, count) = count_turns_incremental("/nonexistent/path.log", 0, 0);
        assert_eq!(offset, 0);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_turns_incremental_partial_line_skipped() {
        // Write a complete line followed by a partial (no trailing newline).
        let complete = r#"{"type":"assistant","message":{"content":[]}}"#;
        let partial = r#"{"type":"assistant","message":{"content":[]"#; // incomplete JSON, no newline
        let content = format!("{complete}\n{partial}");

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &content).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        // Should only count the complete line; offset stops after the '\n'.
        let (offset, count) = count_turns_incremental(&path, 0, 0);
        assert_eq!(
            count, 1,
            "partial line without trailing newline must be skipped"
        );
        let expected_offset = complete.len() as u64 + 1; // +1 for '\n'
        assert_eq!(offset, expected_offset);

        // Once the partial line is completed with a newline, the next call picks it up.
        let finished_line = r#"{"type":"assistant","message":{"content":[]}}"#;
        let finished = format!("{complete}\n{finished_line}\n");
        std::fs::write(tmp.path(), &finished).unwrap();
        let (offset2, count2) = count_turns_incremental(&path, offset, count);
        assert_eq!(count2, 2, "completed line should now be counted");
        assert_eq!(offset2, finished.len() as u64);
    }

    #[test]
    fn test_count_turns_incremental_only_partial_line() {
        // File contains only a partial line (no newline at all).
        let partial = r#"{"type":"assistant","message":{"content":[]}}"#; // no trailing newline
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), partial).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        // No complete lines → count stays at 0, offset stays at 0.
        let (offset, count) = count_turns_incremental(&path, 0, 0);
        assert_eq!(count, 0, "no complete line should yield zero turns");
        assert_eq!(offset, 0, "offset should not advance past partial data");
    }

    // ---- try_recover_from_log_at tests ----

    use crate::agent::manager::AgentManager;
    use crate::agent::status::AgentRunStatus;

    /// Write a minimal completed-result log line to a temp dir keyed by `run_id`.
    fn write_result_log(dir: &std::path::Path, run_id: &str, is_error: bool, result: &str) {
        let content = format!(
            "{{\"type\":\"result\",\"result\":\"{result}\",\"is_error\":{is_error},\
             \"total_cost_usd\":0.01,\"num_turns\":3,\"duration_ms\":500,\
             \"usage\":{{\"input_tokens\":10,\"output_tokens\":20,\
             \"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}}}\n"
        );
        std::fs::write(dir.join(format!("{run_id}.log")), content).unwrap();
    }

    #[test]
    fn test_try_recover_from_log_at_completed() {
        let conn = crate::agent::manager::setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        write_result_log(tmp_dir.path(), &run.id, false, "all done");

        let recovered = try_recover_from_log_at(&mgr, &run.id, tmp_dir.path());
        assert!(recovered.is_some(), "expected recovery to succeed");
        let recovered = recovered.unwrap();
        assert_eq!(recovered.status, AgentRunStatus::Completed);
        assert_eq!(recovered.result_text.as_deref(), Some("all done"));
    }

    #[test]
    fn test_try_recover_from_log_at_error_result() {
        let conn = crate::agent::manager::setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        write_result_log(tmp_dir.path(), &run.id, true, "something went wrong");

        let recovered = try_recover_from_log_at(&mgr, &run.id, tmp_dir.path());
        assert!(
            recovered.is_some(),
            "expected recovery to succeed even for error result"
        );
        let recovered = recovered.unwrap();
        assert_eq!(recovered.status, AgentRunStatus::Failed);
        assert_eq!(
            recovered.result_text.as_deref(),
            Some("something went wrong")
        );
    }

    #[test]
    fn test_try_recover_from_log_at_missing_log() {
        let conn = crate::agent::manager::setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        // No log file written — should return None without touching the DB.
        let result = try_recover_from_log_at(&mgr, &run.id, tmp_dir.path());
        assert!(result.is_none());
        // Run should still be in running state.
        let still_running = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(still_running.status, AgentRunStatus::Running);
    }

    #[test]
    fn test_try_recover_from_log_at_no_result_event() {
        let conn = crate::agent::manager::setup_db();
        let mgr = AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "test", None, None).unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        // Log exists but contains no result event.
        let log_path = tmp_dir.path().join(format!("{}.log", run.id));
        std::fs::write(&log_path, "{\"type\":\"system\",\"subtype\":\"init\"}\n").unwrap();

        let result = try_recover_from_log_at(&mgr, &run.id, tmp_dir.path());
        assert!(result.is_none());
        let still_running = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(still_running.status, AgentRunStatus::Running);
    }
}
