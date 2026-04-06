use std::collections::HashMap;

use chrono::Utc;

/// Ticket columns for SELECT queries that join `tickets` with alias `t`.
const TICKET_COLS: &str = "t.id, t.repo_id, t.source_type, t.source_id, t.title, t.body, t.state, t.labels, t.assignee, t.priority, t.url, t.synced_at, t.raw_json, t.workflow, t.agent_map";
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::github::has_merged_pr;
use crate::worktree::WorktreeManager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub repo_id: String,
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub url: String,
    pub synced_at: String,
    pub raw_json: String,
    pub workflow: Option<String>,
    pub agent_map: Option<String>,
}

/// A normalized ticket from any source, ready to be upserted into the database.
pub struct TicketInput {
    pub source_type: String,
    pub source_id: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub url: String,
    pub raw_json: Option<String>,
    /// Label details (name + color) for populating the ticket_labels join table.
    /// Pass `vec![]` for sources that do not supply color data.
    pub label_details: Vec<TicketLabelInput>,
    /// Source IDs (within the same source_type) of tickets that block this one.
    /// Resolved to internal ULIDs and written to ticket_dependencies during upsert.
    pub blocked_by: Vec<String>,
    /// Source IDs of child tickets (this ticket is the parent).
    /// Resolved to internal ULIDs and written to ticket_dependencies during upsert.
    pub children: Vec<String>,
    /// Source ID of the parent ticket (this ticket is a child).
    /// Resolved and written to ticket_dependencies during upsert.
    /// Setting this replaces any existing parent relationship for this ticket.
    pub parent: Option<String>,
}

const VALID_TICKET_STATES: &[&str] = &["open", "in_progress", "closed"];

impl TicketInput {
    /// Validate this ticket input, returning an error if any field is invalid.
    pub fn validate(&self) -> Result<()> {
        if !VALID_TICKET_STATES.contains(&self.state.as_str()) {
            return Err(crate::error::ConductorError::InvalidInput(format!(
                "Invalid ticket state '{}'. Must be one of: open, in_progress, closed.",
                self.state
            )));
        }
        Ok(())
    }

    fn labels_json(&self) -> String {
        serde_json::to_string(&self.labels).unwrap_or_else(|_| "[]".to_string())
    }
}

/// Label detail passed in during sync. Carries color alongside the name.
pub struct TicketLabelInput {
    pub name: String,
    pub color: Option<String>,
}

/// A label row from the ticket_labels table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLabel {
    pub ticket_id: String,
    pub label: String,
    pub color: Option<String>,
}

/// Dependency relationships for a single ticket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TicketDependencies {
    /// Tickets that must complete before this one (blocks this ticket).
    pub blocked_by: Vec<Ticket>,
    /// Tickets that this ticket blocks.
    pub blocks: Vec<Ticket>,
    /// Parent ticket, if any.
    pub parent: Option<Ticket>,
    /// Child tickets.
    pub children: Vec<Ticket>,
}

impl TicketDependencies {
    /// Returns `true` if this ticket has at least one unresolved (non-closed) blocker.
    pub fn is_actively_blocked(&self) -> bool {
        self.blocked_by.iter().any(|b| b.state != "closed")
    }

    /// Returns an iterator over unresolved (non-closed) blockers.
    pub fn active_blockers(&self) -> impl Iterator<Item = &Ticket> {
        self.blocked_by.iter().filter(|b| b.state != "closed")
    }
}

/// A ticket that is ready to be worked on: not closed, has no unresolved blockers,
/// and is not already linked to an active workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyTicket {
    pub id: String,
    pub source_id: String,
    pub title: String,
    pub url: String,
    /// The dep_type of an incoming parent_of edge, if any ('parent_of'), or `None` for
    /// unconstrained tickets with no dependency edges pointing at them.
    pub dep_type: Option<String>,
}

/// Filter options for [`TicketSyncer::list_filtered`].
pub struct TicketFilter {
    /// Only include tickets that have ALL of these labels.
    /// NOTE: label filtering uses the `ticket_labels` join table, which is only
    /// populated when `label_details` are provided during upsert. Tickets synced
    /// without label details will never match a label filter even if their JSON
    /// `labels` field is non-empty.
    pub labels: Vec<String>,
    /// Case-insensitive substring match against ticket title and body (ASCII only).
    pub search: Option<String>,
    /// When `false` (default), only open tickets are returned.
    pub include_closed: bool,
}

impl Ticket {
    pub fn matches_filter(&self, query: &str) -> bool {
        self.title.to_lowercase().contains(query)
            || self.source_id.contains(query)
            || self.labels.to_lowercase().contains(query)
    }
}

fn ticket_not_found(id: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> ConductorError {
    let id = id.into();
    move |e| match e {
        rusqlite::Error::QueryReturnedNoRows => ConductorError::TicketNotFound { id },
        _ => ConductorError::Database(e),
    }
}

pub struct TicketSyncer<'a> {
    conn: &'a Connection,
}

const CLOSED_TICKET_ARTIFACTS_SQL: &str = "SELECT r.local_path, w.path, w.branch, r.remote_url
     FROM worktrees w
     JOIN repos r ON r.id = w.repo_id
     WHERE w.repo_id = ?1
       AND w.status != 'merged'
       AND w.ticket_id IS NOT NULL
       AND w.ticket_id IN (SELECT id FROM tickets WHERE state = 'closed')";

/// Look up the internal ULID for a dependency ticket by its source_id.
/// Checks `id_map` first (O(1) in-batch lookup) then falls back to a DB query.
/// Returns `None` if the ticket does not exist; the caller is responsible for
/// warning and skipping the dependency in that case.
fn resolve_dep_ticket_id(
    id_map: &HashMap<&str, &str>,
    tx: &rusqlite::Transaction<'_>,
    repo_id: &str,
    source_type: &str,
    src: &str,
    owner_source_id: &str,
    context: &str,
) -> Result<Option<String>> {
    if let Some(&id) = id_map.get(src) {
        return Ok(Some(id.to_string()));
    }
    match tx.query_row(
        "SELECT id FROM tickets WHERE repo_id = ?1 AND source_type = ?2 AND source_id = ?3",
        params![repo_id, source_type, src],
        |row| row.get(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            warn!("ticket dependency source_id {} not found, skipping", src);
            Ok(None)
        }
        Err(e) => Err(ConductorError::TicketSync(format!(
            "ticket {}: {} lookup for source_id={}: {}",
            owner_source_id, context, src, e
        ))),
    }
}

