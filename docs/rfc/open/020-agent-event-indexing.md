# RFC 020: Agent Event Indexing

**Status:** Draft
**Date:** 2026-04-21
**Author:** Devin

---

## Problem

Agent logs are written as NDJSON flat files at `~/.conductor/agent-logs/{run_id}.log`. This works well for a single run — tail the file, grep for an error, done. At scale it breaks down:

1. **No cross-run queries.** "Which runs used tool X?" or "Which agents hit this error?" requires grepping N files. There is no way to answer these questions from the conductor CLI, TUI, or MCP tools without reading every log.

2. **Retention is all-or-nothing.** Pruning old runs requires deleting log files manually. There is no way to expire logs by age, repo, or ticket without a custom script.

3. **Structured data stored as text.** Claude emits well-structured stream-json events. Storing them as raw NDJSON discards the structure. Queries against log files are line-oriented text searches, not typed lookups.

4. **MCP `get_step_log` scales poorly.** The tool reads an entire log file to return its contents. For long-running agents with thousands of events, this floods context with noise. There is no way to filter by event type or time range.

5. **`agent_log_path` validation is weaker than it could be.** Issue #2394 proposes adding a DB existence check to `agent_log_path`. That check requires a separate query against `agent_runs`. With an event table, the log's existence and its content live in the same place.

---

## Goals

- Answer cross-run questions from the CLI, TUI, and MCP without reading log files
- Support retention policies (expire events by age or run count) independently of flat files
- Enable filtered log retrieval by event type, turn number, or tool name
- Lay the foundation for an agent activity feed in the TUI and web UI

## Non-Goals

- Replacing flat log files for live streaming (files remain the primary write path during a run)
- Full-text search across event content (FTS5 is a later addition if usage warrants it)
- Changing how agents are launched or how events are emitted
- Real-time per-event DB writes during a run (out of scope; see §Design)

---

## Proposed Design

### Core idea: post-run ingestion

Flat files are the right write path during a live run — sequential append, no locking, zero overhead. After a run completes (or is cancelled/failed), an ingest step parses the NDJSON log and writes one row per event into a new `agent_run_events` table. The flat file is retained as a backup and streaming source; the DB becomes the query surface.

```
During run:
  claude stdout → drain_stream_json() → flat log file (unchanged)

After run completes:
  flat log file → ingest_run_events() → agent_run_events table
```

This keeps the write-hot path identical to today and adds zero overhead to running agents.

### Schema

```sql
-- Migration 0XX: agent event indexing
CREATE TABLE agent_run_events (
    id            TEXT PRIMARY KEY,          -- ULID
    run_id        TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    seq           INTEGER NOT NULL,          -- 0-based line number in the log file
    event_type    TEXT NOT NULL,             -- stream-json "type" field
    tool_name     TEXT,                      -- populated for tool_use / tool_result events
    turn          INTEGER,                   -- assistant turn number (null for non-assistant events)
    payload       TEXT NOT NULL,             -- full JSON of the event (for retrieval)
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_agent_run_events_run_id ON agent_run_events(run_id, seq);
CREATE INDEX idx_agent_run_events_type   ON agent_run_events(run_id, event_type);
CREATE INDEX idx_agent_run_events_tool   ON agent_run_events(run_id, tool_name) WHERE tool_name IS NOT NULL;
```

`ON DELETE CASCADE` means pruning a run from `agent_runs` automatically removes its events — one delete call handles both.

### Ingest function

```rust
pub fn ingest_run_events(conn: &Connection, run_id: &str) -> Result<usize> {
    let log_path = agent_log_path_verified(conn, run_id)?;
    let file = std::fs::File::open(&log_path)?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for (seq, line) in reader.lines().enumerate() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };

        let event_type = v["type"].as_str().unwrap_or("unknown").to_string();
        let tool_name = v["name"].as_str().map(str::to_string);  // tool_use / tool_result
        let turn = v["turn"].as_u64().map(|t| t as i64);

        conn.execute(
            "INSERT OR IGNORE INTO agent_run_events
             (id, run_id, seq, event_type, tool_name, turn, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                new_id(), run_id, seq as i64, event_type, tool_name, turn,
                line, utc_now(),
            ],
        )?;
        count += 1;
    }

    Ok(count)
}
```

`INSERT OR IGNORE` makes ingest idempotent — safe to re-run if a previous attempt was interrupted.

### Trigger points

