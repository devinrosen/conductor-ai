//! Opt-in trace of slow SQLite statements and transaction hold times.
//!
//! Set `CONDUCTOR_DB_TRACE=<absolute log path>` in every process you want to
//! observe (CLI, TUI, web, hooks). Each process appends tab-separated lines:
//! `timestamp pid process thread kind ms sql`. `CONDUCTOR_DB_TRACE_MS` sets the
//! threshold in milliseconds (default 20). When the variable is unset no
//! profile callback is installed, so there is no overhead.
//!
//! `tx` lines report the time from `BEGIN` to `COMMIT`/`ROLLBACK`. A
//! `BEGIN DEFERRED` transaction only takes the write lock at its first write,
//! so its `tx` lines over-approximate the lock hold; `BEGIN IMMEDIATE` is exact.

use rusqlite::Connection;
use std::cell::RefCell;
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const DEFAULT_THRESHOLD_MS: u64 = 20;
const MAX_SQL_CHARS: usize = 200;

struct TraceConfig {
    path: std::path::PathBuf,
    threshold: Duration,
    pid: u32,
    label: String,
}

static CONFIG: OnceLock<Option<TraceConfig>> = OnceLock::new();

thread_local! {
    /// Start of the open transaction on this thread and its first statement.
    static TX: RefCell<Option<TxState>> = const { RefCell::new(None) };
}

struct TxState {
    started: Instant,
    first_sql: Option<String>,
}

fn config() -> &'static Option<TraceConfig> {
    CONFIG.get_or_init(|| {
        let path = std::env::var_os("CONDUCTOR_DB_TRACE").filter(|v| !v.is_empty())?;
        let ms = std::env::var("CONDUCTOR_DB_TRACE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_THRESHOLD_MS);
        let args: Vec<String> = std::env::args().collect();
        Some(TraceConfig {
            path: path.into(),
            threshold: Duration::from_millis(ms),
            pid: std::process::id(),
            label: process_label(&args),
        })
    })
}

/// Executable basename plus up to two leading non-flag arguments.
fn process_label(args: &[String]) -> String {
    let exe = args
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    let rest = args.iter().skip(1).filter(|a| !a.starts_with('-')).take(2);
    std::iter::once(exe)
        .chain(rest.cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn collapse_sql(sql: &str) -> String {
    let mut out = String::new();
    for (i, word) in sql.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > MAX_SQL_CHARS {
            break;
        }
    }
    out.chars().take(MAX_SQL_CHARS).collect()
}

fn format_line(
    ts: &str,
    pid: u32,
    label: &str,
    thread: &str,
    kind: &str,
    dur: Duration,
    sql: &str,
) -> String {
    format!(
        "{ts}\t{pid}\t{label}\t{thread}\t{kind}\t{:.1}\t{}\n",
        dur.as_secs_f64() * 1000.0,
        collapse_sql(sql)
    )
}

fn keyword(sql: &str) -> &str {
    let t = sql.trim_start();
    let end = t
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(t.len());
    &t[..end]
}

/// Update transaction tracking for `sql` finishing at `now`. Returns the
/// `(hold time, first statement)` of a transaction that just ended.
fn track_tx(
    state: &mut Option<TxState>,
    sql: &str,
    now: Instant,
) -> Option<(Duration, Option<String>)> {
    let kw = keyword(sql);
    if kw.eq_ignore_ascii_case("begin") {
        *state = Some(TxState {
            started: now,
            first_sql: None,
        });
        None
    } else if kw.eq_ignore_ascii_case("commit")
        || kw.eq_ignore_ascii_case("end")
        || kw.eq_ignore_ascii_case("rollback")
    {
        state
            .take()
            .map(|s| (now.duration_since(s.started), s.first_sql))
    } else {
        if let Some(s) = state.as_mut() {
            if s.first_sql.is_none() {
                s.first_sql = Some(collapse_sql(sql));
            }
        }
        None
    }
}

fn append(cfg: &TraceConfig, kind: &str, dur: Duration, sql: &str) {
    let thread = std::thread::current();
    let thread_name = thread
        .name()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:?}", thread.id()));
    let line = format_line(
        &chrono::Utc::now().to_rfc3339(),
        cfg.pid,
        &cfg.label,
        &thread_name,
        kind,
        dur,
        sql,
    );
    // One write_all of one line on an O_APPEND file; errors never reach callers.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn on_profile(sql: &str, dur: Duration) {
    let Some(cfg) = config().as_ref() else {
        return;
    };
    if dur >= cfg.threshold {
        append(cfg, "stmt", dur, sql);
    }
    let ended = TX.with(|tx| {
        // try_borrow_mut: never panic inside a SQLite callback.
        tx.try_borrow_mut()
            .ok()
            .and_then(|mut s| track_tx(&mut s, sql, Instant::now()))
    });
    if let Some((hold, first)) = ended {
        if hold >= cfg.threshold {
            append(cfg, "tx", hold, first.as_deref().unwrap_or(""));
        }
    }
}

/// Install the profile callback on `conn` when `CONDUCTOR_DB_TRACE` is set.
pub(crate) fn install(conn: &mut Connection) {
    if config().is_some() {
        conn.profile(Some(on_profile));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_label_keeps_two_positional_args() {
        let args: Vec<String> = ["/usr/bin/conductor", "session", "--x", "hook", "extra"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(process_label(&args), "conductor session hook");
        assert_eq!(process_label(&[]), "unknown");
    }

    #[test]
    fn format_line_shape() {
        let line = format_line(
            "T",
            7,
            "conductor x",
            "main",
            "stmt",
            Duration::from_millis(25),
            "SELECT\n  1",
        );
        assert_eq!(line, "T\t7\tconductor x\tmain\tstmt\t25.0\tSELECT 1\n");
    }

    #[test]
    fn collapse_sql_truncates() {
        let long = "SELECT ".to_string() + &"a ".repeat(300);
        assert!(collapse_sql(&long).chars().count() <= MAX_SQL_CHARS);
    }

    #[test]
    fn begin_to_commit_reports_hold_and_first_statement() {
        let mut state = None;
        let t0 = Instant::now();
        assert!(track_tx(&mut state, "BEGIN IMMEDIATE", t0).is_none());
        assert!(track_tx(&mut state, "UPDATE t SET a = 1", t0).is_none());
        assert!(track_tx(&mut state, "INSERT INTO t VALUES (1)", t0).is_none());
        let (hold, first) = track_tx(&mut state, "commit", t0 + Duration::from_millis(50)).unwrap();
        assert_eq!(hold, Duration::from_millis(50));
        assert_eq!(first.as_deref(), Some("UPDATE t SET a = 1"));
        assert!(state.is_none());
    }

    #[test]
    fn commit_without_begin_is_ignored() {
        let mut state = None;
        assert!(track_tx(&mut state, "COMMIT", Instant::now()).is_none());
    }
}
