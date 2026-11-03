//! `conductor session hook`: record an interactive Claude Code session's state
//! from a hook payload on stdin (RFC 021 §2).
//!
//! This runs inside the developer's Claude Code session on every hook, so it
//! must be fast and must never get in the way: it reads stdin up to a cap,
//! exits early before touching the database whenever the event is irrelevant,
//! waits at most [`BUSY_TIMEOUT`] for a lock, and always exits 0 with nothing
//! on stdout. Errors reach stderr only when `CONDUCTOR_SESSION_HOOK_DEBUG` is set.

use std::io::{self, Read};
use std::time::{Duration, Instant};

use conductor_core::session::hook::{
    apply_hook_event, handled_event_names, is_handled_event, HookEnv, HookEvent,
};

/// Maximum stdin bytes parsed; `tool_response` can be far larger.
const STDIN_CAP: u64 = 1024 * 1024;

/// How long a hook waits for a database lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_millis(150);

/// String fields recovered from the prefix of a payload truncated at the cap.
/// Claude Code serializes these before `tool_response`.
const SALVAGE_KEYS: &[&str] = &[
    "hook_event_name",
    "session_id",
    "cwd",
    "transcript_path",
    "tool_name",
    "prompt_id",
];

/// Entry point from `main`: run the hook and exit 0, whatever happens.
pub fn run_hook_and_exit() -> ! {
    let debug = std::env::var_os("CONDUCTOR_SESSION_HOOK_DEBUG").is_some();
    if !debug {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let _ = std::panic::catch_unwind(|| handle_session_hook(debug));
    std::process::exit(0)
}

fn handle_session_hook(debug: bool) {
    let started = Instant::now();
    let report = |msg: &str| {
        if debug {
            eprintln!(
                "conductor session hook: {msg} ({}µs)",
                started.elapsed().as_micros()
            );
        }
    };

    let env = capture_env();
    // Always consume stdin, even when we will not use it, so Claude Code never sees EPIPE.
    let (input, truncated) = match read_capped(io::stdin().lock(), STDIN_CAP) {
        Ok(r) => r,
        Err(e) => return report(&format!("reading stdin failed: {e}")),
    };

    if !env.attended {
        return report("not an attended session");
    }
    if !mentions_handled_event(&input) {
        return report("unhandled event");
    }
    if !conductor_core::config::db_path().exists() {
        return report("no conductor database");
    }

    let value = match serde_json::from_slice::<serde_json::Value>(&input) {
        Ok(v) => v,
        Err(e) if truncated => match salvage_prefix(&input) {
            Some(v) => v,
            None => return report(&format!("unparseable truncated payload: {e}")),
        },
        Err(e) => return report(&format!("malformed payload: {e}")),
    };

    let ev = HookEvent::from_payload(&value, env, chrono::Utc::now().to_rfc3339());
    if !ev.hook_event_name.as_deref().is_some_and(is_handled_event) {
        return report("unhandled event");
    }

    let result = conductor_core::db::open_database_compat_with_busy_timeout(
        &conductor_core::config::db_path(),
        BUSY_TIMEOUT,
    )
    .and_then(|conn| apply_hook_event(&conn, &ev));
    match result {
        Ok(outcome) => report(&format!("{outcome:?}")),
        Err(e) => {
            report(&format!("write failed: {e}"));
            on_write_error(&ev, &e);
        }
    }
}

/// Seam for the #3160 spool: called when the event could not be written
/// (typically `SQLITE_BUSY`). Until the spool lands the event is dropped.
fn on_write_error(_ev: &HookEvent, _err: &conductor_core::error::ConductorError) {
    // #3160: spool on error
}

fn capture_env() -> HookEnv {
    HookEnv {
        attended: std::env::var("CLAUDE_CODE_SESSION_ATTENDED").is_ok_and(|v| v == "1"),
        claude_pid: std::env::var("CLAUDE_PID")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|p| *p > 0)
            .or_else(parent_pid),
        tmux_pane: std::env::var("TMUX_PANE").ok().filter(|v| !v.is_empty()),
    }
}

#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    // SAFETY: getppid has no preconditions and cannot fail.
    let pid = unsafe { libc::getppid() };
    u32::try_from(pid).ok().filter(|p| *p > 0)
}

#[cfg(not(unix))]
fn parent_pid() -> Option<u32> {
    None
}

/// Read up to `cap` bytes, then drain and discard the rest. Returns the bytes
/// read and whether anything beyond the cap was discarded.
fn read_capped<R: Read>(mut reader: R, cap: u64) -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    (&mut reader).take(cap).read_to_end(&mut buf)?;
    let drained = io::copy(&mut reader, &mut io::sink())?;
    Ok((buf, drained > 0))
}

/// Cheap pre-parse filter: does the payload name any event we handle?
fn mentions_handled_event(input: &[u8]) -> bool {
    handled_event_names().any(|name| input.windows(name.len()).any(|w| w == name.as_bytes()))
}

/// Recover top-level string fields from the prefix of a truncated JSON payload.
/// Returns `None` unless `hook_event_name` and `session_id` were both found.
fn salvage_prefix(input: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(input);
    let mut map = serde_json::Map::new();
    for key in SALVAGE_KEYS {
        if let Some(val) = extract_string_field(&text, key) {
            map.insert((*key).to_string(), serde_json::Value::String(val));
        }
    }
    (map.contains_key("hook_event_name") && map.contains_key("session_id"))
        .then_some(serde_json::Value::Object(map))
}

/// Find `"key" : "value"` and return the unescaped value. Tolerant, not a parser.
fn extract_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(pos) = text[from..].find(&needle) {
        let after = from + pos + needle.len();
        let rest = text[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            if rest.starts_with('"') {
                let mut escaped = false;
                for (i, c) in rest.char_indices().skip(1) {
                    match c {
                        _ if escaped => escaped = false,
                        '\\' => escaped = true,
                        '"' => return serde_json::from_str::<String>(&rest[..=i]).ok(),
                        _ => {}
                    }
                }
                return None; // value itself was cut off
            }
        }
        from = after;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_capped_truncates_and_drains() {
        let data = [b'a'; 100];
        let (buf, truncated) = read_capped(&data[..], 10).unwrap();
        assert_eq!(buf.len(), 10);
        assert!(truncated);
        let (buf, truncated) = read_capped(&data[..], 100).unwrap();
        assert_eq!(buf.len(), 100);
        assert!(!truncated);
    }

    #[test]
    fn salvage_recovers_leading_fields() {
        let input = br#"{"session_id":"s\"1","hook_event_name":"PostToolUse","cwd":"/a/b","tool_name":"Bash","tool_response":{"stdout":"xxxxxxxx"#;
        let v = salvage_prefix(input).unwrap();
        assert_eq!(v["session_id"], "s\"1");
        assert_eq!(v["hook_event_name"], "PostToolUse");
        assert_eq!(v["cwd"], "/a/b");
        assert_eq!(v["tool_name"], "Bash");
    }

    #[test]
    fn salvage_requires_event_and_session() {
        assert!(salvage_prefix(br#"{"cwd":"/a"}"#).is_none());
        // A value cut off mid-string is not used.
        assert!(salvage_prefix(br#"{"hook_event_name":"Stop","session_id":"abc"#).is_none());
    }

    #[test]
    fn handled_event_prefilter() {
        assert!(mentions_handled_event(br#"{"hook_event_name":"Stop"}"#));
        assert!(!mentions_handled_event(
            br#"{"hook_event_name":"PreToolUse"}"#
        ));
    }
}