impl<'a> TicketSyncer<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Upsert a batch of tickets for a repo. Returns the number of tickets upserted.
    pub fn upsert_tickets(&self, repo_id: &str, tickets: &[TicketInput]) -> Result<usize> {
        for ticket in tickets {
            ticket.validate()?;
        }

        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();

        // First pass: upsert tickets and their labels, collecting internal IDs.
        let mut ticket_ids: Vec<(&TicketInput, String)> = Vec::with_capacity(tickets.len());
        for ticket in tickets {
            let id = crate::new_id();
            let labels_json = ticket.labels_json();
            // When the caller supplies no raw_json (None), preserve whatever is
            // already stored rather than overwriting with an empty placeholder.
            // This is resolved in Rust so the SQL layer carries no sentinel knowledge.
            let raw_json: String = match &ticket.raw_json {
                Some(v) => v.clone(),
                None => tx
                    .query_row(
                        "SELECT raw_json FROM tickets WHERE repo_id = ?1 AND source_type = ?2 AND source_id = ?3",
                        params![repo_id, ticket.source_type, ticket.source_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap_or_else(|_| "{}".to_string()),
            };
            tx.execute(
                "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(repo_id, source_type, source_id) DO UPDATE SET
                     title = excluded.title,
                     body = excluded.body,
                     state = excluded.state,
                     labels = excluded.labels,
                     assignee = excluded.assignee,
                     priority = excluded.priority,
                     url = excluded.url,
                     synced_at = excluded.synced_at,
                     raw_json = excluded.raw_json",
                params![
                    id,
                    repo_id,
                    ticket.source_type,
                    ticket.source_id,
                    ticket.title,
                    ticket.body,
                    ticket.state,
                    labels_json,
                    ticket.assignee,
                    ticket.priority,
                    ticket.url,
                    now,
                    raw_json,
                ],
            )?;

            let ticket_id: String = tx.query_row(
                "SELECT id FROM tickets WHERE repo_id = ?1 AND source_type = ?2 AND source_id = ?3",
                params![repo_id, ticket.source_type, ticket.source_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "DELETE FROM ticket_labels WHERE ticket_id = ?1",
                params![ticket_id],
            )?;
            for ld in &ticket.label_details {
                tx.execute(
                    "INSERT OR REPLACE INTO ticket_labels (ticket_id, label, color) VALUES (?1, ?2, ?3)",
                    params![ticket_id, ld.name, ld.color],
                )?;
            }
            ticket_ids.push((ticket, ticket_id));
        }

        // Build a source_id → internal ULID map from the first pass for O(1) lookups.
        let id_map: HashMap<&str, &str> = ticket_ids
            .iter()
            .map(|(t, id)| (t.source_id.as_str(), id.as_str()))
            .collect();

        // Second pass: write ticket_dependencies. All tickets are already upserted above,
        // so forward references within the same batch resolve correctly.
        for (ticket, ticket_id) in &ticket_ids {
            // Clear stale dependency rows owned by this ticket before re-inserting,
            // but only per-field when the TicketInput actually declares that field.
            // An empty value (e.g. from a GitHub sync that doesn't parse body text)
            // is treated as "no opinion" and must not overwrite deps set by another
            // source. Each dep type is guarded independently so that setting only
            // `parent` does not accidentally wipe existing `blocked_by` or `children`.
            if !ticket.blocked_by.is_empty() {
                tx.execute(
                    "DELETE FROM ticket_dependencies WHERE to_ticket_id = ?1 AND dep_type = 'blocks'",
                    params![ticket_id],
                )?;
            }
            if !ticket.children.is_empty() {
                tx.execute(
                    "DELETE FROM ticket_dependencies WHERE from_ticket_id = ?1 AND dep_type = 'parent_of'",
                    params![ticket_id],
                )?;
            }

            // blocked_by: another ticket blocks this one → (blocker_id, ticket_id, 'blocks')
            for src in &ticket.blocked_by {
                let blocker_id = resolve_dep_ticket_id(
                    &id_map,
                    &tx,
                    repo_id,
                    &ticket.source_type,
                    src,
                    &ticket.source_id,
                    "blocked_by",
                )?;
                if let Some(id) = blocker_id {
                    tx.execute(
                        "INSERT OR IGNORE INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (?1, ?2, 'blocks')",
                        params![id, ticket_id],
                    )?;
                }
            }

            // children: this ticket is parent of another → (ticket_id, child_id, 'parent_of')
            for src in &ticket.children {
                let child_id = resolve_dep_ticket_id(
                    &id_map,
                    &tx,
                    repo_id,
                    &ticket.source_type,
                    src,
                    &ticket.source_id,
                    "children",
                )?;
                if let Some(id) = child_id {
                    tx.execute(
                        "INSERT OR IGNORE INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (?1, ?2, 'parent_of')",
                        params![ticket_id, id],
                    )?;
                }
            }

            // parent: another ticket is parent of this one → (parent_id, ticket_id, 'parent_of')
            if let Some(src) = &ticket.parent {
                // Replace any existing parent for this ticket
                tx.execute(
                    "DELETE FROM ticket_dependencies WHERE to_ticket_id = ?1 AND dep_type = 'parent_of'",
                    params![ticket_id],
                )?;
                let parent_id = resolve_dep_ticket_id(
                    &id_map,
                    &tx,
                    repo_id,
                    &ticket.source_type,
                    src,
                    &ticket.source_id,
                    "parent",
                )?;
                if let Some(id) = parent_id {
                    tx.execute(
                        "INSERT OR IGNORE INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (?1, ?2, 'parent_of')",
                        params![id, ticket_id],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(tickets.len())
    }

    /// Mark tickets as closed if they were not in the latest sync batch.
    /// After upserting open tickets, any ticket for the same repo+source_type
    /// that wasn't in the synced set is presumed closed.
    /// Returns the number of tickets marked closed.
    pub fn close_missing_tickets(
        &self,
        repo_id: &str,
        source_type: &str,
        synced_source_ids: &[&str],
    ) -> Result<usize> {
        if synced_source_ids.is_empty() {
            // Nothing was synced — don't mark everything as closed
            return Ok(0);
        }

        let now = Utc::now().to_rfc3339();
        let ids: Vec<String> = synced_source_ids.iter().map(|s| s.to_string()).collect();
        crate::db::with_in_clause(
            "UPDATE tickets SET state = 'closed', synced_at = ?1 \
             WHERE repo_id = ?2 AND source_type = ?3 AND state != 'closed' \
             AND source_id NOT IN",
            &[
                &now as &dyn rusqlite::types::ToSql,
                &repo_id as &dyn rusqlite::types::ToSql,
                &source_type as &dyn rusqlite::types::ToSql,
            ],
            &ids,
            |sql, params| Ok(self.conn.prepare(sql)?.execute(params)?),
        )
    }

    /// Return the most recent `synced_at` timestamp for tickets in a repo.
    /// Returns `None` when no tickets exist for the repo (i.e. never synced).
    pub fn latest_synced_at(&self, repo_id: &str) -> Result<Option<String>> {
        let ts: Option<String> = self.conn.query_row(
            "SELECT MAX(synced_at) FROM tickets WHERE repo_id = ?1",
            params![repo_id],
            |row| row.get(0),
        )?;
        Ok(ts)
    }

    /// List tickets, optionally filtered by repo.
    ///
    /// Results are sorted by issue number descending (highest first).
    /// Non-numeric `source_id` values (e.g. Jira keys like `PROJ-123`) cast to 0
    /// and sort after all numeric IDs, ordered among themselves by string comparison.
    pub fn list(&self, repo_id: Option<&str>) -> Result<Vec<Ticket>> {
        let query = match repo_id {
            Some(_) => {
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets WHERE repo_id = ?1 ORDER BY CAST(source_id AS INTEGER) DESC, source_id DESC"
            }
            None => {
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets ORDER BY CAST(source_id AS INTEGER) DESC, source_id DESC"
            }
        };

        let tickets = if let Some(rid) = repo_id {
            query_collect(self.conn, query, params![rid], map_ticket_row)?
        } else {
            query_collect(self.conn, query, [], map_ticket_row)?
        };
        Ok(tickets)
    }

    /// Shared SELECT clause for ticket queries.
    fn ticket_select() -> &'static str {
        "SELECT t.id, t.repo_id, t.source_type, t.source_id, t.title, t.body, \
         t.state, t.labels, t.assignee, t.priority, t.url, t.synced_at, t.raw_json, t.workflow, t.agent_map \
         FROM tickets t"
    }

    /// Shared label EXISTS subquery fragment (requires one `?` param bound to the label value).
    fn label_exists_subquery() -> &'static str {
        "EXISTS (SELECT 1 FROM ticket_labels tl WHERE tl.ticket_id = t.id AND tl.label = ?)"
    }

    /// Execute a ticket SELECT query with the given SQL and boxed parameters.
    fn execute_ticket_query(
        &self,
        sql: &str,
        param_values: Vec<Box<dyn rusqlite::types::ToSql>>,
    ) -> Result<Vec<Ticket>> {
        let params: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params.as_slice(), map_ticket_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// List tickets with optional filtering. Open-only by default.
    ///
    /// Filters are applied in SQL:
    /// - `repo_id`: scoped to a single repo when provided.
    /// - `filter.include_closed`: when `false`, restricts to `state = 'open'`.
    /// - `filter.labels`: ALL listed labels must be present (AND semantics via EXISTS subqueries).
    /// - `filter.search`: `LIKE %term%` on title and body (case-insensitive for ASCII).
    pub fn list_filtered(
        &self,
        repo_id: Option<&str>,
        filter: &TicketFilter,
    ) -> Result<Vec<Ticket>> {
        let select = Self::ticket_select();

        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(rid) = repo_id {
            conditions.push("t.repo_id = ?".to_string());
            param_values.push(Box::new(rid.to_string()));
        }

        if !filter.include_closed {
            conditions.push("t.state = 'open'".to_string());
        }

        for label in &filter.labels {
            conditions.push(Self::label_exists_subquery().to_string());
            param_values.push(Box::new(label.clone()));
        }

        if let Some(ref term) = filter.search {
            conditions.push("(t.title LIKE ? OR t.body LIKE ?)".to_string());
            let pattern = format!("%{term}%");
            param_values.push(Box::new(pattern.clone()));
            param_values.push(Box::new(pattern));
        }

        let sql = if conditions.is_empty() {
            format!("{select} ORDER BY CAST(t.source_id AS INTEGER) DESC, t.source_id DESC")
        } else {
            format!(
                "{select} WHERE {} ORDER BY CAST(t.source_id AS INTEGER) DESC, t.source_id DESC",
                conditions.join(" AND ")
            )
        };

        self.execute_ticket_query(&sql, param_values)
    }

    /// Link a ticket to a worktree.
    /// Returns an error if the worktree already has a linked ticket.
    pub fn link_to_worktree(&self, ticket_id: &str, worktree_id: &str) -> Result<()> {
        let existing: Option<String> = self.conn.query_row(
            "SELECT ticket_id FROM worktrees WHERE id = ?1",
            params![worktree_id],
            |row| row.get(0),
        )?;
        if existing.is_some() {
            return Err(ConductorError::TicketAlreadyLinked);
        }
        self.conn.execute(
            "UPDATE worktrees SET ticket_id = ?1 WHERE id = ?2",
            params![ticket_id, worktree_id],
        )?;
        Ok(())
    }

    /// Fetch a single ticket by repo ID + external source ID (e.g. GitHub issue number).
    /// Returns `TicketNotFound` if no matching ticket exists.
    pub fn get_by_source_id(&self, repo_id: &str, source_id: &str) -> Result<Ticket> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets WHERE repo_id = ?1 AND source_id = ?2",
                params![repo_id, source_id],
                map_ticket_row,
            )
            .map_err(ticket_not_found(source_id))
    }

    /// Fetch a single ticket by source_id across all repos.
    /// Returns the first match. Use when the caller does not know the repo_id.
    pub fn get_by_source_id_any_repo(&self, source_id: &str) -> Result<Ticket> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets WHERE source_id = ?1 LIMIT 1",
                params![source_id],
                map_ticket_row,
            )
            .map_err(ticket_not_found(source_id))
    }

    /// Fetch a single ticket by its internal (ULID) ID.
    pub fn get_by_id(&self, ticket_id: &str) -> Result<Ticket> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets WHERE id = ?1",
                params![ticket_id],
                map_ticket_row,
            )
            .map_err(ticket_not_found(ticket_id))
    }

    /// Update the `state`, `workflow`, and/or `agent_map` columns on a ticket.
    ///
    /// For `workflow` and `agent_map`:
    /// - `Some("")` clears the column to NULL.
    /// - `Some(value)` sets the column to `value`.
    /// - `None` leaves the column unchanged.
    ///
    /// For `state`:
    /// - `Some(value)` sets the state (must be one of: open, in_progress, closed).
    /// - `None` leaves the state unchanged.
    /// - Empty string is NOT valid for state (state is NOT NULL).
    pub fn update_ticket(
        &self,
        ticket_id: &str,
        state: Option<&str>,
        workflow: Option<&str>,
        agent_map: Option<&str>,
    ) -> Result<()> {
        // Verify ticket exists
        let _ = self.get_by_id(ticket_id)?;

        if let Some(s) = state {
            if !VALID_TICKET_STATES.contains(&s) {
                return Err(crate::error::ConductorError::InvalidInput(format!(
                    "Invalid ticket state '{}'. Must be one of: open, in_progress, closed.",
                    s
                )));
            }
            self.conn.execute(
                "UPDATE tickets SET state = ?1 WHERE id = ?2",
                rusqlite::params![s, ticket_id],
            )?;
        }
        if let Some(w) = workflow {
            let val: Option<&str> = if w.is_empty() { None } else { Some(w) };
            self.conn.execute(
                "UPDATE tickets SET workflow = ?1 WHERE id = ?2",
                rusqlite::params![val, ticket_id],
            )?;
        }
        if let Some(a) = agent_map {
            let val: Option<&str> = if a.is_empty() { None } else { Some(a) };
            self.conn.execute(
                "UPDATE tickets SET agent_map = ?1 WHERE id = ?2",
                rusqlite::params![val, ticket_id],
            )?;
        }
        Ok(())
    }

    /// Delete a ticket by its `(repo_id, source_type, source_id)` key.
    /// NULLs out `workflow_runs.ticket_id` first (that FK lacks ON DELETE SET NULL),
    /// then deletes the ticket row. Returns an error if no matching ticket exists.
    pub fn delete_ticket(&self, repo_id: &str, source_type: &str, source_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        // Look up the ticket id first so we can clean up the FK.
        let ticket_id: String = tx
            .query_row(
                "SELECT id FROM tickets WHERE repo_id = ?1 AND source_type = ?2 AND source_id = ?3",
                params![repo_id, source_type, source_id],
                |row| row.get(0),
            )
            .map_err(ticket_not_found(format!("{source_type}#{source_id}")))?;

        // NULL out workflow_runs.ticket_id (FK lacks ON DELETE SET NULL).
        tx.execute(
            "UPDATE workflow_runs SET ticket_id = NULL WHERE ticket_id = ?1",
            params![ticket_id],
        )?;

        // Delete the ticket row. Cascades handle ticket_labels and feature_tickets;
        // worktrees.ticket_id is ON DELETE SET NULL.
        tx.execute("DELETE FROM tickets WHERE id = ?1", params![ticket_id])?;

        tx.commit()?;
        Ok(())
    }

    /// Upsert a batch of synced tickets, close any missing ones, and mark their
    /// worktrees. Returns `(synced, closed)` counts. Errors from the close and
    /// mark steps are logged as warnings rather than propagated, matching the
    /// intent that one source failure should not abort the entire sync.
    pub fn sync_and_close_tickets(
        &self,
        repo_id: &str,
        source_type: &str,
        tickets: &[TicketInput],
    ) -> (usize, usize) {
        let warn_and_default = |result: Result<usize>, ctx: &str| {
            result.unwrap_or_else(|e| {
                warn!("{ctx} failed for {repo_id}: {e}");
                0
            })
        };
        let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
        let synced = warn_and_default(self.upsert_tickets(repo_id, tickets), "upsert_tickets");
        let closed = warn_and_default(
            self.close_missing_tickets(repo_id, source_type, &synced_ids),
            "close_missing_tickets",
        );
        warn_and_default(
            self.mark_worktrees_for_closed_tickets(repo_id),
            "mark_worktrees_for_closed_tickets",
        );
        (synced, closed)
    }

    /// Query the normalized labels for a ticket by its internal (ULID) ID.
    pub fn get_labels(&self, ticket_id: &str) -> Result<Vec<TicketLabel>> {
        query_collect(
            self.conn,
            "SELECT ticket_id, label, color FROM ticket_labels WHERE ticket_id = ?1 ORDER BY label",
            params![ticket_id],
            |row| {
                Ok(TicketLabel {
                    ticket_id: row.get(0)?,
                    label: row.get(1)?,
                    color: row.get(2)?,
                })
            },
        )
    }

    /// Fetch all label rows grouped by ticket_id.
    /// Returns a `HashMap<ticket_id, Vec<TicketLabel>>` in a single query,
    /// avoiding N+1 per-ticket queries.
    pub fn get_all_labels(&self) -> Result<HashMap<String, Vec<TicketLabel>>> {
        let all = query_collect(
            self.conn,
            "SELECT ticket_id, label, color FROM ticket_labels ORDER BY ticket_id, label",
            [],
            |row| {
                Ok(TicketLabel {
                    ticket_id: row.get(0)?,
                    label: row.get(1)?,
                    color: row.get(2)?,
                })
            },
        )?;
        let mut map: HashMap<String, Vec<TicketLabel>> = HashMap::new();
        for lbl in all {
            map.entry(lbl.ticket_id.clone()).or_default().push(lbl);
        }
        Ok(map)
    }

    /// Returns dependency relationships for a single ticket.
    pub fn get_dependencies(&self, ticket_id: &str) -> Result<TicketDependencies> {
        // Tickets that block this one (from_ticket_id = blocker, to_ticket_id = this)
        let blocked_by = query_collect(
            self.conn,
            &format!(
                "SELECT {TICKET_COLS} FROM tickets t
                 JOIN ticket_dependencies d ON d.from_ticket_id = t.id
                 WHERE d.to_ticket_id = ?1 AND d.dep_type = 'blocks'"
            ),
            params![ticket_id],
            map_ticket_row,
        )?;

        // Tickets this one blocks (from_ticket_id = this, to_ticket_id = blocked)
        let blocks = query_collect(
            self.conn,
            &format!(
                "SELECT {TICKET_COLS} FROM tickets t
                 JOIN ticket_dependencies d ON d.to_ticket_id = t.id
                 WHERE d.from_ticket_id = ?1 AND d.dep_type = 'blocks'"
            ),
            params![ticket_id],
            map_ticket_row,
        )?;

        // Parent ticket (from_ticket_id = parent, to_ticket_id = this)
        let parent = query_collect(
            self.conn,
            &format!(
                "SELECT {TICKET_COLS} FROM tickets t
                 JOIN ticket_dependencies d ON d.from_ticket_id = t.id
                 WHERE d.to_ticket_id = ?1 AND d.dep_type = 'parent_of'"
            ),
            params![ticket_id],
            map_ticket_row,
        )?
        .into_iter()
        .next();

        // Child tickets (from_ticket_id = this, to_ticket_id = child)
        let children = query_collect(
            self.conn,
            &format!(
                "SELECT {TICKET_COLS} FROM tickets t
                 JOIN ticket_dependencies d ON d.to_ticket_id = t.id
                 WHERE d.from_ticket_id = ?1 AND d.dep_type = 'parent_of'"
            ),
            params![ticket_id],
            map_ticket_row,
        )?;

        Ok(TicketDependencies {
            blocked_by,
            blocks,
            parent,
            children,
        })
    }

    /// Batch-loads dependencies for all tickets in two queries (one per dep_type).
    /// Returns `ticket_id → TicketDependencies`. Used by the TUI background
    /// poller to avoid N+1 queries.
    pub fn get_all_dependencies(&self) -> Result<HashMap<String, TicketDependencies>> {
        let blocks_rows = query_dep_pairs(self.conn, "blocks")?;
        let parent_rows = query_dep_pairs(self.conn, "parent_of")?;

        let mut map: HashMap<String, TicketDependencies> = HashMap::new();

        // blocks_rows: (from_id=blocker, to_id=blocked, from_ticket=blocker, to_ticket=blocked)
        // → to_id's blocked_by list gets the blocker; from_id's blocks list gets the blocked
        for (from_id, to_id, from_ticket, to_ticket) in blocks_rows {
            map.entry(to_id).or_default().blocked_by.push(from_ticket);
            map.entry(from_id).or_default().blocks.push(to_ticket);
        }

        // parent_rows: (from_id=parent, to_id=child, from_ticket=parent, to_ticket=child)
        // → to_id's parent gets the parent ticket; from_id's children list gets the child
        for (from_id, to_id, from_ticket, to_ticket) in parent_rows {
            map.entry(to_id).or_default().parent = Some(from_ticket);
            map.entry(from_id).or_default().children.push(to_ticket);
        }

        Ok(map)
    }

    /// After syncing tickets, mark any linked worktrees whose ticket is now
    /// closed by setting their status to `'merged'`. Also removes the git
    /// worktree directory and branch for each affected worktree (best-effort).
    /// Only removes artifacts for worktrees whose PR has actually merged —
    /// a closed ticket is necessary but not sufficient (CI may still be pending).
    /// Called as part of the ticket sync flow, typically after
    /// [`TicketSyncer::close_missing_tickets`].
    /// Returns the number of worktrees updated.
    pub fn mark_worktrees_for_closed_tickets(&self, repo_id: &str) -> Result<usize> {
        self.mark_worktrees_for_closed_tickets_with_merge_check(repo_id, has_merged_pr)
    }

    fn mark_worktrees_for_closed_tickets_with_merge_check(
        &self,
        repo_id: &str,
        merge_check: impl Fn(&str, &str) -> bool,
    ) -> Result<usize> {
        // Collect git paths before updating so we can clean up worktree dirs and branches.
        let artifacts: Vec<(String, String, String, String)> = query_collect(
            self.conn,
            CLOSED_TICKET_ARTIFACTS_SQL,
            params![repo_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

        let now = Utc::now().to_rfc3339();
        let mut count = 0usize;

        for (repo_path, worktree_path, branch, remote_url) in &artifacts {
            if !merge_check(remote_url, branch) {
                // Ticket is closed but PR not yet merged — leave the worktree alone.
                continue;
            }
            self.conn.execute(
                "UPDATE worktrees SET status = 'merged', completed_at = ?1
                 WHERE path = ?2 AND status != 'merged'",
                params![now, worktree_path],
            )?;
            count += 1;
            WorktreeManager::remove_artifacts(repo_path, worktree_path, branch);
        }

        Ok(count)
    }

    /// Return tickets that are ready to be worked on for `repo_id`.
    ///
    /// A ticket is ready when:
    /// - Its state is not `'closed'`
    /// - It has no unresolved `'blocks'` blocker (blocker is open OR its workflow run is not
    ///   completed)
    /// - It is not already linked to an active workflow run
    ///
    /// Optional filters:
    /// - `root_ticket_id`: restrict to direct children of this ticket (parent_of edges)
    /// - `label`: restrict to tickets with this label in `ticket_labels`
    /// - `limit`: cap result count (default caller-supplied; must be > 0)
    pub fn get_ready_tickets(
        &self,
        repo_id: &str,
        root_ticket_id: Option<&str>,
        label: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ReadyTicket>> {
        let mut sql = String::from(
            "SELECT t.id, t.source_id, t.title, t.url, \
             (SELECT dep.dep_type FROM ticket_dependencies dep \
              WHERE dep.to_ticket_id = t.id AND dep.dep_type = 'parent_of' LIMIT 1) AS dep_type \
             FROM tickets t \
             WHERE t.state != 'closed' \
               AND t.repo_id = ? \
               AND NOT EXISTS ( \
                   SELECT 1 FROM ticket_dependencies dep \
                   JOIN tickets blocker ON blocker.id = dep.from_ticket_id \
                   LEFT JOIN workflow_runs wr ON wr.ticket_id = blocker.id \
                   WHERE dep.to_ticket_id = t.id \
                     AND dep.dep_type = 'blocks' \
                     AND (blocker.state != 'closed' OR COALESCE(wr.status, 'completed') != 'completed') \
               ) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM workflow_runs wr \
                   WHERE wr.ticket_id = t.id \
                     AND wr.status IN ('running', 'waiting_for_feedback', 'paused') \
               )",
        );

        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(repo_id.to_string())];

        if let Some(root_id) = root_ticket_id {
            sql.push_str(
                " AND EXISTS ( \
                   SELECT 1 FROM ticket_dependencies pof \
                   WHERE pof.from_ticket_id = ? AND pof.to_ticket_id = t.id \
                     AND pof.dep_type = 'parent_of' \
                 )",
            );
            param_values.push(Box::new(root_id.to_string()));
        }

        if let Some(lbl) = label {
            sql.push_str(&format!(" AND {}", Self::label_exists_subquery()));
            param_values.push(Box::new(lbl.to_string()));
        }

        sql.push_str(" ORDER BY CAST(t.source_id AS INTEGER) DESC, t.source_id DESC LIMIT ?");
        param_values.push(Box::new(limit as i64));

        let params: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok(ReadyTicket {
                id: row.get(0)?,
                source_id: row.get(1)?,
                title: row.get(2)?,
                url: row.get(3)?,
                dep_type: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Resolve a user-supplied ticket identifier to `(source_type, source_id)`.
    ///
    /// Accepts three forms:
    /// - GitHub PR URL (contains `/pull/`) — resolves via PR → head branch → worktree → ticket
    /// - 26-character ULID — looks up by internal ID
    /// - Anything else — treated as an external source ID (GitHub issue number or Jira key)
    pub fn resolve_ticket_id(
        &self,
        worktree_mgr: &WorktreeManager<'_>,
        repo: &crate::repo::Repo,
        ticket_id_str: &str,
    ) -> Result<(String, String)> {
        use crate::github;

        // PR URL path
        if ticket_id_str.contains("/pull/") {
            let pr_number = github::parse_pr_number_from_url(ticket_id_str).ok_or_else(|| {
                ConductorError::TicketSync(format!(
                    "could not parse PR number from URL: {ticket_id_str}"
                ))
            })?;
            let branch = github::get_pr_head_branch(&repo.remote_url, pr_number)?;
            let wt = worktree_mgr.get_by_branch(&repo.id, &branch)?;
            let ticket_id = wt.ticket_id.ok_or_else(|| {
                ConductorError::TicketSync(format!(
                    "worktree for branch {branch} has no linked ticket"
                ))
            })?;
            let ticket = self.get_by_id(&ticket_id)?;
            return Ok((ticket.source_type, ticket.source_id));
        }

        // ULID path (26 chars)
        if ticket_id_str.len() == 26 {
            match self.get_by_id(ticket_id_str) {
                Ok(ticket) => return Ok((ticket.source_type, ticket.source_id)),
                Err(ConductorError::TicketNotFound { .. }) => {
                    // Not a ULID match — fall through to source_id lookup
                }
                Err(e) => return Err(e),
            }
        }

        // External source_id path
        let ticket = self.get_by_source_id(&repo.id, ticket_id_str)?;
        Ok((ticket.source_type, ticket.source_id))
    }
}

/// Build a rich agent prompt from a ticket's context.
pub fn build_agent_prompt(ticket: &Ticket) -> String {
    let labels_display = if ticket.labels.is_empty() || ticket.labels == "[]" {
        "None".to_string()
    } else {
        ticket.labels.clone()
    };

    let body_display = if ticket.body.is_empty() {
        "(No description provided)".to_string()
    } else {
        ticket.body.clone()
    };

    format!(
        "Work on the following GitHub issue in this repository.\n\
         \n\
         Issue: #{source_id} — {title}\n\
         State: {state}\n\
         Labels: {labels}\n\
         \n\
         Description:\n\
         {body}\n\
         \n\
         Implement the changes described in the issue. Follow existing code conventions and patterns. Write tests if appropriate.",
        source_id = ticket.source_id,
        title = ticket.title,
        state = ticket.state,
        labels = labels_display,
        body = body_display,
    )
}

fn map_ticket_row(row: &rusqlite::Row) -> rusqlite::Result<Ticket> {
    map_ticket_row_at(row, 0)
}

/// Runs the shared double-join query for a single `dep_type` and returns
/// `(from_ticket_id, to_ticket_id, from_ticket, to_ticket)` for every row.
/// Used by `get_all_dependencies` to eliminate query/mapper duplication.
fn query_dep_pairs(
    conn: &Connection,
    dep_type: &str,
) -> Result<Vec<(String, String, Ticket, Ticket)>> {
    const FROM_OFFSET: usize = 2;
    const TO_OFFSET: usize = 17;

    // Use LEFT JOIN so orphaned edges (referencing deleted tickets) still
    // produce rows — we detect them via a NULL tf.id / tt.id and return
    // TicketNotFound instead of silently dropping the edge.
    let mut stmt = conn
        .prepare(
            "SELECT d.from_ticket_id, d.to_ticket_id,
             tf.id, tf.repo_id, tf.source_type, tf.source_id, tf.title, tf.body, tf.state,
             tf.labels, tf.assignee, tf.priority, tf.url, tf.synced_at, tf.raw_json,
             tf.workflow, tf.agent_map,
             tt.id, tt.repo_id, tt.source_type, tt.source_id, tt.title, tt.body, tt.state,
             tt.labels, tt.assignee, tt.priority, tt.url, tt.synced_at, tt.raw_json,
             tt.workflow, tt.agent_map
             FROM ticket_dependencies d
             LEFT JOIN tickets tf ON tf.id = d.from_ticket_id
             LEFT JOIN tickets tt ON tt.id = d.to_ticket_id
             WHERE d.dep_type = ?1",
        )
        .map_err(ConductorError::Database)?;

    let rows = stmt
        .query_map(rusqlite::params![dep_type], |row| {
            let from_id: String = row.get(0)?;
            let to_id: String = row.get(1)?;
            let from_exists: Option<String> = row.get(FROM_OFFSET)?;
            let to_exists: Option<String> = row.get(TO_OFFSET)?;
            Ok((from_id, to_id, from_exists, to_exists))
        })
        .map_err(ConductorError::Database)?;

    // First pass: check for orphaned references.
    let mut checked = Vec::new();
    for row in rows {
        let (from_id, to_id, from_exists, to_exists) = row.map_err(ConductorError::Database)?;
        if from_exists.is_none() {
            return Err(ConductorError::TicketNotFound { id: from_id });
        }
        if to_exists.is_none() {
            return Err(ConductorError::TicketNotFound { id: to_id });
        }
        checked.push((from_id, to_id));
    }

    // All tickets exist — re-query with INNER JOIN to map full Ticket objects.
    query_collect(
        conn,
        "SELECT d.from_ticket_id, d.to_ticket_id,
         tf.id, tf.repo_id, tf.source_type, tf.source_id, tf.title, tf.body, tf.state,
         tf.labels, tf.assignee, tf.priority, tf.url, tf.synced_at, tf.raw_json,
         tf.workflow, tf.agent_map,
         tt.id, tt.repo_id, tt.source_type, tt.source_id, tt.title, tt.body, tt.state,
         tt.labels, tt.assignee, tt.priority, tt.url, tt.synced_at, tt.raw_json,
         tt.workflow, tt.agent_map
         FROM ticket_dependencies d
         JOIN tickets tf ON tf.id = d.from_ticket_id
         JOIN tickets tt ON tt.id = d.to_ticket_id
         WHERE d.dep_type = ?1",
        rusqlite::params![dep_type],
        |row| {
            let from_id: String = row.get(0)?;
            let to_id: String = row.get(1)?;
            let from_ticket = map_ticket_row_at(row, FROM_OFFSET)?;
            let to_ticket = map_ticket_row_at(row, TO_OFFSET)?;
            Ok((from_id, to_id, from_ticket, to_ticket))
        },
    )
}

/// Like `map_ticket_row` but reads ticket fields starting at the given column `offset`.
/// Used when ticket columns are preceded by other fields (e.g. join key columns).
fn map_ticket_row_at(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<Ticket> {
    Ok(Ticket {
        id: row.get(offset)?,
        repo_id: row.get(offset + 1)?,
        source_type: row.get(offset + 2)?,
        source_id: row.get(offset + 3)?,
        title: row.get(offset + 4)?,
        body: row.get(offset + 5)?,
        state: row.get(offset + 6)?,
        labels: row.get(offset + 7)?,
        assignee: row.get(offset + 8)?,
        priority: row.get(offset + 9)?,
        url: row.get(offset + 10)?,
        synced_at: row.get(offset + 11)?,
        raw_json: row.get(offset + 12)?,
        workflow: row.get(offset + 13)?,
        agent_map: row.get(offset + 14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        crate::test_helpers::setup_db()
    }

    fn make_ticket(source_id: &str, title: &str) -> TicketInput {
        TicketInput {
            source_type: "github".to_string(),
            source_id: source_id.to_string(),
            title: title.to_string(),
            body: String::new(),
            state: "open".to_string(),
            labels: vec![],
            assignee: None,
            priority: None,
            url: String::new(),
            raw_json: None,
            label_details: vec![],
            blocked_by: vec![],
            children: vec![],
            parent: None,
        }
    }

    fn get_ticket_state(conn: &Connection, source_id: &str) -> String {
        conn.query_row(
            "SELECT state FROM tickets WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn make_ticket_stub(state: &str) -> Ticket {
        Ticket {
            id: "stub".to_string(),
            repo_id: "repo".to_string(),
            source_type: "github".to_string(),
            source_id: "1".to_string(),
            title: "stub".to_string(),
            body: String::new(),
            state: state.to_string(),
            labels: String::new(),
            assignee: None,
            priority: None,
            url: String::new(),
            synced_at: String::new(),
            raw_json: "{}".to_string(),
            workflow: None,
            agent_map: None,
        }
    }

    #[test]
    fn test_is_actively_blocked_empty() {
        let deps = TicketDependencies::default();
        assert!(!deps.is_actively_blocked());
    }

    #[test]
    fn test_is_actively_blocked_all_closed() {
        let deps = TicketDependencies {
            blocked_by: vec![make_ticket_stub("closed"), make_ticket_stub("closed")],
            ..Default::default()
        };
        assert!(!deps.is_actively_blocked());
    }

    #[test]
    fn test_is_actively_blocked_one_open() {
        let deps = TicketDependencies {
            blocked_by: vec![make_ticket_stub("closed"), make_ticket_stub("open")],
            ..Default::default()
        };
        assert!(deps.is_actively_blocked());
    }

    #[test]
    fn test_active_blockers_empty() {
        let deps = TicketDependencies::default();
        assert_eq!(deps.active_blockers().count(), 0);
    }

    #[test]
    fn test_active_blockers_filters_closed() {
        let deps = TicketDependencies {
            blocked_by: vec![
                make_ticket_stub("closed"),
                make_ticket_stub("open"),
                make_ticket_stub("open"),
            ],
            ..Default::default()
        };
        let active: Vec<_> = deps.active_blockers().collect();
        assert_eq!(active.len(), 2);
        assert!(active.iter().all(|b| b.state == "open"));
    }

    #[test]
    fn test_active_blockers_all_closed() {
        let deps = TicketDependencies {
            blocked_by: vec![make_ticket_stub("closed")],
            ..Default::default()
        };
        assert_eq!(deps.active_blockers().count(), 0);
    }

    #[test]
    fn test_latest_synced_at_no_tickets() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let result = syncer.latest_synced_at("r1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_latest_synced_at_returns_most_recent() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Insert first ticket, then manually backdate its synced_at.
        syncer
            .upsert_tickets("r1", &[make_ticket("1", "Issue 1")])
            .unwrap();
        let old_ts = "2020-01-01T00:00:00Z";
        conn.execute(
            "UPDATE tickets SET synced_at = ?1 WHERE source_id = '1'",
            rusqlite::params![old_ts],
        )
        .unwrap();

        // Insert a second ticket — it gets the current timestamp.
        syncer
            .upsert_tickets("r1", &[make_ticket("2", "Issue 2")])
            .unwrap();

        let latest = syncer.latest_synced_at("r1").unwrap().unwrap();
        // The MAX must be the newer ticket's timestamp, not the backdated one.
        assert_ne!(
            latest, old_ts,
            "MAX should return the most recent timestamp"
        );
        assert!(
            latest.as_str() > old_ts,
            "latest synced_at should be after the backdated timestamp"
        );
    }

    #[test]
    fn test_latest_synced_at_scoped_to_repo() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        syncer
            .upsert_tickets("r1", &[make_ticket("1", "Issue 1")])
            .unwrap();

        // Different repo has no tickets
        let ts = syncer.latest_synced_at("other-repo").unwrap();
        assert!(ts.is_none());
    }

    #[test]
    fn test_sync_and_close_tickets_returns_counts_and_marks_worktrees() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First sync: two open tickets
        let first = vec![make_ticket("1", "Issue 1"), make_ticket("2", "Issue 2")];
        let (synced, closed) = syncer.sync_and_close_tickets("r1", "github", &first);
        assert_eq!(synced, 2);
        assert_eq!(closed, 0);

        // Get ticket id for issue 1 and link a worktree to it
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        // Second sync: only issue 2 remains open → issue 1 closed
        // The worktree is left active because has_merged_pr returns false in test environments.
        let second = vec![make_ticket("2", "Issue 2")];
        let (synced2, closed2) = syncer.sync_and_close_tickets("r1", "github", &second);
        assert_eq!(synced2, 1);
        assert_eq!(closed2, 1);
        assert_eq!(get_ticket_state(&conn, "1"), "closed");
        // Worktree stays active: PR merge check skips cleanup when gh CLI is unavailable.
        assert_eq!(get_worktree_status(&conn, "wt1"), "active");
    }

    #[test]
    fn test_close_missing_tickets_marks_absent_as_closed() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Sync #1: issues 1, 2, 3 are open
        let tickets = vec![
            make_ticket("1", "Issue 1"),
            make_ticket("2", "Issue 2"),
            make_ticket("3", "Issue 3"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        // Sync #2: only issues 1, 3 are open (issue 2 was closed on GitHub)
        let tickets2 = vec![make_ticket("1", "Issue 1"), make_ticket("3", "Issue 3")];
        let synced_ids: Vec<&str> = tickets2.iter().map(|t| t.source_id.as_str()).collect();
        syncer.upsert_tickets("r1", &tickets2).unwrap();
        let closed = syncer
            .close_missing_tickets("r1", "github", &synced_ids)
            .unwrap();

        assert_eq!(closed, 1);
        assert_eq!(get_ticket_state(&conn, "1"), "open");
        assert_eq!(get_ticket_state(&conn, "2"), "closed");
        assert_eq!(get_ticket_state(&conn, "3"), "open");
    }

    #[test]
    fn test_close_missing_does_not_reclose_already_closed() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Sync #1: issues 1, 2 are open
        let tickets = vec![make_ticket("1", "Issue 1"), make_ticket("2", "Issue 2")];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        // Sync #2: only issue 1 open → issue 2 closed
        let synced_ids = vec!["1"];
        syncer
            .close_missing_tickets("r1", "github", &synced_ids)
            .unwrap();
        assert_eq!(get_ticket_state(&conn, "2"), "closed");

        // Sync #3: still only issue 1 open → issue 2 already closed, count should be 0
        let closed = syncer
            .close_missing_tickets("r1", "github", &synced_ids)
            .unwrap();
        assert_eq!(closed, 0);
    }

    #[test]
    fn test_close_missing_empty_sync_is_noop() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Sync existing tickets
        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        // Empty sync should not close anything (protects against API failures)
        let closed = syncer.close_missing_tickets("r1", "github", &[]).unwrap();
        assert_eq!(closed, 0);
        assert_eq!(get_ticket_state(&conn, "1"), "open");
    }

    #[test]
    fn test_close_missing_scoped_to_repo_and_source_type() {
        let conn = setup_db();
        // Add a second repo
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let syncer = TicketSyncer::new(&conn);

        // Both repos have issue #1
        let tickets1 = vec![make_ticket("1", "Repo1 Issue")];
        let tickets2 = vec![make_ticket("1", "Repo2 Issue")];
        syncer.upsert_tickets("r1", &tickets1).unwrap();
        syncer.upsert_tickets("repo2", &tickets2).unwrap();

        // Sync repo1 with no open issues → only repo1's ticket should close
        let closed = syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();
        assert_eq!(closed, 1);

        // repo1's ticket should be closed
        let repo1_state: String = conn
            .query_row(
                "SELECT state FROM tickets WHERE repo_id = 'r1' AND source_id = '1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repo1_state, "closed");

        // repo2's ticket should still be open (different repo, unaffected)
        let repo2_state: String = conn
            .query_row(
                "SELECT state FROM tickets WHERE repo_id = 'repo2' AND source_id = '1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repo2_state, "open");
    }

    fn insert_worktree(
        conn: &Connection,
        id: &str,
        repo_id: &str,
        ticket_id: Option<&str>,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                repo_id,
                format!("wt-{id}"),
                format!("feat/{id}"),
                format!("/tmp/wt-{id}"),
                ticket_id,
                status,
                "2024-01-01T00:00:00Z",
            ],
        )
        .unwrap();
    }

    fn get_worktree_status(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT status FROM worktrees WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_mark_worktrees_active_to_merged() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| true)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
    }

    #[test]
    fn test_mark_worktrees_abandoned_to_merged() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "abandoned");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| true)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
    }

    #[test]
    fn test_mark_worktrees_skips_unlinked() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        insert_worktree(&conn, "wt1", "r1", None, "active");

        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
        assert_eq!(count, 0);
        assert_eq!(get_worktree_status(&conn, "wt1"), "active");
    }

    #[test]
    fn test_mark_worktrees_skips_open_ticket() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Insert an open ticket and link a worktree to it
        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        // Do NOT close the ticket — it stays open
        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
        assert_eq!(count, 0);
        assert_eq!(get_worktree_status(&conn, "wt1"), "active");
    }

    #[test]
    fn test_mark_worktrees_artifacts_query_returns_correct_paths() {
        // Verify the artifact-collection JOIN query (CLOSED_TICKET_ARTIFACTS_SQL)
        // returns the expected (local_path, worktree_path, branch) for a closed-ticket worktree.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        // Use the same constant the implementation uses so this test stays in sync.
        let artifacts: Vec<(String, String, String, String)> = conn
            .prepare(CLOSED_TICKET_ARTIFACTS_SQL)
            .unwrap()
            .query_map(rusqlite::params!["r1"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].0, "/tmp/repo"); // repo local_path from setup_db
        assert_eq!(artifacts[0].1, "/tmp/wt-wt1"); // worktree path from insert_worktree
        assert_eq!(artifacts[0].2, "feat/wt1"); // branch from insert_worktree
    }

    #[test]
    fn test_mark_worktrees_artifacts_skips_already_merged() {
        // mark_worktrees_for_closed_tickets must not attempt artifact cleanup for
        // worktrees whose status is already 'merged' (verified via CLOSED_TICKET_ARTIFACTS_SQL).
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "merged");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        // Use the same constant the implementation uses so this test stays in sync.
        let artifacts: Vec<(String, String, String, String)> = conn
            .prepare(CLOSED_TICKET_ARTIFACTS_SQL)
            .unwrap()
            .query_map(rusqlite::params!["r1"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        assert_eq!(artifacts.len(), 0);
    }

    #[test]
    fn test_mark_worktrees_for_closed_tickets_end_to_end() {
        // Verify that mark_worktrees_for_closed_tickets completes successfully
        // in the closed-ticket scenario, updating DB state and exercising the
        // artifact-cleanup loop (remove_git_artifacts is best-effort and no-ops
        // on non-existent paths, so this is safe in tests).
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| true)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
    }

    #[test]
    fn test_mark_worktrees_sets_completed_at() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");

        // Verify completed_at starts as NULL
        let before: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(before.is_none());

        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();
        syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| true)
            .unwrap();

        let after: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM worktrees WHERE id = 'wt1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            after.is_some(),
            "completed_at must be set when marking as merged"
        );
    }

    #[test]
    fn test_mark_worktrees_idempotent() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "merged");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer.mark_worktrees_for_closed_tickets("r1").unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_mark_worktrees_scoped_to_repo() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let syncer = TicketSyncer::new(&conn);

        let t1 = vec![make_ticket("1", "Repo1 Issue")];
        let t2 = vec![make_ticket("1", "Repo2 Issue")];
        syncer.upsert_tickets("r1", &t1).unwrap();
        syncer.upsert_tickets("repo2", &t2).unwrap();

        let tid1: String = conn
            .query_row("SELECT id FROM tickets WHERE repo_id = 'r1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let tid2: String = conn
            .query_row(
                "SELECT id FROM tickets WHERE repo_id = 'repo2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&tid1), "active");
        insert_worktree(&conn, "wt2", "repo2", Some(&tid2), "active");

        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();
        syncer
            .close_missing_tickets("repo2", "github", &["999"])
            .unwrap();

        let count = syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| true)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
        assert_eq!(get_worktree_status(&conn, "wt2"), "active");
    }

    #[test]
    fn test_mark_worktrees_skips_unmerged_pr() {
        // When the merge check returns false, a closed ticket's worktree must not be touched.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| false)
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(get_worktree_status(&conn, "wt1"), "active");
    }

    #[test]
    fn test_mark_worktrees_removes_when_pr_merged() {
        // When the merge check returns true, a closed ticket's worktree must be updated.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&ticket_id), "active");
        syncer
            .close_missing_tickets("r1", "github", &["999"])
            .unwrap();

        let count = syncer
            .mark_worktrees_for_closed_tickets_with_merge_check("r1", |_, _| true)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(get_worktree_status(&conn, "wt1"), "merged");
    }

    #[test]
    fn test_link_to_worktree_success() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", None, "active");

        syncer.link_to_worktree(&ticket_id, "wt1").unwrap();

        let linked: Option<String> = conn
            .query_row(
                "SELECT ticket_id FROM worktrees WHERE id = 'wt1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(linked, Some(ticket_id));
    }

    #[test]
    fn test_link_to_worktree_rejects_if_already_linked() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let tickets = vec![make_ticket("1", "Issue 1"), make_ticket("2", "Issue 2")];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        let tid1: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let tid2: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
                row.get(0)
            })
            .unwrap();
        insert_worktree(&conn, "wt1", "r1", Some(&tid1), "active");

        let result = syncer.link_to_worktree(&tid2, "wt1");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already has a linked ticket"));
    }

    #[test]
    fn test_get_by_id_success() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let tickets = vec![make_ticket("1", "Issue 1")];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let ticket = syncer.get_by_id(&ticket_id).unwrap();
        assert_eq!(ticket.source_id, "1");
        assert_eq!(ticket.title, "Issue 1");
    }

    #[test]
    fn test_get_by_id_not_found() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let result = syncer.get_by_id("nonexistent-id");
        assert!(result.is_err());
    }

    #[test]
    fn test_upsert_tickets_stores_label_details() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let mut ticket = make_ticket("1", "Issue 1");
        ticket.label_details = vec![
            TicketLabelInput {
                name: "bug".to_string(),
                color: Some("d73a4a".to_string()),
            },
            TicketLabelInput {
                name: "enhancement".to_string(),
                color: None,
            },
        ];
        ticket.labels = vec!["bug".to_string(), "enhancement".to_string()];
        syncer.upsert_tickets("r1", &[ticket]).unwrap();

        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let labels = syncer.get_labels(&ticket_id).unwrap();
        assert_eq!(labels.len(), 2);
        let bug = labels.iter().find(|l| l.label == "bug").unwrap();
        assert_eq!(bug.color, Some("d73a4a".to_string()));
        let enh = labels.iter().find(|l| l.label == "enhancement").unwrap();
        assert_eq!(enh.color, None);
    }

    #[test]
    fn test_resync_removes_stale_labels() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First sync: bug + enhancement
        let mut ticket = make_ticket("1", "Issue 1");
        ticket.label_details = vec![
            TicketLabelInput {
                name: "bug".to_string(),
                color: Some("d73a4a".to_string()),
            },
            TicketLabelInput {
                name: "enhancement".to_string(),
                color: None,
            },
        ];
        syncer.upsert_tickets("r1", &[ticket]).unwrap();

        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(syncer.get_labels(&ticket_id).unwrap().len(), 2);

        // Second sync: only bug remains
        let mut ticket2 = make_ticket("1", "Issue 1");
        ticket2.label_details = vec![TicketLabelInput {
            name: "bug".to_string(),
            color: Some("d73a4a".to_string()),
        }];
        syncer.upsert_tickets("r1", &[ticket2]).unwrap();

        let labels = syncer.get_labels(&ticket_id).unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].label, "bug");
    }

    #[test]
    fn test_resync_adds_new_labels() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First sync: no labels
        let ticket = make_ticket("1", "Issue 1");
        syncer.upsert_tickets("r1", &[ticket]).unwrap();

        let ticket_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(syncer.get_labels(&ticket_id).unwrap().len(), 0);

        // Second sync: add a label
        let mut ticket2 = make_ticket("1", "Issue 1");
        ticket2.label_details = vec![TicketLabelInput {
            name: "wontfix".to_string(),
            color: Some("ffffff".to_string()),
        }];
        syncer.upsert_tickets("r1", &[ticket2]).unwrap();

        let labels = syncer.get_labels(&ticket_id).unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].label, "wontfix");
    }

    #[test]
    fn test_build_agent_prompt_full_ticket() {
        let ticket = Ticket {
            id: "01ABCDEF".to_string(),
            repo_id: "r1".to_string(),
            source_type: "github".to_string(),
            source_id: "42".to_string(),
            title: "Add dark mode support".to_string(),
            body: "We need dark mode for the settings page.".to_string(),
            state: "open".to_string(),
            labels: "enhancement, ui".to_string(),
            assignee: Some("dev1".to_string()),
            priority: None,
            url: "https://github.com/org/repo/issues/42".to_string(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            raw_json: "{}".to_string(),
            workflow: None,
            agent_map: None,
        };

        let prompt = build_agent_prompt(&ticket);
        assert!(prompt.contains("Issue: #42 — Add dark mode support"));
        assert!(prompt.contains("State: open"));
        assert!(prompt.contains("Labels: enhancement, ui"));
        assert!(prompt.contains("We need dark mode for the settings page."));
        assert!(prompt.contains("Implement the changes described in the issue."));
    }

    #[test]
    fn test_build_agent_prompt_empty_body_and_labels() {
        let ticket = Ticket {
            id: "01ABCDEF".to_string(),
            repo_id: "r1".to_string(),
            source_type: "github".to_string(),
            source_id: "7".to_string(),
            title: "Fix typo".to_string(),
            body: String::new(),
            state: "open".to_string(),
            labels: "[]".to_string(),
            assignee: None,
            priority: None,
            url: String::new(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            raw_json: "{}".to_string(),
            workflow: None,
            agent_map: None,
        };

        let prompt = build_agent_prompt(&ticket);
        assert!(prompt.contains("Issue: #7 — Fix typo"));
        assert!(prompt.contains("Labels: None"));
        assert!(prompt.contains("(No description provided)"));
    }

    /// Verify that `TicketSyncer::list` returns all tickets regardless of state,
    /// including closed ones. The display-layer filtering (hide closed by default)
    /// is intentionally done in the TUI / web route, not in core.
    #[test]
    fn test_list_includes_closed_tickets() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Upsert two tickets: one open, one that will be closed
        let tickets = vec![
            make_ticket("10", "Open issue"),
            make_ticket("11", "Soon closed"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        // Close ticket 11
        syncer
            .close_missing_tickets("r1", "github", &["10"])
            .unwrap();

        let all = syncer.list(None).unwrap();
        assert_eq!(
            all.len(),
            2,
            "list() must return all tickets including closed"
        );

        let states: Vec<&str> = all.iter().map(|t| t.state.as_str()).collect();
        assert!(states.contains(&"open"), "open ticket must be present");
        assert!(states.contains(&"closed"), "closed ticket must be present");

        // Simulate the web-route filter (show_closed=false)
        let visible: Vec<_> = all.iter().filter(|t| t.state != "closed").collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].source_id, "10");
    }

    #[test]
    fn test_get_all_labels_groups_by_ticket_id() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Two tickets, first with two labels, second with one label, third with none.
        let mut t1 = make_ticket("1", "Issue 1");
        t1.label_details = vec![
            TicketLabelInput {
                name: "bug".to_string(),
                color: Some("d73a4a".to_string()),
            },
            TicketLabelInput {
                name: "enhancement".to_string(),
                color: None,
            },
        ];
        let mut t2 = make_ticket("2", "Issue 2");
        t2.label_details = vec![TicketLabelInput {
            name: "docs".to_string(),
            color: Some("0075ca".to_string()),
        }];
        let t3 = make_ticket("3", "Issue 3"); // no labels

        syncer.upsert_tickets("r1", &[t1, t2, t3]).unwrap();

        let tid1: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let tid2: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let tid3: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '3'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let map = syncer.get_all_labels().unwrap();

        // ticket 1: two labels
        let lbls1 = map.get(&tid1).expect("ticket 1 must have labels");
        assert_eq!(lbls1.len(), 2);
        assert!(lbls1
            .iter()
            .any(|l| l.label == "bug" && l.color == Some("d73a4a".to_string())));
        assert!(lbls1
            .iter()
            .any(|l| l.label == "enhancement" && l.color.is_none()));

        // ticket 2: one label
        let lbls2 = map.get(&tid2).expect("ticket 2 must have labels");
        assert_eq!(lbls2.len(), 1);
        assert_eq!(lbls2[0].label, "docs");
        assert_eq!(lbls2[0].color, Some("0075ca".to_string()));

        // ticket 3: no entry in the map
        assert!(
            !map.contains_key(&tid3),
            "ticket with no labels must not appear in the map"
        );
    }

    #[test]
    fn test_get_all_labels_empty_db() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        let map = syncer.get_all_labels().unwrap();
        assert!(map.is_empty(), "empty DB must yield empty label map");
    }

    // -----------------------------------------------------------------------
    // list_filtered tests
    // -----------------------------------------------------------------------

    fn make_ticket_with_body(source_id: &str, title: &str, body: &str) -> TicketInput {
        TicketInput {
            source_type: "github".to_string(),
            source_id: source_id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            state: "open".to_string(),
            labels: vec![],
            assignee: None,
            priority: None,
            url: String::new(),
            raw_json: None,
            label_details: vec![],
            blocked_by: vec![],
            children: vec![],
            parent: None,
        }
    }

    #[test]
    fn test_list_filtered_defaults_to_open_only() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![
            make_ticket("1", "Open issue"),
            make_ticket("2", "Closed issue"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        syncer
            .close_missing_tickets("r1", "github", &["1"])
            .unwrap();

        let filter = TicketFilter {
            labels: vec![],
            search: None,
            include_closed: false,
        };
        let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "1");
    }

    #[test]
    fn test_list_filtered_include_closed() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![
            make_ticket("1", "Open issue"),
            make_ticket("2", "Closed issue"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();
        syncer
            .close_missing_tickets("r1", "github", &["1"])
            .unwrap();

        let filter = TicketFilter {
            labels: vec![],
            search: None,
            include_closed: true,
        };
        let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_list_filtered_by_label() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let mut t1 = make_ticket("1", "Bug report");
        t1.label_details = vec![TicketLabelInput {
            name: "bug".to_string(),
            color: None,
        }];
        let t2 = make_ticket("2", "Feature request"); // no labels

        syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

        let filter = TicketFilter {
            labels: vec!["bug".to_string()],
            search: None,
            include_closed: false,
        };
        let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "1");
    }

    #[test]
    fn test_list_filtered_by_multiple_labels_and_semantics() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // t1 has both "bug" and "urgent"
        let mut t1 = make_ticket("1", "Critical bug");
        t1.label_details = vec![
            TicketLabelInput {
                name: "bug".to_string(),
                color: None,
            },
            TicketLabelInput {
                name: "urgent".to_string(),
                color: None,
            },
        ];
        // t2 has only "bug"
        let mut t2 = make_ticket("2", "Normal bug");
        t2.label_details = vec![TicketLabelInput {
            name: "bug".to_string(),
            color: None,
        }];

        syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

        // Filtering for both labels should return only t1 (AND semantics)
        let filter = TicketFilter {
            labels: vec!["bug".to_string(), "urgent".to_string()],
            search: None,
            include_closed: false,
        };
        let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "1");
    }

    #[test]
    fn test_list_filtered_by_search_title() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        syncer
            .upsert_tickets(
                "r1",
                &[
                    make_ticket_with_body("1", "Fix the login page", ""),
                    make_ticket_with_body("2", "Update dashboard", ""),
                ],
            )
            .unwrap();

        let filter = TicketFilter {
            labels: vec![],
            search: Some("login".to_string()),
            include_closed: false,
        };
        let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "1");
    }

    #[test]
    fn test_list_filtered_by_search_body() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        syncer
            .upsert_tickets(
                "r1",
                &[
                    make_ticket_with_body("1", "Issue A", "contains the keyword xyz"),
                    make_ticket_with_body("2", "Issue B", "nothing relevant"),
                ],
            )
            .unwrap();

        let filter = TicketFilter {
            labels: vec![],
            search: Some("xyz".to_string()),
            include_closed: false,
        };
        let results = syncer.list_filtered(Some("r1"), &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_id, "1");
    }

    #[test]
    fn test_list_filtered_no_repo_scope() {
        let conn = setup_db();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('repo2', 'other-repo', '/tmp/repo2', 'https://github.com/test/other', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let syncer = TicketSyncer::new(&conn);
        syncer
            .upsert_tickets("r1", &[make_ticket("1", "Repo1 issue")])
            .unwrap();
        syncer
            .upsert_tickets("repo2", &[make_ticket("2", "Repo2 issue")])
            .unwrap();

        let filter = TicketFilter {
            labels: vec![],
            search: None,
            include_closed: false,
        };
        let results = syncer.list_filtered(None, &filter).unwrap();
        assert_eq!(results.len(), 2);
    }

    // --- resolve_ticket_id tests ---

    fn make_repo() -> crate::repo::Repo {
        crate::repo::Repo {
            id: "r1".to_string(),
            slug: "test-repo".to_string(),
            local_path: "/tmp/repo".to_string(),
            remote_url: "https://github.com/test/repo.git".to_string(),
            default_branch: "main".to_string(),
            workspace_dir: "/tmp/ws".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            model: None,
            allow_agent_issue_creation: false,
        }
    }

    #[test]
    fn test_resolve_ticket_id_by_source_id() {
        let conn = setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        let wt_mgr = crate::worktree::WorktreeManager::new(&conn, &config);
        let repo = make_repo();

        syncer
            .upsert_tickets("r1", &[make_ticket("42", "Issue 42")])
            .unwrap();

        let (source_type, source_id) = syncer.resolve_ticket_id(&wt_mgr, &repo, "42").unwrap();
        assert_eq!(source_type, "github");
        assert_eq!(source_id, "42");
    }

    #[test]
    fn test_resolve_ticket_id_by_ulid() {
        let conn = setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        let wt_mgr = crate::worktree::WorktreeManager::new(&conn, &config);
        let repo = make_repo();

        syncer
            .upsert_tickets("r1", &[make_ticket("99", "Issue 99")])
            .unwrap();
        let ulid: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '99'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let (source_type, source_id) = syncer.resolve_ticket_id(&wt_mgr, &repo, &ulid).unwrap();
        assert_eq!(source_type, "github");
        assert_eq!(source_id, "99");
    }

    #[test]
    fn test_resolve_ticket_id_ulid_not_found_falls_through() {
        let conn = setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        let wt_mgr = crate::worktree::WorktreeManager::new(&conn, &config);
        let repo = make_repo();

        // Insert a ticket with source_id that is exactly 26 chars (ULID-length)
        // but is NOT a valid internal ULID — should fall through to source_id lookup.
        let fake_ulid = "01ABCDEFGHJKMNPQRSTVWXYZ99";
        assert_eq!(fake_ulid.len(), 26);
        syncer
            .upsert_tickets(
                "r1",
                &[make_ticket(fake_ulid, "Issue with ULID-like source_id")],
            )
            .unwrap();

        let (source_type, source_id) = syncer.resolve_ticket_id(&wt_mgr, &repo, fake_ulid).unwrap();
        assert_eq!(source_type, "github");
        assert_eq!(source_id, fake_ulid);
    }

    #[test]
    fn test_resolve_ticket_id_not_found() {
        let conn = setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        let wt_mgr = crate::worktree::WorktreeManager::new(&conn, &config);
        let repo = make_repo();

        let result = syncer.resolve_ticket_id(&wt_mgr, &repo, "nonexistent");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConductorError::TicketNotFound { .. }
        ));
    }

    #[test]
    fn test_list_sorts_by_issue_number_descending() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Insert tickets with numeric source_ids in non-sequential order
        let tickets = vec![
            make_ticket("5", "Issue 5"),
            make_ticket("123", "Issue 123"),
            make_ticket("1", "Issue 1"),
            make_ticket("42", "Issue 42"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        let result = syncer.list(Some("r1")).unwrap();
        let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
        assert_eq!(ids, vec!["123", "42", "5", "1"]);
    }

    #[test]
    fn test_list_filtered_sorts_by_issue_number_descending() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![
            make_ticket("10", "Issue 10"),
            make_ticket("200", "Issue 200"),
            make_ticket("3", "Issue 3"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        let filter = TicketFilter {
            labels: vec![],
            search: None,
            include_closed: false,
        };
        let result = syncer.list_filtered(Some("r1"), &filter).unwrap();
        let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
        assert_eq!(ids, vec!["200", "10", "3"]);
    }

    #[test]
    fn test_list_all_repos_sorts_by_issue_number_descending() {
        let conn = setup_db();
        // Register a second repo so we can test cross-repo listing
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r2', 'test-repo-2', '/tmp/repo2', 'https://github.com/test/repo2.git', '/tmp/ws2', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        let syncer = TicketSyncer::new(&conn);

        // Insert tickets across two different repos with interleaved source_ids
        let repo1_tickets = vec![
            make_ticket("10", "Repo1 Issue 10"),
            make_ticket("50", "Repo1 Issue 50"),
        ];
        let repo2_tickets = vec![
            make_ticket("25", "Repo2 Issue 25"),
            make_ticket("100", "Repo2 Issue 100"),
        ];
        syncer.upsert_tickets("r1", &repo1_tickets).unwrap();
        syncer.upsert_tickets("r2", &repo2_tickets).unwrap();

        // list(None) should return all tickets sorted by issue number descending
        let result = syncer.list(None).unwrap();
        let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
        assert_eq!(ids, vec!["100", "50", "25", "10"]);
    }

    #[test]
    fn test_list_sorts_non_numeric_source_ids_to_end() {
        // Non-numeric source_ids (e.g. Jira keys) CAST to 0, so they sort
        // after all numeric IDs. Among themselves, they fall back to the
        // secondary `source_id DESC` (string) sort.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets = vec![
            make_ticket("PROJ-10", "Jira ticket 10"),
            make_ticket("5", "GitHub issue 5"),
            make_ticket("PROJ-3", "Jira ticket 3"),
            make_ticket("100", "GitHub issue 100"),
        ];
        syncer.upsert_tickets("r1", &tickets).unwrap();

        let result = syncer.list(Some("r1")).unwrap();
        let ids: Vec<&str> = result.iter().map(|t| t.source_id.as_str()).collect();
        // Numeric IDs first (descending), then non-numeric (string descending)
        assert_eq!(ids, vec!["100", "5", "PROJ-3", "PROJ-10"]);
    }

    // -----------------------------------------------------------------------
    // ticket_dependencies tests
    // -----------------------------------------------------------------------

    fn dep_row(conn: &Connection) -> Option<(String, String, String)> {
        conn.query_row(
            "SELECT from_ticket_id, to_ticket_id, dep_type FROM ticket_dependencies LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
    }

    fn dep_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM ticket_dependencies", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn test_upsert_blocked_by_writes_dependency() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Ticket "2" blocks ticket "1"
        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];

        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

        let id1: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let id2: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let row = dep_row(&conn).expect("expected one dependency row");
        assert_eq!(row.0, id2, "from_ticket_id should be the blocker (2)");
        assert_eq!(row.1, id1, "to_ticket_id should be the blocked ticket (1)");
        assert_eq!(row.2, "blocks");
    }

    #[test]
    fn test_upsert_children_writes_dependency() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Ticket "1" is parent of ticket "2"
        let t2 = make_ticket("2", "Child");
        let mut t1 = make_ticket("1", "Parent");
        t1.children = vec!["2".to_string()];

        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

        let id1: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let id2: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '2'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let row = dep_row(&conn).expect("expected one dependency row");
        assert_eq!(row.0, id1, "from_ticket_id should be the parent (1)");
        assert_eq!(row.1, id2, "to_ticket_id should be the child (2)");
        assert_eq!(row.2, "parent_of");
    }

    #[test]
    fn test_upsert_empty_blocked_by_preserves_existing_deps() {
        // Empty blocked_by is treated as "no opinion" — it must NOT clear deps
        // written by a previous upsert or by another source (e.g. MCP).
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First upsert: ticket "1" is blocked by ticket "2"
        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
        assert_eq!(dep_count(&conn), 1);

        // Re-upsert ticket "1" with empty blocked_by (e.g. from a GitHub sync
        // that doesn't parse body text) — existing dep row must be preserved.
        let t1_no_opinion = make_ticket("1", "Blocked");
        syncer.upsert_tickets("r1", &[t1_no_opinion]).unwrap();
        assert_eq!(
            dep_count(&conn),
            1,
            "empty blocked_by should not remove existing dependency rows"
        );
    }

    #[test]
    fn test_upsert_unknown_source_id_skipped() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let mut t1 = make_ticket("1", "Ticket");
        t1.blocked_by = vec!["nonexistent".to_string()];

        // Should not panic; unresolvable source IDs are silently skipped
        syncer.upsert_tickets("r1", &[t1]).unwrap();
        assert_eq!(
            dep_count(&conn),
            0,
            "unresolvable source_id should produce no row"
        );
    }

    #[test]
    fn test_upsert_dependency_idempotent() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Upsert the same batch twice — each time re-construct the inputs
        for _ in 0..2 {
            let t2 = make_ticket("2", "Blocker");
            let mut t1 = make_ticket("1", "Blocked");
            t1.blocked_by = vec!["2".to_string()];
            syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
        }

        assert_eq!(
            dep_count(&conn),
            1,
            "second upsert should not duplicate the dependency row"
        );
    }

    #[test]
    fn test_upsert_empty_children_preserves_existing_deps() {
        // Empty children is treated as "no opinion" — it must NOT clear deps
        // written by a previous upsert or by another source (e.g. MCP).
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First upsert: ticket "1" is parent of ticket "2"
        let t2 = make_ticket("2", "Child");
        let mut t1 = make_ticket("1", "Parent");
        t1.children = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
        assert_eq!(dep_count(&conn), 1);

        // Re-upsert ticket "1" with empty children (e.g. from a GitHub sync
        // that doesn't parse body text) — existing dep row must be preserved.
        let t1_no_opinion = make_ticket("1", "Parent");
        syncer.upsert_tickets("r1", &[t1_no_opinion]).unwrap();
        assert_eq!(
            dep_count(&conn),
            1,
            "empty children should not remove existing dependency rows"
        );
    }

    #[test]
    fn test_upsert_only_parent_preserves_blocked_by_and_children() {
        // Setting only `parent` must not clear existing `blocked_by` or `children`
        // relationships — the guard must be per-field, not shared.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First upsert: ticket "1" is blocked by "2" and is parent of "3"
        let t2 = make_ticket("2", "Blocker");
        let t3 = make_ticket("3", "Child");
        let mut t1 = make_ticket("1", "Middle");
        t1.blocked_by = vec!["2".to_string()];
        t1.children = vec!["3".to_string()];
        syncer.upsert_tickets("r1", &[t2, t3, t1]).unwrap();
        // 1 blocks row + 1 parent_of row
        assert_eq!(dep_count(&conn), 2);

        // Insert a parent ticket "0"
        let t0 = make_ticket("0", "GrandParent");
        syncer.upsert_tickets("r1", &[t0]).unwrap();

        // Second upsert: ticket "1" with only parent set, blocked_by and children are empty
        let mut t1_parent_only = make_ticket("1", "Middle");
        t1_parent_only.parent = Some("0".to_string());
        syncer.upsert_tickets("r1", &[t1_parent_only]).unwrap();

        // Should now have 3 rows: the original blocks + parent_of(1→3) + new parent_of(0→1)
        assert_eq!(
            dep_count(&conn),
            3,
            "setting only parent must not wipe existing blocked_by or children rows"
        );
    }

    #[test]
    fn test_upsert_only_blocked_by_preserves_parent_of() {
        // Setting only `blocked_by` must not clear existing `children` (parent_of) rows.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First upsert: ticket "1" is parent of ticket "2"
        let t2 = make_ticket("2", "Child");
        let mut t1 = make_ticket("1", "Parent");
        t1.children = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
        assert_eq!(dep_count(&conn), 1, "should have 1 parent_of row");

        // Insert a blocker ticket "3"
        let t3 = make_ticket("3", "Blocker");
        syncer.upsert_tickets("r1", &[t3]).unwrap();

        // Re-upsert ticket "1" with only blocked_by set, children empty
        let mut t1_blocked_only = make_ticket("1", "Parent");
        t1_blocked_only.blocked_by = vec!["3".to_string()];
        syncer.upsert_tickets("r1", &[t1_blocked_only]).unwrap();

        // Should now have 2 rows: original parent_of(1→2) + new blocks(1←3)
        assert_eq!(
            dep_count(&conn),
            2,
            "setting only blocked_by must not wipe existing parent_of (children) rows"
        );
    }

    #[test]
    fn test_upsert_only_children_preserves_blocked_by() {
        // Setting only `children` must not clear existing `blocked_by` (blocks) rows.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // First upsert: ticket "1" is blocked by ticket "2"
        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();
        assert_eq!(dep_count(&conn), 1, "should have 1 blocks row");

        // Insert a child ticket "3"
        let t3 = make_ticket("3", "Child");
        syncer.upsert_tickets("r1", &[t3]).unwrap();

        // Re-upsert ticket "1" with only children set, blocked_by empty
        let mut t1_children_only = make_ticket("1", "Blocked");
        t1_children_only.children = vec!["3".to_string()];
        syncer.upsert_tickets("r1", &[t1_children_only]).unwrap();

        // Should now have 2 rows: original blocks(1←2) + new parent_of(1→3)
        assert_eq!(
            dep_count(&conn),
            2,
            "setting only children must not wipe existing blocked_by (blocks) rows"
        );
    }

    // -----------------------------------------------------------------------
    // get_ready_tickets tests
    // -----------------------------------------------------------------------

    fn insert_workflow_run_for_ticket(
        conn: &Connection,
        wf_id: &str,
        ticket_id: &str,
        status: &str,
    ) {
        // Insert a minimal agent_run first (parent_run_id FK)
        let ar_id = format!("ar-{wf_id}");
        conn.execute(
            "INSERT OR IGNORE INTO worktrees (id, repo_id, slug, branch, path, created_at) \
             VALUES ('wt-sys', 'r1', 'sys', 'sys', '/tmp/sys', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO agent_runs (id, worktree_id, prompt, status, started_at) \
             VALUES (?1, 'wt-sys', 'test', 'completed', '2024-01-01T00:00:00Z')",
            params![ar_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, parent_run_id, status, started_at, ticket_id, repo_id) \
             VALUES (?1, 'wf', ?2, ?3, '2024-01-01T00:00:00Z', ?4, 'r1')",
            params![wf_id, ar_id, status, ticket_id],
        )
        .unwrap();
    }

    #[test]
    fn test_get_ready_tickets_no_deps_all_open_ready() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        syncer
            .upsert_tickets("r1", &[make_ticket("1", "A"), make_ticket("2", "B")])
            .unwrap();

        let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
        assert_eq!(ready.len(), 2);
        assert!(ready.iter().all(|t| t.dep_type.is_none()));
    }

    #[test]
    fn test_get_ready_tickets_blocked_ticket_excluded() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Ticket "2" blocks ticket "1"; blocker is still open → ticket "1" is not ready
        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

        let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].source_id, "2"); // only the blocker itself is ready
    }

    #[test]
    fn test_get_ready_tickets_blocker_closed_makes_blocked_ready() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

        // Close the blocker
        syncer
            .close_missing_tickets("r1", "github", &["1"])
            .unwrap();

        let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
        let ids: Vec<&str> = ready.iter().map(|t| t.source_id.as_str()).collect();
        assert!(
            ids.contains(&"1"),
            "blocked ticket should be ready once blocker is closed"
        );
    }

    #[test]
    fn test_get_ready_tickets_active_run_excluded() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        syncer
            .upsert_tickets("r1", &[make_ticket("1", "A")])
            .unwrap();
        let tid: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();

        // Link an active workflow run to the ticket
        insert_workflow_run_for_ticket(&conn, "wr1", &tid, "running");

        let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
        assert_eq!(ready.len(), 0, "ticket with active run must be excluded");
    }

    #[test]
    fn test_get_ready_tickets_completed_run_not_excluded() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        syncer
            .upsert_tickets("r1", &[make_ticket("1", "A")])
            .unwrap();
        let tid: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();

        // Completed run — should not block the ticket from being ready
        insert_workflow_run_for_ticket(&conn, "wr1", &tid, "completed");

        let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn test_get_ready_tickets_root_ticket_id_scope() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Ticket "1" is parent of "2"; ticket "3" is unrelated
        let t2 = make_ticket("2", "Child");
        let t3 = make_ticket("3", "Unrelated");
        let mut t1 = make_ticket("1", "Parent");
        t1.children = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t3, t1]).unwrap();

        let parent_id: String = conn
            .query_row("SELECT id FROM tickets WHERE source_id = '1'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let ready = syncer
            .get_ready_tickets("r1", Some(&parent_id), None, 50)
            .unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].source_id, "2");
        assert_eq!(ready[0].dep_type.as_deref(), Some("parent_of"));
    }

    #[test]
    fn test_get_ready_tickets_label_scope() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let mut t1 = make_ticket("1", "With label");
        t1.label_details = vec![TicketLabelInput {
            name: "backend".to_string(),
            color: None,
        }];
        let t2 = make_ticket("2", "No label");
        syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

        let ready = syncer
            .get_ready_tickets("r1", None, Some("backend"), 50)
            .unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].source_id, "1");
    }

    #[test]
    fn test_get_ready_tickets_limit_respected() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let tickets: Vec<_> = (1..=5)
            .map(|i| make_ticket(&i.to_string(), &format!("Issue {i}")))
            .collect();
        syncer.upsert_tickets("r1", &tickets).unwrap();

        let ready = syncer.get_ready_tickets("r1", None, None, 3).unwrap();
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn test_get_ready_tickets_coalesce_no_run_as_completed() {
        // A blocker with state='closed' and NO workflow run must be treated as
        // resolved (COALESCE(wr.status, 'completed') = 'completed').
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        let t2 = make_ticket("2", "Blocker");
        let mut t1 = make_ticket("1", "Blocked");
        t1.blocked_by = vec!["2".to_string()];
        syncer.upsert_tickets("r1", &[t2, t1]).unwrap();

        // Close the blocker (no workflow run created)
        syncer
            .close_missing_tickets("r1", "github", &["1"])
            .unwrap();

        let ready = syncer.get_ready_tickets("r1", None, None, 50).unwrap();
        let ids: Vec<&str> = ready.iter().map(|t| t.source_id.as_str()).collect();
        assert!(
            ids.contains(&"1"),
            "blocked ticket should be ready when closed blocker has no run (COALESCE = completed)"
        );
    }

    #[test]
    fn test_blocks_delete_does_not_contaminate_parent_of() {
        // Regression test: stale-clear DELETE for dep_type='blocks' must not remove
        // parent_of rows written by a different ticket during an incremental sync.
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Batch 1: ticket "B" is the parent of ticket "A" → writes parent_of(B→A)
        let ta = make_ticket("A", "Child");
        let mut tb = make_ticket("B", "Parent");
        tb.children = vec!["A".to_string()];
        syncer.upsert_tickets("r1", &[ta, tb]).unwrap();
        assert_eq!(dep_count(&conn), 1, "setup: one parent_of row expected");

        // Batch 2: re-upsert ticket "A" alone with empty blocked_by.
        // The stale-clear for blocks scoped to to_ticket_id=A should NOT remove
        // the parent_of row where A is the child (from_ticket_id=B, to_ticket_id=A).
        let ta_clear = make_ticket("A", "Child");
        syncer.upsert_tickets("r1", &[ta_clear]).unwrap();
        assert_eq!(
            dep_count(&conn),
            1,
            "parent_of row must survive a separate blocked_by clear for the child ticket"
        );
    }

    // ── get_dependencies / get_all_dependencies tests ───────────────────────

    /// Returns the source_ids of a ticket slice for readable assertions.
    fn source_ids(tickets: &[Ticket]) -> Vec<&str> {
        tickets.iter().map(|t| t.source_id.as_str()).collect()
    }

    fn get_ticket_id(conn: &Connection, source_id: &str) -> String {
        conn.query_row(
            "SELECT id FROM tickets WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )
        .expect("ticket not found")
    }

    #[test]
    fn test_get_dependencies_blocked_by_and_blocks() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // ticket "1" blocks ticket "2"
        let t1 = make_ticket("1", "Blocker");
        // no special fields needed — relationship is written via t2.blocked_by
        let mut t2 = make_ticket("2", "Blocked");
        t2.blocked_by = vec!["1".to_string()];

        syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

        let deps = syncer
            .get_dependencies_by_source_id("r1", "1")
            .expect("get_dependencies for ticket 1");

        // Ticket 1 blocks ticket 2
        assert_eq!(
            source_ids(&deps.blocks),
            vec!["2"],
            "ticket 1 should block ticket 2"
        );
        assert!(
            deps.blocked_by.is_empty(),
            "ticket 1 should not be blocked by anything"
        );

        let deps2 = syncer
            .get_dependencies_by_source_id("r1", "2")
            .expect("get_dependencies for ticket 2");

        // Ticket 2 is blocked by ticket 1
        assert_eq!(
            source_ids(&deps2.blocked_by),
            vec!["1"],
            "ticket 2 should be blocked by ticket 1"
        );
        assert!(
            deps2.blocks.is_empty(),
            "ticket 2 should not block anything"
        );
    }

    #[test]
    fn test_get_dependencies_parent_and_children() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // ticket "10" is parent of "11" and "12"
        let child1 = make_ticket("11", "Child 1");
        let child2 = make_ticket("12", "Child 2");
        let mut parent = make_ticket("10", "Parent");
        parent.children = vec!["11".to_string(), "12".to_string()];

        syncer
            .upsert_tickets("r1", &[child1, child2, parent])
            .unwrap();

        let parent_deps = syncer
            .get_dependencies_by_source_id("r1", "10")
            .expect("get_dependencies for parent ticket");

        let mut child_ids = source_ids(&parent_deps.children);
        child_ids.sort();
        assert_eq!(
            child_ids,
            vec!["11", "12"],
            "parent should list both children"
        );
        assert!(
            parent_deps.parent.is_none(),
            "parent ticket has no parent itself"
        );

        let child1_deps = syncer
            .get_dependencies_by_source_id("r1", "11")
            .expect("get_dependencies for child 1");

        assert_eq!(
            child1_deps.parent.as_ref().map(|t| t.source_id.as_str()),
            Some("10"),
            "child 1 should know its parent"
        );
        assert!(child1_deps.children.is_empty(), "child has no children");
    }

    #[test]
    fn test_get_dependencies_empty_when_no_deps() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);
        syncer
            .upsert_tickets("r1", &[make_ticket("99", "Standalone")])
            .unwrap();

        let deps = syncer
            .get_dependencies_by_source_id("r1", "99")
            .expect("get_dependencies for standalone ticket");

        assert!(deps.blocked_by.is_empty());
        assert!(deps.blocks.is_empty());
        assert!(deps.parent.is_none());
        assert!(deps.children.is_empty());
    }

    #[test]
    fn test_get_all_dependencies_maps_both_directions() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // ticket "A" blocks ticket "B"
        let ta = make_ticket("A", "Blocker");
        let mut tb = make_ticket("B", "Blocked");
        tb.blocked_by = vec!["A".to_string()];

        // ticket "P" is parent of "C"
        let tc = make_ticket("C", "Child");
        let mut tp = make_ticket("P", "Parent");
        tp.children = vec!["C".to_string()];

        syncer.upsert_tickets("r1", &[ta, tb, tc, tp]).unwrap();

        let all = syncer.get_all_dependencies().expect("get_all_dependencies");

        // Look up internal IDs via source_id
        let id_a = ticket_id_for_source(&conn, "A");
        let id_b = ticket_id_for_source(&conn, "B");
        let id_p = ticket_id_for_source(&conn, "P");
        let id_c = ticket_id_for_source(&conn, "C");

        let deps_b = all.get(&id_b).expect("entry for ticket B");
        assert_eq!(source_ids(&deps_b.blocked_by), vec!["A"], "B blocked_by A");
        assert!(deps_b.blocks.is_empty(), "B blocks nothing");

        let deps_a = all.get(&id_a).expect("entry for ticket A");
        assert_eq!(source_ids(&deps_a.blocks), vec!["B"], "A blocks B");
        assert!(deps_a.blocked_by.is_empty(), "A is not blocked");

        let deps_c = all.get(&id_c).expect("entry for ticket C");
        assert_eq!(
            deps_c.parent.as_ref().map(|t| t.source_id.as_str()),
            Some("P"),
            "C parent is P"
        );
        assert!(deps_c.children.is_empty());

        let deps_p = all.get(&id_p).expect("entry for ticket P");
        assert_eq!(source_ids(&deps_p.children), vec!["C"], "P children: C");
        assert!(deps_p.parent.is_none());
    }

    /// Helper for dependency tests: look up the internal ULID for a ticket by source_id.
    fn ticket_id_for_source(conn: &Connection, source_id: &str) -> String {
        conn.query_row(
            "SELECT id FROM tickets WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )
        .expect("ticket not found")
    }

    /// Helper: call get_dependencies by source_id (resolves ULID internally).
    impl TicketSyncer<'_> {
        fn get_dependencies_by_source_id(
            &self,
            _repo_id: &str,
            source_id: &str,
        ) -> Result<TicketDependencies> {
            let ticket_id: String = self
                .conn
                .query_row(
                    "SELECT id FROM tickets WHERE source_id = ?1",
                    params![source_id],
                    |row| row.get(0),
                )
                .map_err(ConductorError::Database)?;
            self.get_dependencies(&ticket_id)
        }
    }

    /// If a ticket_dependencies row references a ticket ID that no longer exists in the
    /// tickets table (e.g. deleted after FK was written with constraints off), query_dep_pairs
    /// must return TicketNotFound rather than silently dropping the edge.
    #[test]
    fn test_query_dep_pairs_orphaned_ticket_returns_error() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Insert one real ticket to act as the "from" side.
        syncer
            .upsert_tickets("r1", &[make_ticket("orphan-from", "From Ticket")])
            .unwrap();
        let from_id = get_ticket_id(&conn, "orphan-from");

        // Bypass FK constraints to insert an edge referencing a non-existent to_ticket_id.
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) \
             VALUES (?1, 'nonexistent-ticket-id', 'blocks')",
            rusqlite::params![from_id],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();

        let result = query_dep_pairs(&conn, "blocks");
        assert!(
            result.is_err(),
            "query_dep_pairs must return Err when a referenced ticket is missing"
        );
        match result.unwrap_err() {
            ConductorError::TicketNotFound { id } => {
                assert_eq!(id, "nonexistent-ticket-id");
            }
            e => panic!("expected TicketNotFound, got {e:?}"),
        }
    }

    #[test]
    fn test_upsert_preserves_raw_json_on_cli_re_upsert() {
        let conn = setup_db();
        let syncer = TicketSyncer::new(&conn);

        // Simulate a sync with real raw_json from a source (e.g. GitHub).
        let mut synced = make_ticket("42", "Real Issue");
        synced.raw_json = Some(r#"{"id":42,"number":42,"title":"Real Issue"}"#.to_string());
        syncer.upsert_tickets("r1", &[synced]).unwrap();

        // Verify the raw_json was stored correctly.
        let stored: String = conn
            .query_row(
                "SELECT raw_json FROM tickets WHERE source_id = '42'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, r#"{"id":42,"number":42,"title":"Real Issue"}"#);

        // Simulate a CLI re-upsert (passes None — no raw_json available).
        let mut cli_upsert = make_ticket("42", "Real Issue Updated");
        cli_upsert.raw_json = None;
        syncer.upsert_tickets("r1", &[cli_upsert]).unwrap();

        // raw_json must be unchanged — None must not clobber synced data.
        let after: String = conn
            .query_row(
                "SELECT raw_json FROM tickets WHERE source_id = '42'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            after, r#"{"id":42,"number":42,"title":"Real Issue"}"#,
            "CLI re-upsert with None must not overwrite existing raw_json"
        );

        // Title update from CLI upsert should still be applied.
        let title: String = conn
            .query_row(
                "SELECT title FROM tickets WHERE source_id = '42'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "Real Issue Updated");
    }
}
