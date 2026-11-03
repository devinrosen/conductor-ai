//! Incremental transcript ingestion for interactive sessions (RFC 021 §9 and §11, #3165).
//!
//! The TUI and web pollers call [`TranscriptIngester::poll`] on every tick, after the
//! spool drain. It never runs in the hook: transcripts reach 10 MB and more.
//!
//! Two jobs:
//!
//! 1. **Metrics** for runs that are `idle`, or `completed`/`failed` within the last 24 hours
//!    (the `failed` arm is the reaper's "once more" pass after a `kill -9`). New complete
//!    lines are read from `agent_runs.transcript_offset`; then, in one IMMEDIATE
//!    transaction, the offset is compare-and-swapped (the claim: a second poller that read
//!    the same range finds it moved and writes nothing), session totals are written, per-turn
//!    usage is merged into the turn's `result` event and `num_turns` is set.
//! 2. **Denial check** for `waiting_for_feedback` runs with a pending permission request: a
//!    rejection or interrupt entry newer than the request moves the run to `idle`. This scan
//!    does *not* advance `transcript_offset` (the metrics pass wants whole turns), so an
//!    in-memory cursor keeps each tick from re-reading the turn in progress.
//!
//! Every failure is logged at debug level and swallowed: the transcript format is
//! undocumented and must never fail a poller tick.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{named_params, params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};

use super::hook::{close_prompt_span, dismiss_pending};
use super::transcript::{self, DenialKind, Parsed, Tokens};
use crate::agent::status::{AgentRunStatus, FeedbackStatus};
use crate::agent::types::{
    EVENT_KIND_PROMPT, EVENT_KIND_RESULT, EVENT_KIND_SYSTEM, INTERACTIVE_RUNTIME, META_KEY_USAGE,
};
use crate::error::Result;
use crate::new_id;

/// Largest chunk read for one run in one pass; also the longest line that is not skipped.
const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Once a tick has read this many bytes, remaining runs wait for the next tick.
const TICK_BYTE_BUDGET: usize = 8 * 1024 * 1024;
/// Smaller budget for the web poller, which holds the shared database mutex while a tick
/// runs: bounds how long other request handlers can be stalled during catch-up.
pub const WEB_TICK_BYTE_BUDGET: usize = 1024 * 1024;
/// Completed or failed runs are read for this long after they end.
const RECENT_END_HOURS: i64 = 24;

const SUMMARY_PERMISSION_DENIED: &str = "Permission denied";
const SUMMARY_INTERRUPTED: &str = "Interrupted";

/// What one [`TranscriptIngester::poll`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestStats {
    /// Runs whose metrics were written.
    pub metrics_applied: usize,
    /// Runs another poller claimed first.
    pub metrics_skipped: usize,
    /// Runs moved to `idle` because a denial or interrupt was found.
    pub denials: usize,
}

/// Reads interactive session transcripts. Hold one per poller thread.
#[derive(Debug)]
pub struct TranscriptIngester {
    /// Bytes read per tick across all runs; also caps one chunk (and so the longest line
    /// that is not skipped).
    tick_budget: usize,
    /// `run_id -> (latest pending request id, offset scanned to)` for the denial check.
    denial_cursors: HashMap<String, (String, u64)>,
}

impl TranscriptIngester {
    pub fn new() -> Self {
        Self::with_tick_budget(TICK_BYTE_BUDGET)
    }

    /// Like [`new`](Self::new) with a custom per-tick byte budget.
    pub fn with_tick_budget(tick_budget: usize) -> Self {
        Self {
            tick_budget,
            denial_cursors: HashMap::new(),
        }
    }

    /// Run the denial check, then the metrics pass. Never fails.
    pub fn poll(&mut self, conn: &Connection, now: DateTime<Utc>) -> IngestStats {
        let mut stats = IngestStats::default();
        let mut budget = self.tick_budget;
        let max_chunk = self.tick_budget.min(MAX_CHUNK_BYTES);
        match self.denial_pass(conn, &mut budget, max_chunk) {
            Ok(n) => stats.denials = n,
            Err(e) => tracing::debug!("transcript denial check failed: {e}"),
        }
        match metrics_pass(conn, now, &mut budget, max_chunk) {
            Ok((applied, skipped)) => {
                stats.metrics_applied = applied;
                stats.metrics_skipped = skipped;
            }
            Err(e) => tracing::debug!("transcript metrics pass failed: {e}"),
        }
        stats
    }

