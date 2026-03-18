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
    }
    mgr.get_run(run_id).ok().flatten()
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
    let Ok(contents) = fs::read_to_string(Path::new(path)) else {
        return 0;
    };

    let mut count: i64 = 0;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("assistant") {
            count += 1;
        }
    }
    count
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
