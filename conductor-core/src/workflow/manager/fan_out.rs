use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{query_collect, sql_placeholders, with_in_clause};
use crate::error::{ConductorError, Result};

use super::WorkflowManager;

/// A single row in the `workflow_run_step_fan_out_items` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FanOutItemRow {
    pub id: String,
    pub step_run_id: String,
    pub item_type: String,
    pub item_id: String,
    pub item_ref: String,
    pub child_run_id: Option<String>,
    pub status: String,
    pub dispatched_at: Option<String>,
    pub completed_at: Option<String>,
}

impl<'a> WorkflowManager<'a> {
    /// Insert a fan-out item row with status = 'pending'.
    /// Ignores duplicates (idempotent — safe to call on resume).
    pub fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String> {
        let id = crate::new_id();
        self.conn.execute(
            "INSERT OR IGNORE INTO workflow_run_step_fan_out_items \
             (id, step_run_id, item_type, item_id, item_ref, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
            params![id, step_run_id, item_type, item_id, item_ref],
        )?;
        Ok(id)
    }

    /// Fetch fan-out items for a step, validating that the step belongs to the specified run.
    /// Returns `WorkflowStepNotFound` if the step doesn't exist, or `WorkflowStepNotInRun`
    /// if it exists but belongs to a different run. Protects all callers (web, CLI, MCP).
    pub fn get_fan_out_items_checked(
        &self,
        run_id: &str,
        step_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<FanOutItemRow>> {
        let step =
            self.get_step_by_id(step_id)?
                .ok_or_else(|| ConductorError::WorkflowStepNotFound {
                    id: step_id.to_string(),
                })?;
        if step.workflow_run_id != run_id {
            return Err(ConductorError::WorkflowStepNotInRun {
                step_id: step_id.to_string(),
                run_id: run_id.to_string(),
            });
        }
        self.get_fan_out_items(step_id, status_filter)
    }

    /// Fetch all fan-out items for a step, optionally filtered by status.
    /// Pass `None` for `status_filter` to get all items.
    pub fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<FanOutItemRow>> {
        let row_mapper = |row: &rusqlite::Row| {
            Ok(FanOutItemRow {
                id: row.get(0)?,
                step_run_id: row.get(1)?,
                item_type: row.get(2)?,
                item_id: row.get(3)?,
                item_ref: row.get(4)?,
                child_run_id: row.get(5)?,
                status: row.get(6)?,
                dispatched_at: row.get(7)?,
                completed_at: row.get(8)?,
            })
        };
        if let Some(status) = status_filter {
            query_collect(
                self.conn,
                "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
                 status, dispatched_at, completed_at \
                 FROM workflow_run_step_fan_out_items \
                 WHERE step_run_id = ?1 AND status = ?2 \
                 ORDER BY id ASC",
                params![step_run_id, status],
                row_mapper,
            )
        } else {
            query_collect(
                self.conn,
                "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
                 status, dispatched_at, completed_at \
                 FROM workflow_run_step_fan_out_items \
                 WHERE step_run_id = ?1 \
                 ORDER BY id ASC",
                params![step_run_id],
                row_mapper,
            )
        }
    }

    /// Fetch fan-out items for multiple steps in a single query.
    /// Returns a map from `step_run_id` → items, omitting step IDs that have no items.
    pub fn get_fan_out_items_for_steps(
        &self,
        step_run_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<FanOutItemRow>>> {
        if step_run_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let sql = format!(
            "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
             status, dispatched_at, completed_at \
             FROM workflow_run_step_fan_out_items \
             WHERE step_run_id IN ({}) \
             ORDER BY step_run_id, id ASC",
            sql_placeholders(step_run_ids.len())
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(step_run_ids.iter()), |row| {
            Ok(FanOutItemRow {
                id: row.get(0)?,
                step_run_id: row.get(1)?,
                item_type: row.get(2)?,
                item_id: row.get(3)?,
                item_ref: row.get(4)?,
                child_run_id: row.get(5)?,
                status: row.get(6)?,
                dispatched_at: row.get(7)?,
                completed_at: row.get(8)?,
            })
        })?;
        let mut map: std::collections::HashMap<String, Vec<FanOutItemRow>> =
            std::collections::HashMap::new();
        for row in rows {
            let item = row?;
            map.entry(item.step_run_id.clone()).or_default().push(item);
        }
        Ok(map)
    }

    /// Get the IDs of all items already in the fan-out table for a step (for dedup on resume).
    pub fn get_existing_fan_out_item_ids(&self, step_run_id: &str) -> Result<Vec<String>> {
        query_collect(
            self.conn,
            "SELECT item_id FROM workflow_run_step_fan_out_items WHERE step_run_id = ?1",
            params![step_run_id],
            |row| row.get(0),
        )
    }

    /// Mark a fan-out item as running and set its child_run_id.
    pub fn update_fan_out_item_running(&self, item_id: &str, child_run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = 'running', child_run_id = ?1, dispatched_at = ?2 \
             WHERE id = ?3",
            params![child_run_id, now, item_id],
        )?;
        Ok(())
    }

    /// Mark a fan-out item as terminal (completed, failed, or skipped).
    pub fn update_fan_out_item_terminal(&self, item_id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = ?1, completed_at = ?2 \
             WHERE id = ?3",
            params![status, now, item_id],
        )?;
        Ok(())
    }