    fn denial_pass(
        &mut self,
        conn: &Connection,
        budget: &mut usize,
        max_chunk: usize,
    ) -> Result<usize> {
        let candidates = denial_candidates(conn)?;
        let live: HashSet<&str> = candidates.iter().map(|c| c.run_id.as_str()).collect();
        self.denial_cursors.retain(|k, _| live.contains(k.as_str()));

        let mut found = 0;
        for c in &candidates {
            if *budget == 0 {
                break;
            }
            match self.check_denial(conn, c, budget, max_chunk) {
                Ok(true) => found += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!(run_id = %c.run_id, "transcript denial check failed: {e}")
                }
            }
        }
        Ok(found)
    }

    fn check_denial(
        &mut self,
        conn: &Connection,
        c: &DenialCandidate,
        budget: &mut usize,
        max_chunk: usize,
    ) -> Result<bool> {
        let Some(created) = transcript::parse_utc(&c.created_at) else {
            tracing::debug!(
                run_id = %c.run_id,
                request_id = %c.request_id,
                created_at = %c.created_at,
                "unparseable feedback request timestamp; denial check skipped"
            );
            return Ok(false);
        };
        let from = match self.denial_cursors.get(&c.run_id) {
            Some((req, pos)) if *req == c.request_id => c.offset.max(*pos),
            _ => c.offset,
        };
        let path = Path::new(&c.path);
        if std::fs::metadata(path).map(|m| m.len()).ok() == Some(from) {
            return Ok(false);
        }
        let chunk = transcript::read_chunk(path, from, max_chunk)?;
        *budget = budget.saturating_sub(chunk.bytes.len());
        let parsed = transcript::parse(&chunk.bytes);
        let Some(denial) = parsed.denials.iter().find(|d| d.timestamp > created) else {
            self.denial_cursors
                .insert(c.run_id.clone(), (c.request_id.clone(), chunk.end));
            return Ok(false);
        };
        let ts = denial.timestamp.to_rfc3339();
        let summary = match denial.kind {
            DenialKind::Rejected => SUMMARY_PERMISSION_DENIED,
            DenialKind::Interrupted => SUMMARY_INTERRUPTED,
        };
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        // The hook (or another poller) may have moved the run on already.
        let moved = tx.execute(
            "UPDATE agent_runs SET status = ?2, idle_since = ?3 WHERE id = ?1 AND status = ?4",
            params![
                c.run_id,
                AgentRunStatus::Idle.as_str(),
                ts,
                AgentRunStatus::WaitingForFeedback.as_str()
            ],
        )?;
        if moved == 0 {
            tx.commit()?;
            self.denial_cursors.remove(&c.run_id);
            return Ok(false);
        }
        dismiss_pending(&tx, &c.run_id, &ts)?;
        close_prompt_span(&tx, &c.run_id, denial.prompt_id.as_deref(), &ts)?;
        tx.execute(
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, ended_at, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                new_id(),
                c.run_id,
                EVENT_KIND_SYSTEM,
                summary,
                ts,
                json!({ "source": "transcript", "prompt_id": denial.prompt_id }).to_string()
            ],
        )?;
        tx.commit()?;
        self.denial_cursors.remove(&c.run_id);
        Ok(true)
    }
}

impl Default for TranscriptIngester {
    fn default() -> Self {
        Self::new()
    }
}

struct DenialCandidate {
    run_id: String,
    path: String,
    offset: u64,
    request_id: String,
    created_at: String,
}

/// The latest pending request per `waiting_for_feedback` interactive run. The latest, not any:
/// a denial that answered an earlier request must not dismiss a newer, live prompt.
fn denial_candidates(conn: &Connection) -> Result<Vec<DenialCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT ar.id, ar.transcript_path, COALESCE(ar.transcript_offset, 0), fr.id, fr.created_at \
         FROM agent_runs ar JOIN feedback_requests fr ON fr.run_id = ar.id AND fr.status = ?3 \
         WHERE ar.runtime = ?1 AND ar.status = ?2 AND ar.transcript_path IS NOT NULL \
         ORDER BY fr.created_at, fr.id",
    )?;
    let rows = stmt.query_map(
        params![
            INTERACTIVE_RUNTIME,
            AgentRunStatus::WaitingForFeedback.as_str(),
            FeedbackStatus::Pending.as_str()
        ],
        |r| {
            Ok(DenialCandidate {
                run_id: r.get(0)?,
                path: r.get(1)?,
                offset: r.get::<_, i64>(2)?.max(0) as u64,
                request_id: r.get(3)?,
                created_at: r.get(4)?,
            })
        },
    )?;
    let mut latest: HashMap<String, DenialCandidate> = HashMap::new();
    for row in rows {
        let c = row?;
        // Ordered oldest first, so a later row replaces an earlier one.
        latest.insert(c.run_id.clone(), c);
    }
    Ok(latest.into_values().collect())
}