Ingest runs automatically at run completion. The three existing completion paths each call one of:

| Completion path | Where to add ingest |
|---|---|
| `drain_stream_json()` EOF (normal) | After the result event is written to DB |
| Cancellation via SIGTERM | After `status = cancelled` is written |
| Orphan reaper marks run failed | After `status = failed` is written |

Ingest is synchronous and runs on the same thread that owns the run — no new threads required.

### `agent_log_path_verified` connection

This RFC makes the `agent_log_path_verified(conn, run_id)` function from issue #2394 a first-class internal primitive rather than a safety shim. The ingest function calls it naturally — you cannot ingest events for a run that does not exist in the DB.

### Retention

A new `conductor runs prune` CLI command (and equivalent MCP tool) deletes runs older than a configurable threshold:

```bash
conductor runs prune --older-than 30d   # delete runs and events older than 30 days
conductor runs prune --keep-last 500    # keep only the 500 most recent runs per repo
```

`ON DELETE CASCADE` handles event cleanup automatically. The flat log file is removed in the same operation.

---

## What becomes possible

| Query | Today | After RFC |
|---|---|---|
| All tool calls made by run X | grep log file | `SELECT * FROM agent_run_events WHERE run_id=? AND event_type='tool_use'` |
| All runs that used tool Y | grep N log files | `SELECT DISTINCT run_id FROM agent_run_events WHERE tool_name=?` |
| Turn count for run X | scan log bytes | `SELECT MAX(turn) FROM agent_run_events WHERE run_id=?` |
| Events for run X, type filter | N/A | `mcp get_step_log` with `event_type` filter param |
| Prune runs older than 30 days | manual file deletion | `conductor runs prune --older-than 30d` |

---

## MCP `get_step_log` enhancement

The existing `conductor_get_step_log` tool returns the full raw log. After this RFC it gains optional filter params:

```json
{
  "run_id": "01J...",
  "event_types": ["tool_use", "tool_result"],
  "tool_name": "Bash",
  "limit": 50,
  "offset": 0
}
```

Unfiltered calls fall back to reading the flat file (backward compatible). Filtered calls query `agent_run_events` directly.

---

## Implementation Plan

### Phase 1 — Schema and ingest (no behavior change)
1. DB migration — add `agent_run_events` table with indexes
2. Implement `ingest_run_events()` in `conductor-core/src/agent/`
3. Wire ingest into `drain_stream_json()` EOF path
4. Wire ingest into cancellation and orphan-reaper paths
5. Add `conductor runs ingest --run-id <id>` CLI command for manual backfill

### Phase 2 — Query surface
6. `AgentManager::list_events()` — typed query wrapper
7. `conductor runs events <run-id>` CLI subcommand with `--type` filter
8. `conductor_get_step_log` MCP tool gains filter params
9. TUI: filtered event view in run detail panel (replaces raw log tail)

### Phase 3 — Retention
10. `conductor runs prune` CLI command
11. Config option: `[agent_runs] retention_days = 30` in `config.toml`
12. Prune runs automatically on conductor startup if retention is configured

---

## Open Questions

1. **Backfill existing runs.** Completed runs already have flat log files but no event rows. Should `conductor runs ingest` (Phase 1, step 5) be run automatically on first startup after migration, or left as an opt-in command?

2. **Payload storage size.** Storing the full JSON payload per event means large tool outputs (e.g., Bash stdout) are duplicated between the flat file and the DB. Should payloads be truncated at a threshold (e.g., 64 KB) with a `truncated` flag, or stored in full?

3. **FTS5 for content search.** SQLite FTS5 would enable `SELECT * FROM agent_run_events WHERE payload MATCH 'error'`. Is full-text search within event payloads valuable enough to add the schema complexity in Phase 2, or defer to a later RFC?

4. **Flat file deprecation.** Long-term, if the event table is the source of truth for queries and the MCP tool, do flat files become redundant? A future RFC could propose replacing them entirely (streaming writes to DB via WAL, no flat files). Out of scope here — flat files are retained indefinitely.

---

## Related

- devinrosen/conductor-ai#2394 — `agent_log_path` validation; `agent_log_path_verified` is used directly by the ingest function proposed here
- docs/rfc/closed/016-headless-agent-execution.md — headless subprocess infrastructure; `drain_stream_json()` is the completion hook where ingest is wired in
- docs/rfc/open/012-external-control-api.md — external API RFC; filtered log retrieval via MCP aligns with the broader external control surface