    /// Mark all pending/running fan-out items for a step as skipped.
    /// Used when on_child_fail = Halt to cancel remaining work.
    pub fn cancel_fan_out_items(&self, step_run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = 'skipped', completed_at = ?1 \
             WHERE step_run_id = ?2 AND status IN ('pending', 'running')",
            params![now, step_run_id],
        )?;
        Ok(())
    }

    /// Mark specific fan-out items as skipped by their item_id values.
    /// Used for skip_dependents to mark transitively blocked items.
    pub fn skip_fan_out_items_by_item_ids(
        &self,
        step_run_id: &str,
        item_ids: &[String],
    ) -> Result<()> {
        if item_ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        // ?1 = now, ?2 = step_run_id; item_ids are bound starting at ?3.
        // SQLite's default SQLITE_MAX_VARIABLE_NUMBER is 999, which is not a
        // practical concern here (skip_dependents list is bounded by fan-out size).
        with_in_clause(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = 'skipped', completed_at = ?1 \
             WHERE step_run_id = ?2 AND status = 'pending' AND item_id IN",
            &[&now as &dyn rusqlite::ToSql, &step_run_id as &dyn rusqlite::ToSql],
            item_ids,
            |sql, params| self.conn.execute(sql, params),
        )?;
        Ok(())
    }

    /// Recount fan-out progress counters from items table and update workflow_run_steps row.
    /// Uses atomic SQL increments to avoid lost updates.
    pub fn refresh_fan_out_counters(&self, step_run_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET \
             fan_out_completed = (SELECT COUNT(*) FROM workflow_run_step_fan_out_items \
                                  WHERE step_run_id = ?1 AND status = 'completed'), \
             fan_out_failed = (SELECT COUNT(*) FROM workflow_run_step_fan_out_items \
                               WHERE step_run_id = ?1 AND status = 'failed'), \
             fan_out_skipped = (SELECT COUNT(*) FROM workflow_run_step_fan_out_items \
                                WHERE step_run_id = ?1 AND status = 'skipped') \
             WHERE id = ?1",
            params![step_run_id],
        )?;
        Ok(())
    }

    /// Set the fan_out_total counter on a step row.
    pub fn set_fan_out_total(&self, step_run_id: &str, total: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET fan_out_total = ?1 WHERE id = ?2",
            params![total, step_run_id],
        )?;
        Ok(())
    }

    /// Get the status of a workflow run by its ID.
    pub fn get_workflow_run_status(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?)
    }
}
