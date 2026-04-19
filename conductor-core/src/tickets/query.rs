use rusqlite::Connection;

use crate::db::query_collect;
use crate::error::{ConductorError, Result};

use super::Ticket;

/// Ticket columns for SELECT queries that join `tickets` with alias `t`.
pub(super) const TICKET_COLS: &str = "t.id, t.repo_id, t.source_type, t.source_id, t.title, t.body, t.state, t.labels, t.assignee, t.priority, t.url, t.synced_at, t.raw_json, t.workflow, t.agent_map";
/// Ticket columns for SELECT queries without a table alias.
pub(super) const TICKET_COLS_BARE: &str = "id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map";

pub(super) fn map_ticket_row(row: &rusqlite::Row) -> rusqlite::Result<Ticket> {
    Ok(Ticket {
        id: row.get("id")?,
        repo_id: row.get("repo_id")?,
        source_type: row.get("source_type")?,
        source_id: row.get("source_id")?,
        title: row.get("title")?,
        body: row.get("body")?,
        state: row.get("state")?,
        labels: row.get("labels")?,
        assignee: row.get("assignee")?,
        priority: row.get("priority")?,
        url: row.get("url")?,
        synced_at: row.get("synced_at")?,
        raw_json: row.get("raw_json")?,
        workflow: row.get("workflow")?,
        agent_map: row.get("agent_map")?,
    })
}

/// Runs the shared double-join query for a single `dep_type` and returns
/// `(from_ticket_id, to_ticket_id, from_ticket, to_ticket)` for every row.
/// Used by `get_all_dependencies` to eliminate query/mapper duplication.
pub(super) fn query_dep_pairs(
    conn: &Connection,
    dep_type: &str,
) -> Result<Vec<(String, String, Ticket, Ticket)>> {
    // Use LEFT JOIN so orphaned edges (referencing deleted tickets) still
    // produce rows — we detect them via a NULL tf.id / tt.id and return
    // TicketNotFound instead of silently dropping the edge.
    let mut stmt = conn
        .prepare(
            "SELECT d.from_ticket_id, d.to_ticket_id,
             tf.id AS tf_id, tf.repo_id AS tf_repo_id, tf.source_type AS tf_source_type, tf.source_id AS tf_source_id,
             tf.title AS tf_title, tf.body AS tf_body, tf.state AS tf_state,
             tf.labels AS tf_labels, tf.assignee AS tf_assignee, tf.priority AS tf_priority,
             tf.url AS tf_url, tf.synced_at AS tf_synced_at, tf.raw_json AS tf_raw_json,
             tf.workflow AS tf_workflow, tf.agent_map AS tf_agent_map,
             tt.id AS tt_id, tt.repo_id AS tt_repo_id, tt.source_type AS tt_source_type, tt.source_id AS tt_source_id,
             tt.title AS tt_title, tt.body AS tt_body, tt.state AS tt_state,
             tt.labels AS tt_labels, tt.assignee AS tt_assignee, tt.priority AS tt_priority,
             tt.url AS tt_url, tt.synced_at AS tt_synced_at, tt.raw_json AS tt_raw_json,
             tt.workflow AS tt_workflow, tt.agent_map AS tt_agent_map
             FROM ticket_dependencies d
             LEFT JOIN tickets tf ON tf.id = d.from_ticket_id
             LEFT JOIN tickets tt ON tt.id = d.to_ticket_id
             WHERE d.dep_type = :dep_type",
        )
        .map_err(ConductorError::Database)?;

    let rows = stmt
        .query_map(rusqlite::named_params! { ":dep_type": dep_type }, |row| {
            let from_id: String = row.get("from_ticket_id")?;
            let to_id: String = row.get("to_ticket_id")?;
            let tf_id: Option<String> = row.get("tf_id")?;
            let tt_id: Option<String> = row.get("tt_id")?;
            let from_ticket = if tf_id.is_some() {
                Some(map_ticket_row_aliased(row, "tf_")?)
            } else {
                None
            };
            let to_ticket = if tt_id.is_some() {
                Some(map_ticket_row_aliased(row, "tt_")?)
            } else {
                None
            };
            Ok((from_id, to_id, from_ticket, to_ticket))
        })
        .map_err(ConductorError::Database)?;

    let mut result = Vec::new();
    for row in rows {
        let (from_id, to_id, from_ticket, to_ticket) = row.map_err(ConductorError::Database)?;
        match (from_ticket, to_ticket) {
            (Some(from), Some(to)) => result.push((from_id, to_id, from, to)),
            (None, _) => return Err(ConductorError::TicketNotFound { id: from_id }),
            (_, None) => return Err(ConductorError::TicketNotFound { id: to_id }),
        }
    }
    Ok(result)
}

