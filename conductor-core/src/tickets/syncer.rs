use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rusqlite::{named_params, Connection};
use tracing::warn;

use crate::db::{query_collect, sql_placeholders, with_in_clause};
use crate::error::{ConductorError, Result};
use crate::github::merged_branches_for_repo;

use super::query::{
    map_ticket_row, query_dep_pairs, query_dep_pairs_for_repo, TICKET_COLS, TICKET_COLS_BARE,
};
use super::{
    ticket_not_found, ReadyTicket, Ticket, TicketDependencies, TicketFilter, TicketInput,
    TicketLabel, VALID_TICKET_STATES,
};

pub struct TicketSyncer<'a> {
    pub(super) conn: &'a Connection,
}

pub(in crate::tickets) const CLOSED_TICKET_ARTIFACTS_SQL: &str =
    "SELECT r.local_path, w.path, w.branch, r.remote_url
     FROM worktrees w
     JOIN repos r ON r.id = w.repo_id
     WHERE w.repo_id = :repo_id
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
        "SELECT id FROM tickets WHERE repo_id = :repo_id AND source_type = :source_type AND source_id = :source_id",
        named_params! { ":repo_id": repo_id, ":source_type": source_type, ":source_id": src },
        |row| row.get("id"),
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

    /// For a Jira-sourced ticket, fetches fresh data including comments and persists it.
    /// Returns `(effective_raw_json, formatted_comments_section)`.
    /// Falls back to the stored ticket data on any failure (non-fatal).
    pub fn enrich_jira_ticket_with_comments(&self, ticket: &Ticket) -> (String, String) {
        if ticket.source_type != "jira" {
            return (ticket.raw_json.clone(), String::new());
        }

        let jira_url = crate::issue_source::IssueSourceManager::new(self.conn)
            .list(&ticket.repo_id)
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.source_type == "jira")
            .and_then(|s| {
                serde_json::from_str::<crate::issue_source::JiraConfig>(&s.config_json)
                    .ok()
                    .map(|c| c.url)
            });

        let Some(url) = jira_url else {
            return (ticket.raw_json.clone(), String::new());
        };

        match crate::jira_acli::fetch_jira_issue(&ticket.source_id, &url) {
            Ok(fresh) => {
                let comments_str = super::format_comments_section(&fresh.comments);
                let fresh_raw = fresh.raw_json.clone();
                if let Err(e) = self.upsert_tickets(&ticket.repo_id, &[fresh]) {
                    tracing::warn!(
                        "failed to persist enriched Jira ticket {}: {e}",
                        ticket.source_id
                    );
                }
                (
                    fresh_raw.unwrap_or_else(|| ticket.raw_json.clone()),
                    comments_str,
                )
            }
            Err(e) => {
                tracing::warn!(
                    "failed to re-fetch Jira issue {} for enrichment: {e}",
                    ticket.source_id
                );
                (ticket.raw_json.clone(), String::new())
            }
        }
    }

    /// Upsert a batch of tickets for a repo. Returns the number of tickets upserted.
    pub fn upsert_tickets(&self, repo_id: &str, tickets: &[TicketInput]) -> Result<usize> {
        for ticket in tickets {
            ticket.validate()?;
        }

        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().to_rfc3339();

        // Pre-fetch existing raw_json for all tickets that don't supply one,
        // replacing the per-ticket SELECT with a single bulk query.
        let none_source_ids: Vec<String> = tickets
            .iter()
            .filter(|t| t.raw_json.is_none())
            .map(|t| t.source_id.clone())
            .collect();
        let mut existing_raw_json: HashMap<(String, String), String> = HashMap::new();
        if !none_source_ids.is_empty() {
            with_in_clause(
                "SELECT source_type, source_id, raw_json FROM tickets WHERE repo_id = ?1 AND source_id IN",
                &[&repo_id as &dyn rusqlite::types::ToSql],
                &none_source_ids,
                |sql, params| -> Result<()> {
                    let mut stmt = tx.prepare(sql)?;
                    let rows = stmt.query_map(params, |row| {
                        Ok((
                            row.get::<_, String>("source_type")?,
                            row.get::<_, String>("source_id")?,
                            row.get::<_, String>("raw_json")?,
                        ))
                    })?;
                    for row in rows {
                        let (source_type, source_id, raw_json) = row?;
                        existing_raw_json.insert((source_type, source_id), raw_json);
                    }
                    Ok(())
                },
            )?;
        }

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
                None => existing_raw_json
                    .get(&(ticket.source_type.clone(), ticket.source_id.clone()))
                    .cloned()
                    .unwrap_or_else(|| "{}".to_string()),
            };
            let ticket_id: String = tx.query_row(
                "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json)
                 VALUES (:id, :repo_id, :source_type, :source_id, :title, :body, :state, :labels, :assignee, :priority, :url, :synced_at, :raw_json)
                 ON CONFLICT(repo_id, source_type, source_id) DO UPDATE SET
                     title = excluded.title,
                     body = excluded.body,
                     state = excluded.state,
                     labels = excluded.labels,
                     assignee = excluded.assignee,
                     priority = excluded.priority,
                     url = excluded.url,
                     synced_at = excluded.synced_at,
                     raw_json = excluded.raw_json
                 RETURNING id",
                named_params! {
                    ":id": id,
                    ":repo_id": repo_id,
                    ":source_type": ticket.source_type,
                    ":source_id": ticket.source_id,
                    ":title": ticket.title,
                    ":body": ticket.body,
                    ":state": ticket.state,
                    ":labels": labels_json,
                    ":assignee": ticket.assignee,
                    ":priority": ticket.priority,
                    ":url": ticket.url,
                    ":synced_at": now,
                    ":raw_json": raw_json,
                },
                |row| row.get("id"),
            )?;
            tx.execute(
                "DELETE FROM ticket_labels WHERE ticket_id = :ticket_id",
                named_params! { ":ticket_id": ticket_id },
            )?;
            for ld in &ticket.label_details {
                tx.execute(
                    "INSERT OR REPLACE INTO ticket_labels (ticket_id, label, color) VALUES (:ticket_id, :label, :color)",
                    named_params! { ":ticket_id": ticket_id, ":label": ld.name, ":color": ld.color },
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
                    "DELETE FROM ticket_dependencies WHERE to_ticket_id = :ticket_id AND dep_type = 'blocks'",
                    named_params! { ":ticket_id": ticket_id },
                )?;
            }
            if !ticket.children.is_empty() {
                tx.execute(
                    "DELETE FROM ticket_dependencies WHERE from_ticket_id = :ticket_id AND dep_type = 'parent_of'",
                    named_params! { ":ticket_id": ticket_id },
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
                        "INSERT OR IGNORE INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'blocks')",
                        named_params! { ":from_id": id, ":to_id": ticket_id },
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
                        "INSERT OR IGNORE INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'parent_of')",
                        named_params! { ":from_id": ticket_id, ":to_id": id },
                    )?;
                }
            }

            // parent: another ticket is parent of this one → (parent_id, ticket_id, 'parent_of')
            if let Some(src) = &ticket.parent {
                // Replace any existing parent for this ticket
                tx.execute(
                    "DELETE FROM ticket_dependencies WHERE to_ticket_id = :ticket_id AND dep_type = 'parent_of'",
                    named_params! { ":ticket_id": ticket_id },
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
                        "INSERT OR IGNORE INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'parent_of')",
                        named_params! { ":from_id": id, ":to_id": ticket_id },
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
            "SELECT MAX(synced_at) AS max_synced_at FROM tickets WHERE repo_id = :repo_id",
            named_params! { ":repo_id": repo_id },
            |row| row.get("max_synced_at"),
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
                 FROM tickets WHERE repo_id = :repo_id ORDER BY CAST(source_id AS INTEGER) DESC, source_id DESC"
            }
            None => {
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets ORDER BY CAST(source_id AS INTEGER) DESC, source_id DESC"
            }
        };

        let tickets = if let Some(rid) = repo_id {
            query_collect(
                self.conn,
                query,
                rusqlite::named_params! { ":repo_id": rid },
                map_ticket_row,
            )?
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

        if filter.unlabeled_only {
            conditions.push(
                "NOT EXISTS (SELECT 1 FROM ticket_labels tl WHERE tl.ticket_id = t.id)".to_string(),
            );
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
            "SELECT ticket_id FROM worktrees WHERE id = :id",
            named_params! { ":id": worktree_id },
            |row| row.get("ticket_id"),
        )?;
        if existing.is_some() {
            return Err(ConductorError::TicketAlreadyLinked);
        }
        self.conn.execute(
            "UPDATE worktrees SET ticket_id = :ticket_id WHERE id = :worktree_id",
            named_params! { ":ticket_id": ticket_id, ":worktree_id": worktree_id },
        )?;
        Ok(())
    }

    /// Fetch a single ticket by repo ID + external source ID (e.g. GitHub issue number).
    /// Returns `TicketNotFound` if no matching ticket exists.
    pub fn get_by_source_id(&self, repo_id: &str, source_id: &str) -> Result<Ticket> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets WHERE repo_id = :repo_id AND source_id = :source_id",
                named_params! { ":repo_id": repo_id, ":source_id": source_id },
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
                 FROM tickets WHERE source_id = :source_id LIMIT 1",
                named_params! { ":source_id": source_id },
                map_ticket_row,
            )
            .map_err(ticket_not_found(source_id))
    }

    /// Fetch a single ticket by its internal (ULID) ID.
    pub fn get_by_id(&self, ticket_id: &str) -> Result<Ticket> {
        self.conn
            .query_row(
                "SELECT id, repo_id, source_type, source_id, title, body, state, labels, assignee, priority, url, synced_at, raw_json, workflow, agent_map
                 FROM tickets WHERE id = :id",
                named_params! { ":id": ticket_id },
                map_ticket_row,
            )
            .map_err(ticket_not_found(ticket_id))
    }

    /// Resolve a mixed list of IDs (internal IDs or source_ids) within a single repo
    /// using at most 2 DB queries regardless of how many IDs are provided.
    ///
    /// First, all inputs are attempted as internal ticket IDs in a single IN-clause
    /// query.  Any IDs not found that way (or belonging to a different repo) are
    /// retried as `source_id`s in a second IN-clause query.
    /// Returns tickets in the same order as `raw_ids`.
    /// Returns `ConductorError::TicketNotFound` for the first unresolvable ID.
    pub fn resolve_tickets_in_repo(
        &self,
        repo_id: &str,
        raw_ids: &[String],
    ) -> Result<Vec<Ticket>> {
        if raw_ids.is_empty() {
            return Ok(vec![]);
        }

        // Query 1: batch-fetch by internal id.
        let ph = sql_placeholders(raw_ids.len());
        let sql = format!("SELECT {TICKET_COLS_BARE} FROM tickets WHERE id IN ({ph})");
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            raw_ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = query_collect(self.conn, &sql, params_vec.as_slice(), map_ticket_row)?;
        let mut by_id: HashMap<String, Ticket> = HashMap::with_capacity(rows.len());
        for t in rows {
            by_id.insert(t.id.clone(), t);
        }

        // Collect IDs that were not found or belong to a different repo.
        let fallback_ids: Vec<&str> = raw_ids
            .iter()
            .filter(|raw| by_id.get(raw.as_str()).is_none_or(|t| t.repo_id != repo_id))
            .map(String::as_str)
            .collect();

        // Query 2: batch-fetch misses by source_id (scoped to the repo).
        let mut by_source_id: HashMap<String, Ticket> = HashMap::new();
        if !fallback_ids.is_empty() {
            let ph2 = crate::db::sql_placeholders_from(fallback_ids.len(), 2);
            let sql2 = format!(
                "SELECT {TICKET_COLS_BARE} FROM tickets WHERE repo_id = ?1 AND source_id IN ({ph2})"
            );
            let mut p2: Vec<&dyn rusqlite::ToSql> = vec![&repo_id];
            p2.extend(fallback_ids.iter().map(|s| s as &dyn rusqlite::ToSql));
            let rows2 = query_collect(self.conn, &sql2, p2.as_slice(), map_ticket_row)?;
            for t in rows2 {
                by_source_id.insert(t.source_id.clone(), t);
            }
        }

        // Reconstruct in original order: internal-id hit first, then source_id hit.
        let mut resolved = Vec::with_capacity(raw_ids.len());
        for raw in raw_ids {
            let ticket = by_id
                .get(raw)
                .filter(|t| t.repo_id == repo_id)
                .or_else(|| by_source_id.get(raw))
                .ok_or_else(|| ConductorError::TicketNotFound { id: raw.clone() })?;
            resolved.push(ticket.clone());
        }
        Ok(resolved)
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
                "UPDATE tickets SET state = :state WHERE id = :id",
                rusqlite::named_params! { ":state": s, ":id": ticket_id },
            )?;
        }
        if let Some(w) = workflow {
            let val: Option<&str> = if w.is_empty() { None } else { Some(w) };
            self.conn.execute(
                "UPDATE tickets SET workflow = :workflow WHERE id = :id",
                rusqlite::named_params! { ":workflow": val, ":id": ticket_id },
            )?;
        }
        if let Some(a) = agent_map {
            let val: Option<&str> = if a.is_empty() { None } else { Some(a) };
            self.conn.execute(
                "UPDATE tickets SET agent_map = :agent_map WHERE id = :id",
                rusqlite::named_params! { ":agent_map": val, ":id": ticket_id },
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
                "SELECT id FROM tickets WHERE repo_id = :repo_id AND source_type = :source_type AND source_id = :source_id",
                named_params! { ":repo_id": repo_id, ":source_type": source_type, ":source_id": source_id },
                |row| row.get("id"),
            )
            .map_err(ticket_not_found(format!("{source_type}#{source_id}")))?;

        // NULL out workflow_runs.ticket_id (FK lacks ON DELETE SET NULL).
        tx.execute(
            "UPDATE workflow_runs SET ticket_id = NULL WHERE ticket_id = :ticket_id",
            named_params! { ":ticket_id": ticket_id },
        )?;

        // Delete the ticket row. Cascades handle ticket_labels;
        // worktrees.ticket_id is ON DELETE SET NULL.
        tx.execute(
            "DELETE FROM tickets WHERE id = :id",
            named_params! { ":id": ticket_id },
        )?;

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
            "SELECT ticket_id, label, color FROM ticket_labels WHERE ticket_id = :ticket_id ORDER BY label",
            named_params! { ":ticket_id": ticket_id },
            |row| {
                Ok(TicketLabel {
                    ticket_id: row.get("ticket_id")?,
                    label: row.get("label")?,
                    color: row.get("color")?,
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
                    ticket_id: row.get("ticket_id")?,
                    label: row.get("label")?,
                    color: row.get("color")?,
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
                 WHERE d.to_ticket_id = :ticket_id AND d.dep_type = 'blocks'"
            ),
            named_params! { ":ticket_id": ticket_id },
            map_ticket_row,
        )?;

        // Tickets this one blocks (from_ticket_id = this, to_ticket_id = blocked)
        let blocks = query_collect(
            self.conn,
            &format!(
                "SELECT {TICKET_COLS} FROM tickets t
                 JOIN ticket_dependencies d ON d.to_ticket_id = t.id
                 WHERE d.from_ticket_id = :ticket_id AND d.dep_type = 'blocks'"
            ),
            named_params! { ":ticket_id": ticket_id },
            map_ticket_row,
        )?;

        // Parent ticket (from_ticket_id = parent, to_ticket_id = this)
        let parent = query_collect(
            self.conn,
            &format!(
                "SELECT {TICKET_COLS} FROM tickets t
                 JOIN ticket_dependencies d ON d.from_ticket_id = t.id
                 WHERE d.to_ticket_id = :ticket_id AND d.dep_type = 'parent_of'"
            ),
            named_params! { ":ticket_id": ticket_id },
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
                 WHERE d.from_ticket_id = :ticket_id AND d.dep_type = 'parent_of'"
            ),
            named_params! { ":ticket_id": ticket_id },
            map_ticket_row,
        )?;

        Ok(TicketDependencies {
            blocked_by,
            blocks,
            parent,
            children,
        })
    }

    /// Batch-loads dependencies for tickets belonging to a single repo.
    /// Returns `ticket_id → TicketDependencies`. Used by per-repo API endpoints
    /// to avoid loading the entire dependency graph for every API call.
    pub fn get_all_dependencies_for_repo(
        &self,
        repo_id: &str,
    ) -> Result<HashMap<String, TicketDependencies>> {
        let blocks_rows = query_dep_pairs_for_repo(self.conn, "blocks", repo_id)?;
        let parent_rows = query_dep_pairs_for_repo(self.conn, "parent_of", repo_id)?;

        let mut map: HashMap<String, TicketDependencies> = HashMap::new();

        for (from_id, to_id, from_ticket, to_ticket) in blocks_rows {
            map.entry(to_id).or_default().blocked_by.push(from_ticket);
            map.entry(from_id).or_default().blocks.push(to_ticket);
        }

        for (from_id, to_id, from_ticket, to_ticket) in parent_rows {
            map.entry(to_id).or_default().parent = Some(from_ticket);
            map.entry(from_id).or_default().children.push(to_ticket);
        }

        Ok(map)
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

    /// Batch-loads `blocks` edges for a specific set of ticket IDs.
    /// Returns `(from_ticket_id, to_ticket_id)` pairs — i.e. (blocker, blocked).
    /// Callers must guard against an empty `ticket_ids` slice before calling.
    pub fn get_blocking_edges_for_tickets(
        &self,
        ticket_ids: &[&str],
    ) -> Result<Vec<(String, String)>> {
        if ticket_ids.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = sql_placeholders(ticket_ids.len());
        let sql = format!(
            "SELECT from_ticket_id, to_ticket_id FROM ticket_dependencies \
             WHERE to_ticket_id IN ({placeholders}) AND dep_type = 'blocks'"
        );
        let params: Vec<&dyn rusqlite::ToSql> = ticket_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = self.conn.prepare(&sql).map_err(ConductorError::Database)?;
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>("from_ticket_id")?,
                    row.get::<_, String>("to_ticket_id")?,
                ))
            })
            .map_err(ConductorError::Database)?;
        rows.map(|r| r.map_err(ConductorError::Database)).collect()
    }

    /// Returns `(from_ticket_id, to_ticket_id)` pairs for `blocks` edges where
    /// **both** endpoints are within `ticket_ids`. Used by `WorktreeManager` to
    /// build the intra-set dependency graph for stacked-worktree creation.
    pub fn get_blocks_edges_within_set(
        &self,
        ticket_ids: &[String],
    ) -> Result<Vec<(String, String)>> {
        if ticket_ids.is_empty() {
            return Ok(vec![]);
        }
        let n = ticket_ids.len();
        let from_ph = sql_placeholders(n);
        let to_ph = crate::db::sql_placeholders_from(n, n + 1);
        let sql = format!(
            "SELECT from_ticket_id, to_ticket_id \
             FROM ticket_dependencies \
             WHERE from_ticket_id IN ({from_ph}) \
               AND to_ticket_id IN ({to_ph}) \
               AND dep_type = 'blocks'"
        );
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(n * 2);
        for id in ticket_ids {
            params_vec.push(id);
        }
        for id in ticket_ids {
            params_vec.push(id);
        }
        query_collect(self.conn, &sql, params_vec.as_slice(), |row| {
            Ok((
                row.get::<_, String>("from_ticket_id")?,
                row.get::<_, String>("to_ticket_id")?,
            ))
        })
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
        self.mark_worktrees_for_closed_tickets_with_merge_check(repo_id, merged_branches_for_repo)
    }

    pub(super) fn mark_worktrees_for_closed_tickets_with_merge_check(
        &self,
        repo_id: &str,
        merge_check: impl Fn(&str, &[String]) -> HashMap<String, String>,
    ) -> Result<usize> {
        // Collect git paths before updating so we can clean up worktree dirs and branches.
        let artifacts: Vec<(String, String, String, String)> = query_collect(
            self.conn,
            CLOSED_TICKET_ARTIFACTS_SQL,
            named_params! { ":repo_id": repo_id },
            |row| {
                Ok((
                    row.get("local_path")?,
                    row.get("path")?,
                    row.get("branch")?,
                    row.get("remote_url")?,
                ))
            },
        )?;

        // Group branches by remote_url and batch-check merged status per repo.
        let mut branches_by_remote: HashMap<&str, Vec<String>> = HashMap::new();
        for (_, _, branch, remote_url) in &artifacts {
            branches_by_remote
                .entry(remote_url.as_str())
                .or_default()
                .push(branch.clone());
        }
        let mut merged_branches: HashSet<String> = HashSet::new();
        for (remote_url, branches) in &branches_by_remote {
            merged_branches.extend(merge_check(remote_url, branches).into_keys());
        }

        let now = Utc::now().to_rfc3339();
        let mut count = 0usize;

        for (repo_path, worktree_path, branch, _remote_url) in &artifacts {
            if !merged_branches.contains(branch) {
                // Ticket is closed but PR not yet merged — leave the worktree alone.
                continue;
            }
            self.conn.execute(
                "UPDATE worktrees SET status = 'merged', completed_at = :now
                 WHERE path = :path AND status != 'merged'",
                named_params! { ":now": now, ":path": worktree_path },
            )?;
            count += 1;
            crate::worktree::WorktreeManager::remove_artifacts(repo_path, worktree_path, branch);
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
                id: row.get("id")?,
                source_id: row.get("source_id")?,
                title: row.get("title")?,
                url: row.get("url")?,
                dep_type: row.get("dep_type")?,
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
            let ticket_id = crate::worktree::get_ticket_id_by_branch(self.conn, &repo.id, &branch)?;
            let ticket_id = ticket_id.ok_or_else(|| {
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