struct MetricsCandidate {
    run_id: String,
    path: String,
    offset: u64,
}

fn metrics_candidates(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<MetricsCandidate>> {
    let cutoff = now - Duration::hours(RECENT_END_HOURS);
    let mut stmt = conn.prepare(
        "SELECT id, transcript_path, COALESCE(transcript_offset, 0), status, ended_at \
         FROM agent_runs WHERE runtime = ?1 AND transcript_path IS NOT NULL \
         AND status IN ('idle', 'completed', 'failed') ORDER BY started_at, id",
    )?;
    let rows = stmt.query_map(params![INTERACTIVE_RUNTIME], |r| {
        Ok((
            MetricsCandidate {
                run_id: r.get(0)?,
                path: r.get(1)?,
                offset: r.get::<_, i64>(2)?.max(0) as u64,
            },
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (c, status, ended_at) = row?;
        if status != AgentRunStatus::Idle.as_str() {
            // Compare parsed times: stored formats differ (`+00:00` vs `Z`).
            let recent = ended_at
                .as_deref()
                .and_then(transcript::parse_utc)
                .map(|t| t >= cutoff)
                .unwrap_or(true);
            if !recent {
                continue;
            }
        }
        // One stat each: skip runs that are up to date.
        match std::fs::metadata(&c.path) {
            Ok(m) if m.len() != c.offset => out.push(c),
            Ok(_) => {}
            Err(e) => tracing::debug!(run_id = %c.run_id, "transcript not readable: {e}"),
        }
    }
    Ok(out)
}

fn metrics_pass(
    conn: &Connection,
    now: DateTime<Utc>,
    budget: &mut usize,
    max_chunk: usize,
) -> Result<(usize, usize)> {
    let (mut applied, mut skipped) = (0, 0);
    for c in metrics_candidates(conn, now)? {
        if *budget == 0 {
            break;
        }
        let result = (|| -> Result<Option<bool>> {
            let chunk = transcript::read_chunk(Path::new(&c.path), c.offset, max_chunk)?;
            *budget = budget.saturating_sub(chunk.bytes.len());
            if chunk.end == c.offset {
                return Ok(None); // nothing complete yet
            }
            let parsed = transcript::parse(&chunk.bytes);
            if parsed.skipped > 0 {
                tracing::debug!(run_id = %c.run_id, skipped = parsed.skipped, "transcript lines skipped");
            }
            apply_metrics(conn, &c.run_id, c.offset, chunk.end, &parsed).map(Some)
        })();
        match result {
            Ok(Some(true)) => applied += 1,
            Ok(Some(false)) => skipped += 1,
            Ok(None) => {}
            Err(e) => tracing::debug!(run_id = %c.run_id, "transcript metrics failed: {e}"),
        }
    }
    Ok((applied, skipped))
}

/// Where a turn's usage is merged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TurnKey {
    /// The `prompt_id` carried by the turn's `prompt` and `result` events.
    Prompt(String),
    /// A prompt event without a `prompt_id`, addressed by its row id.
    Event(String),
}

#[derive(Default)]
struct TurnUsage {
    tokens: Tokens,
    messages: i64,
    model: Option<String>,
}

/// Write one pass's results in one IMMEDIATE transaction. Returns `false` when another
/// poller already advanced the offset past `expected_offset` (nothing is written then).
fn apply_metrics(
    conn: &Connection,
    run_id: &str,
    expected_offset: u64,
    new_offset: u64,
    parsed: &Parsed,
) -> Result<bool> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;

    // Claim first: the loser of a race writes nothing.
    let claimed = tx.execute(
        "UPDATE agent_runs SET transcript_offset = :end \
         WHERE id = :id AND COALESCE(transcript_offset, 0) = :start",
        named_params! { ":end": new_offset as i64, ":id": run_id, ":start": expected_offset as i64 },
    )?;
    if claimed == 0 {
        return Ok(false);
    }

    // Session totals: the latest cost-state is exact and cumulative. Messages after it (or all
    // of them when there is none) are added on top and marked estimated. `cost_usd` is left
    // alone then: transcripts no longer carry per-entry cost and we don't price tokens.
    let latest = parsed.latest_cost_state();
    if let Some((_, cs)) = latest {
        tx.execute(
            "UPDATE agent_runs SET cost_usd = COALESCE(:cost, cost_usd), \
             input_tokens = :i, output_tokens = :o, cache_read_input_tokens = :cr, \
             cache_creation_input_tokens = :cc, duration_ms = COALESCE(:dur, duration_ms), \
             usage_estimated = 0 WHERE id = :id",
            named_params! {
                ":cost": cs.cost_usd, ":i": cs.tokens.input, ":o": cs.tokens.output,
                ":cr": cs.tokens.cache_read, ":cc": cs.tokens.cache_creation,
                ":dur": cs.api_duration_ms, ":id": run_id,
            },
        )?;
    }
    let after = latest.map_or(0, |(line, _)| *line);
    let mut extra = Tokens::default();
    let mut any_after = false;
    for m in parsed.messages() {
        if latest.is_none() || m.line_index > after {
            any_after = true;
            extra.add(&m.tokens);
        }
    }
    if any_after {
        tx.execute(
            "UPDATE agent_runs SET input_tokens = COALESCE(input_tokens, 0) + :i, \
             output_tokens = COALESCE(output_tokens, 0) + :o, \
             cache_read_input_tokens = COALESCE(cache_read_input_tokens, 0) + :cr, \
             cache_creation_input_tokens = COALESCE(cache_creation_input_tokens, 0) + :cc, \
             usage_estimated = 1 WHERE id = :id",
            named_params! {
                ":i": extra.input, ":o": extra.output, ":cr": extra.cache_read,
                ":cc": extra.cache_creation, ":id": run_id,
            },
        )?;
    }

    merge_turn_usage(&tx, run_id, parsed)?;

    tx.execute(
        "UPDATE agent_runs SET num_turns = \
         (SELECT COUNT(*) FROM agent_run_events WHERE run_id = ?1 AND kind = ?2) WHERE id = ?1",
        params![run_id, EVENT_KIND_PROMPT],
    )?;
    tx.commit()?;
    Ok(true)
}

/// A run's `prompt` and `result` events, loaded once per pass.
#[derive(Default)]
struct RunEvents {
    /// `prompt` events with a usable `started_at`, ascending, for time-based attribution.
    timed_prompts: Vec<TimedPrompt>,
    /// Latest `result` event id per `prompt_id`.
    result_by_prompt: HashMap<String, String>,
    /// Latest `prompt` event id per `prompt_id`.
    prompt_by_prompt: HashMap<String, String>,
}

struct TimedPrompt {
    id: String,
    prompt_id: Option<String>,
    started_at: DateTime<Utc>,
}

fn load_run_events(conn: &Connection, run_id: &str) -> Result<RunEvents> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, json_extract(metadata, '$.prompt_id'), started_at \
         FROM agent_run_events WHERE run_id = ?1 AND kind IN (?2, ?3) ORDER BY started_at, id",
    )?;
    let rows = stmt.query_map(params![run_id, EVENT_KIND_PROMPT, EVENT_KIND_RESULT], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut out = RunEvents::default();
    for row in rows {
        let (id, kind, prompt_id, started_at) = row?;
        let is_prompt = kind == EVENT_KIND_PROMPT;
        if let Some(pid) = &prompt_id {
            // Ascending order: a later event for the same prompt replaces an earlier one.
            let map = if is_prompt {
                &mut out.prompt_by_prompt
            } else {
                &mut out.result_by_prompt
            };
            map.insert(pid.clone(), id.clone());
        }
        if is_prompt {
            match started_at.as_deref().and_then(transcript::parse_utc) {
                Some(started_at) => out.timed_prompts.push(TimedPrompt {
                    id,
                    prompt_id,
                    started_at,
                }),
                None => tracing::debug!(
                    run_id,
                    event_id = %id,
                    started_at = ?started_at,
                    "prompt event has no usable started_at; skipped for turn attribution"
                ),
            }
        }
    }
    // Sorted by the stored text above; times in mixed formats need a real ordering.
    out.timed_prompts.sort_by_key(|p| p.started_at);
    Ok(out)
}

/// Group the chunk's messages by turn and merge each group into that turn's event metadata.
fn merge_turn_usage(conn: &Connection, run_id: &str, parsed: &Parsed) -> Result<()> {
    if parsed.messages().next().is_none() {
        return Ok(());
    }
    let events = load_run_events(conn, run_id)?;
    let mut turns: HashMap<TurnKey, TurnUsage> = HashMap::new();
    for m in parsed.messages() {
        let key = match parsed.prompt_id_for(m) {
            Some(p) => Some(TurnKey::Prompt(p)),
            // The chain left this chunk: attribute by time to the latest prompt at or before it.
            None => m.timestamp.and_then(|ts| {
                let n = events.timed_prompts.partition_point(|p| p.started_at <= ts);
                n.checked_sub(1).map(|i| {
                    let p = &events.timed_prompts[i];
                    match &p.prompt_id {
                        Some(id) => TurnKey::Prompt(id.clone()),
                        None => TurnKey::Event(p.id.clone()),
                    }
                })
            }),
        };
        let Some(key) = key else {
            tracing::debug!(run_id, "transcript message not attributable to a turn");
            continue;
        };
        let t = turns.entry(key).or_default();
        t.tokens.add(&m.tokens);
        t.messages += 1;
        if m.model.is_some() {
            t.model = m.model.clone();
        }
    }
    for (key, usage) in turns {
        let target: Option<&String> = match &key {
            TurnKey::Event(id) => Some(id),
            // The turn's result event; a crash mid-turn or a lagging Stop leaves only the
            // prompt event.
            TurnKey::Prompt(pid) => events
                .result_by_prompt
                .get(pid)
                .or_else(|| events.prompt_by_prompt.get(pid)),
        };
        let Some(event_id) = target else {
            tracing::debug!(run_id, ?key, "no event for transcript turn usage");
            continue;
        };
        merge_usage_into_event(conn, event_id, &usage)?;
    }
    Ok(())
}

fn merge_usage_into_event(conn: &Connection, event_id: &str, add: &TurnUsage) -> Result<()> {
    let existing: Option<String> = conn
        .prepare_cached("SELECT metadata FROM agent_run_events WHERE id = ?1")?
        .query_row([event_id], |r| r.get(0))
        .optional()?
        .flatten();
    let mut meta: Map<String, Value> = match existing.as_deref().map(serde_json::from_str) {
        None => Map::new(),
        Some(Ok(Value::Object(o))) => o,
        Some(other) => {
            // Overwriting would destroy the event's other keys (`prompt_id`, `source`).
            tracing::debug!(
                event_id,
                parse_error = ?other.err(),
                "event metadata is not a JSON object; usage not merged"
            );
            return Ok(());
        }
    };
    let mut usage: Map<String, Value> = match meta.remove(META_KEY_USAGE) {
        Some(Value::Object(o)) => o,
        _ => Map::new(),
    };
    let mut bump = |key: &str, by: i64| {
        let cur = usage.get(key).and_then(Value::as_i64).unwrap_or(0);
        usage.insert(key.to_string(), json!(cur + by));
    };
    bump("input_tokens", add.tokens.input);
    bump("output_tokens", add.tokens.output);
    bump("cache_read_input_tokens", add.tokens.cache_read);
    bump("cache_creation_input_tokens", add.tokens.cache_creation);
    bump("messages", add.messages);
    if let Some(model) = &add.model {
        usage.insert("model".into(), json!(model));
    }
    // Per-message figures leave out Claude Code's side calls: approximate (RFC 021 §9).
    usage.insert("approximate".into(), json!(true));
    meta.insert(META_KEY_USAGE.into(), Value::Object(usage));
    conn.prepare_cached("UPDATE agent_run_events SET metadata = ?2 WHERE id = ?1")?
        .execute(params![event_id, Value::Object(meta).to_string()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::transcript::FIXTURE;
    use crate::test_helpers::setup_db;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-03T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn add_run(conn: &Connection, id: &str, status: &str, path: &Path) {
        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, repo_id, session_id, prompt, status, \
             started_at, runtime, transcript_path) \
             VALUES (?1, 'w1', 'r1', ?1, 'p', ?2, '2026-11-03T16:59:00+00:00', 'interactive', ?3)",
            params![id, status, path.to_str().unwrap()],
        )
        .unwrap();
    }

    fn add_event(conn: &Connection, run: &str, kind: &str, at: &str, pid: &str) {
        conn.execute(
            "INSERT INTO agent_run_events (id, run_id, kind, summary, started_at, metadata) \
             VALUES (?1, ?2, ?3, '', ?4, ?5)",
            params![
                new_id(),
                run,
                kind,
                at,
                json!({ "prompt_id": pid }).to_string()
            ],
        )
        .unwrap();
    }

    /// Prompt P1 with a result event, prompt P2 without one (a turn still open).
    fn seed_turns(conn: &Connection, run: &str) {
        add_event(conn, run, "prompt", "2026-11-03T17:00:00+00:00", "P1");
        add_event(conn, run, "result", "2026-11-03T17:03:00+00:00", "P1");
        add_event(conn, run, "prompt", "2026-11-03T17:05:00+00:00", "P2");
    }

    fn setup(status: &str, data: &str) -> (Connection, tempfile::TempDir, std::path::PathBuf) {
        let conn = setup_db();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, data).unwrap();
        add_run(&conn, "r-1", status, &path);
        seed_turns(&conn, "r-1");
        (conn, dir, path)
    }

    fn col_i(conn: &Connection, col: &str) -> Option<i64> {
        conn.query_row(
            &format!("SELECT {col} FROM agent_runs WHERE id = 'r-1'"),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn usage_of(conn: &Connection, kind: &str, pid: &str) -> Option<Value> {
        let m: String = conn
            .query_row(
                "SELECT metadata FROM agent_run_events WHERE run_id = 'r-1' AND kind = ?1 \
                 AND json_extract(metadata, '$.prompt_id') = ?2",
                params![kind, pid],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str::<Value>(&m)
            .unwrap()
            .get("usage")
            .cloned()
    }

    fn status(conn: &Connection) -> String {
        conn.query_row("SELECT status FROM agent_runs WHERE id = 'r-1'", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    fn fixture_lines() -> Vec<&'static str> {
        FIXTURE.lines().collect()
    }

    #[test]
    fn fixture_totals_and_per_turn_usage() {
        let (conn, _d, path) = setup("idle", FIXTURE);
        let stats = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(stats.metrics_applied, 1);
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
        assert_eq!(col_i(&conn, "output_tokens"), Some(330));
        assert_eq!(col_i(&conn, "cache_read_input_tokens"), Some(9000));
        assert_eq!(col_i(&conn, "cache_creation_input_tokens"), Some(600));
        assert_eq!(col_i(&conn, "duration_ms"), Some(41000));
        assert_eq!(col_i(&conn, "usage_estimated"), Some(0));
        assert_eq!(col_i(&conn, "num_turns"), Some(2));
        assert_eq!(
            col_i(&conn, "transcript_offset"),
            Some(std::fs::metadata(&path).unwrap().len() as i64)
        );
        let cost: f64 = conn
            .query_row("SELECT cost_usd FROM agent_runs WHERE id='r-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cost, 0.5);

        // P1: result event, 3 messages after dedup.
        let u1 = usage_of(&conn, "result", "P1").unwrap();
        assert_eq!(u1["input_tokens"], 12);
        assert_eq!(u1["output_tokens"], 36);
        assert_eq!(u1["cache_read_input_tokens"], 600);
        assert_eq!(u1["cache_creation_input_tokens"], 50);
        assert_eq!(u1["messages"], 3);
        assert_eq!(u1["approximate"], true);
        assert_eq!(u1["model"], "claude-opus-5-5");
        // P2 has no result event yet: lands on the prompt event.
        let u2 = usage_of(&conn, "prompt", "P2").unwrap();
        assert_eq!(u2["input_tokens"], 6);
        assert_eq!(u2["output_tokens"], 55);
        assert_eq!(u2["messages"], 5);
        assert!(usage_of(&conn, "prompt", "P1").is_none());

        // Nothing new: the next poll does no work.
        let again = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(again, IngestStats::default());
        assert_eq!(usage_of(&conn, "result", "P1").unwrap()["messages"], 3);
    }

    #[test]
    fn incremental_read_leaves_partial_line_and_does_not_double_count() {
        let lines = fixture_lines();
        // Through the first cost-state, then half of the next line.
        let cut = lines
            .iter()
            .position(|l| l.contains("\"cost-state\""))
            .unwrap()
            + 1;
        let first: String = lines[..cut].iter().map(|l| format!("{l}\n")).collect();
        let partial = &lines[cut][..10];
        let (conn, _d, path) = setup("idle", &format!("{first}{partial}"));
        let mut ing = TranscriptIngester::new();
        ing.poll(&conn, now());
        assert_eq!(col_i(&conn, "transcript_offset"), Some(first.len() as i64));
        assert_eq!(col_i(&conn, "input_tokens"), Some(1000));
        assert_eq!(usage_of(&conn, "result", "P1").unwrap()["messages"], 3);

        // Append the rest.
        let rest: String = lines[cut..].iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, format!("{first}{rest}")).unwrap();
        ing.poll(&conn, now());
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
        assert_eq!(col_i(&conn, "usage_estimated"), Some(0));
        // P1 is unchanged (no double count); P2 appears.
        assert_eq!(usage_of(&conn, "result", "P1").unwrap()["messages"], 3);
        assert_eq!(usage_of(&conn, "prompt", "P2").unwrap()["messages"], 5);
        assert_eq!(col_i(&conn, "num_turns"), Some(2));
    }

    #[test]
    fn no_cost_state_falls_back_to_estimates_then_resets() {
        let lines = fixture_lines();
        let without: String = lines
            .iter()
            .filter(|l| !l.contains("\"cost-state\""))
            .map(|l| format!("{l}\n"))
            .collect();
        let (conn, _d, path) = setup("idle", &without);
        let mut ing = TranscriptIngester::new();
        ing.poll(&conn, now());
        // Sums of the 8 deduplicated messages.
        assert_eq!(col_i(&conn, "input_tokens"), Some(18));
        assert_eq!(col_i(&conn, "output_tokens"), Some(91));
        assert_eq!(col_i(&conn, "cache_read_input_tokens"), Some(3010));
        assert_eq!(col_i(&conn, "cache_creation_input_tokens"), Some(60));
        assert_eq!(col_i(&conn, "usage_estimated"), Some(1));
        let cost: Option<f64> = conn
            .query_row("SELECT cost_usd FROM agent_runs WHERE id='r-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cost, None);

        // The exact cost-state arrives later and replaces the estimate.
        let cs = lines
            .iter()
            .rev()
            .find(|l| l.contains("\"cost-state\""))
            .unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut f, format!("{cs}\n").as_bytes()).unwrap();
        ing.poll(&conn, now());
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
        assert_eq!(col_i(&conn, "usage_estimated"), Some(0));
    }

    #[test]
    fn garbage_lines_do_not_abort() {
        let mut data = String::from("garbage\n{\"type\":\"who-knows\"}\n[]\n");
        data.push_str(FIXTURE);
        let (conn, _d, _p) = setup("idle", &data);
        let stats = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(stats.metrics_applied, 1);
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
    }

    #[test]
    fn two_pollers_do_not_double_apply() {
        let (conn, _d, path) = setup("idle", FIXTURE);
        let chunk = transcript::read_chunk(&path, 0, MAX_CHUNK_BYTES).unwrap();
        let parsed = transcript::parse(&chunk.bytes);
        assert!(apply_metrics(&conn, "r-1", 0, chunk.end, &parsed).unwrap());
        assert!(!apply_metrics(&conn, "r-1", 0, chunk.end, &parsed).unwrap());
        assert_eq!(usage_of(&conn, "result", "P1").unwrap()["messages"], 3);
    }

    #[test]
    fn two_file_db_connections_claim_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        let a = crate::db::open_database(&db).unwrap();
        crate::test_helpers::insert_test_repo(&a, "r1", "test-repo", "/tmp/repo");
        crate::test_helpers::insert_test_worktree(&a, "w1", "r1", "feat-test", "/tmp/ws/feat-test");
        let t = dir.path().join("t.jsonl");
        std::fs::write(&t, FIXTURE).unwrap();
        add_run(&a, "r-1", "idle", &t);
        seed_turns(&a, "r-1");
        let b = crate::db::open_database(&db).unwrap();
        let chunk = transcript::read_chunk(&t, 0, MAX_CHUNK_BYTES).unwrap();
        let parsed = transcript::parse(&chunk.bytes);
        assert!(apply_metrics(&a, "r-1", 0, chunk.end, &parsed).unwrap());
        assert!(!apply_metrics(&b, "r-1", 0, chunk.end, &parsed).unwrap());
        assert_eq!(usage_of(&b, "result", "P1").unwrap()["messages"], 3);
    }

    #[test]
    fn oversize_line_is_skipped_and_following_lines_read() {
        let (conn, _d, path) = setup("idle", "");
        let mut data = vec![b'x'; MAX_CHUNK_BYTES + 10];
        data.push(b'\n');
        data.extend_from_slice(FIXTURE.as_bytes());
        std::fs::write(&path, &data).unwrap();
        let mut ing = TranscriptIngester::new();
        ing.poll(&conn, now());
        assert_eq!(
            col_i(&conn, "transcript_offset"),
            Some(MAX_CHUNK_BYTES as i64 + 11)
        );
        ing.poll(&conn, now());
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
    }

    #[test]
    fn truncated_file_restarts_at_zero() {
        let (conn, _d, path) = setup("idle", FIXTURE);
        conn.execute("UPDATE agent_runs SET transcript_offset = 10000000", [])
            .unwrap();
        TranscriptIngester::new().poll(&conn, now());
        assert_eq!(
            col_i(&conn, "transcript_offset"),
            Some(std::fs::metadata(&path).unwrap().len() as i64)
        );
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
    }

    #[test]
    fn candidates_skip_old_ended_running_and_current_runs() {
        let (conn, _d, path) = setup("completed", FIXTURE);
        // Ended 3 days ago: not read.
        conn.execute(
            "UPDATE agent_runs SET ended_at = '2026-10-31T10:00:00+00:00'",
            [],
        )
        .unwrap();
        assert!(metrics_candidates(&conn, now()).unwrap().is_empty());
        // Ended an hour ago, with a `Z` suffix: read.
        conn.execute(
            "UPDATE agent_runs SET ended_at = '2026-11-03T17:00:00Z'",
            [],
        )
        .unwrap();
        assert_eq!(metrics_candidates(&conn, now()).unwrap().len(), 1);
        // Running: not read.
        conn.execute("UPDATE agent_runs SET status = 'running'", [])
            .unwrap();
        assert!(metrics_candidates(&conn, now()).unwrap().is_empty());
        // Idle and up to date: not read.
        conn.execute(
            "UPDATE agent_runs SET status = 'idle', transcript_offset = ?1",
            [std::fs::metadata(&path).unwrap().len() as i64],
        )
        .unwrap();
        assert!(metrics_candidates(&conn, now()).unwrap().is_empty());
    }

    fn waiting(created_at: &str) -> (Connection, tempfile::TempDir, std::path::PathBuf) {
        let (conn, d, p) = setup("waiting_for_feedback", FIXTURE);
        conn.execute(
            "INSERT INTO feedback_requests (id, run_id, prompt, status, created_at, feedback_type) \
             VALUES ('f1', 'r-1', 'Permission: Bash', 'pending', ?1, 'text')",
            [created_at],
        )
        .unwrap();
        (conn, d, p)
    }

    fn fb_status(conn: &Connection) -> String {
        conn.query_row(
            "SELECT status FROM feedback_requests WHERE id='f1'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn denial_after_pending_request_dismisses_and_idles() {
        let (conn, _d, _p) = waiting("2026-11-03T17:05:10+00:00");
        let stats = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(stats.denials, 1);
        assert_eq!(status(&conn), "idle");
        assert_eq!(fb_status(&conn), "dismissed");
        let since: String = conn
            .query_row(
                "SELECT idle_since FROM agent_runs WHERE id='r-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            DateTime::parse_from_rfc3339(&since).unwrap(),
            DateTime::parse_from_rfc3339("2026-11-03T17:05:20Z").unwrap()
        );
        let (summary, started): (String, String) = conn
            .query_row(
                "SELECT summary, started_at FROM agent_run_events \
                 WHERE run_id='r-1' AND kind='system'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(summary, "Permission denied");
        assert_eq!(started, since);
        // The open prompt span is closed.
        let open: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_run_events WHERE kind='prompt' AND ended_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 1); // P1's span was never closed by the seed; P2's was.
    }

    #[test]
    fn denial_does_not_advance_offset_and_metrics_follow_on_idle() {
        let (conn, _d, _p) = waiting("2026-11-03T17:05:10+00:00");
        let mut ing = TranscriptIngester::new();
        // The same poll finishes the denial, then reads metrics now that the run is idle.
        let stats = ing.poll(&conn, now());
        assert_eq!((stats.denials, stats.metrics_applied), (1, 1));
        assert_eq!(col_i(&conn, "input_tokens"), Some(2252));
    }

    #[test]
    fn rejection_before_request_is_ignored() {
        let (conn, _d, _p) = waiting("2026-11-03T17:06:00+00:00");
        let stats = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(stats.denials, 0);
        assert_eq!(status(&conn), "waiting_for_feedback");
        assert_eq!(fb_status(&conn), "pending");
        assert_eq!(col_i(&conn, "transcript_offset"), None);
    }

    #[test]
    fn interrupt_only_reports_interrupted() {
        // The rejection is older than the request; only the interrupt is newer.
        let (conn, _d, _p) = waiting("2026-11-03T17:05:20.500+00:00");
        let stats = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(stats.denials, 1);
        let summary: String = conn
            .query_row(
                "SELECT summary FROM agent_run_events WHERE run_id='r-1' AND kind='system'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(summary, "Interrupted");
    }

    #[test]
    fn denial_cursor_avoids_rereading_and_finds_later_denials() {
        let lines = fixture_lines();
        let rej = lines
            .iter()
            .position(|l| l.contains("User rejected tool use"))
            .unwrap();
        let before: String = lines[..rej].iter().map(|l| format!("{l}\n")).collect();
        let (conn, _d, path) = waiting("2026-11-03T17:05:10+00:00");
        std::fs::write(&path, &before).unwrap();
        let mut ing = TranscriptIngester::new();
        assert_eq!(ing.poll(&conn, now()).denials, 0);
        assert_eq!(ing.denial_cursors["r-1"].1, before.len() as u64);
        assert_eq!(col_i(&conn, "transcript_offset"), None);

        std::fs::write(&path, FIXTURE).unwrap();
        assert_eq!(ing.poll(&conn, now()).denials, 1);
        assert!(ing.denial_cursors.is_empty());
    }

    #[test]
    fn failures_are_swallowed() {
        let (conn, _d, path) = setup("idle", FIXTURE);
        std::fs::remove_file(&path).unwrap();
        let stats = TranscriptIngester::new().poll(&conn, now());
        assert_eq!(stats, IngestStats::default());
    }
}