/// Like `query_dep_pairs` but scoped to a single repo (edges where at least one
/// endpoint belongs to `repo_id`). Uses INNER JOIN so orphaned edges are silently
/// excluded rather than returning an error — acceptable for display-only paths.
pub(super) fn query_dep_pairs_for_repo(
    conn: &Connection,
    dep_type: &str,
    repo_id: &str,
) -> Result<Vec<(String, String, Ticket, Ticket)>> {
    query_collect(
        conn,
        "SELECT d.from_ticket_id, d.to_ticket_id,
         tf.id AS tf_id, tf.repo_id AS tf_repo_id, tf.source_type AS tf_source_type, tf.source_id AS tf_source_id,
         tf.title AS tf_title, tf.body AS tf_body, tf.state AS tf_state,
         tf.labels AS tf_labels, tf.assignee AS tf_assignee, tf.priority AS tf_priority,
         tf.url AS tf_url, tf.synced_at AS tf_synced_at, tf.raw_json AS tf_raw_json,
         tf.workflow AS tf_workflow, tf.agent_map AS tf_agent_map,
         tt.id AS tt_id, tt.repo_id AS tt_repo_id, tt.source_type AS tt_source_type, tt.source_id AS tt_source_id,
         tt.title AS tt_title, tt.body AS tt_body, tt.state AS tt_state,
         tt.labels AS tt_labels, tt.assignee AS tt_assignee, tt.priority AS tt_priority,
         tt.url AS tt_url, tt.synced_at AS tt_synced_at, tt.raw_json AS tt_raw_json,
         tt.workflow AS tt_workflow, tt.agent_map AS tt_agent_map
         FROM ticket_dependencies d
         JOIN tickets tf ON tf.id = d.from_ticket_id
         JOIN tickets tt ON tt.id = d.to_ticket_id
         WHERE d.dep_type = :dep_type AND (tf.repo_id = :repo_id OR tt.repo_id = :repo_id)",
        rusqlite::named_params! { ":dep_type": dep_type, ":repo_id": repo_id },
        |row| {
            let from_id: String = row.get("from_ticket_id")?;
            let to_id: String = row.get("to_ticket_id")?;
            let from_ticket = map_ticket_row_aliased(row, "tf_")?;
            let to_ticket = map_ticket_row_aliased(row, "tt_")?;
            Ok((from_id, to_id, from_ticket, to_ticket))
        },
    )
}

/// Map a ticket from a row where columns have a prefix alias (e.g. "tf_id", "tf_repo_id").
/// Used for JOIN queries that select two ticket sets with different table aliases.
pub(super) fn map_ticket_row_aliased(
    row: &rusqlite::Row,
    prefix: &str,
) -> rusqlite::Result<Ticket> {
    let mut col = String::with_capacity(prefix.len() + 16);
    macro_rules! col {
        ($name:expr) => {{
            col.clear();
            col.push_str(prefix);
            col.push_str($name);
            col.as_str()
        }};
    }
    Ok(Ticket {
        id: row.get(col!("id"))?,
        repo_id: row.get(col!("repo_id"))?,
        source_type: row.get(col!("source_type"))?,
        source_id: row.get(col!("source_id"))?,
        title: row.get(col!("title"))?,
        body: row.get(col!("body"))?,
        state: row.get(col!("state"))?,
        labels: row.get(col!("labels"))?,
        assignee: row.get(col!("assignee"))?,
        priority: row.get(col!("priority"))?,
        url: row.get(col!("url"))?,
        synced_at: row.get(col!("synced_at"))?,
        raw_json: row.get(col!("raw_json"))?,
        workflow: row.get(col!("workflow"))?,
        agent_map: row.get(col!("agent_map"))?,
    })
}
