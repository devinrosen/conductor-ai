use chrono::Utc;
use rusqlite::{named_params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::{query_collect, sql_placeholders, with_in_clause};
use crate::error::{ConductorError, Result};
use crate::workflow::persistence::NewFanOutItem;

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

fn fan_out_item_from_row(row: &rusqlite::Row) -> rusqlite::Result<FanOutItemRow> {
    Ok(FanOutItemRow {
        id: row.get("id")?,
        step_run_id: row.get("step_run_id")?,
        item_type: row.get("item_type")?,
        item_id: row.get("item_id")?,
        item_ref: row.get("item_ref")?,
        child_run_id: row.get("child_run_id")?,
        status: row.get("status")?,
        dispatched_at: row.get("dispatched_at")?,
        completed_at: row.get("completed_at")?,
    })
}

impl<'a> WorkflowManager<'a> {
    /// Insert multiple fan-out item rows in a single transaction.
    /// Ignores duplicates (idempotent). No-ops if `items` is empty.
    pub fn insert_fan_out_items_batch(
        &self,
        step_run_id: &str,
        items: &[NewFanOutItem],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for item in items {
            let id = crate::new_id();
            tx.execute(
                "INSERT OR IGNORE INTO workflow_run_step_fan_out_items \
                 (id, step_run_id, item_type, item_id, item_ref, status) \
                 VALUES (:id, :step_run_id, :item_type, :item_id, :item_ref, 'pending')",
                named_params![":id": id, ":step_run_id": step_run_id, ":item_type": item.item_type, ":item_id": item.item_id, ":item_ref": item.item_ref],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

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
             VALUES (:id, :step_run_id, :item_type, :item_id, :item_ref, 'pending')",
            named_params![":id": id, ":step_run_id": step_run_id, ":item_type": item_type, ":item_id": item_id, ":item_ref": item_ref],
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
        if let Some(status) = status_filter {
            query_collect(
                self.conn,
                "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
                 status, dispatched_at, completed_at \
                 FROM workflow_run_step_fan_out_items \
                 WHERE step_run_id = :step_run_id AND status = :status \
                 ORDER BY id ASC",
                named_params![":step_run_id": step_run_id, ":status": status],
                fan_out_item_from_row,
            )
        } else {
            query_collect(
                self.conn,
                "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
                 status, dispatched_at, completed_at \
                 FROM workflow_run_step_fan_out_items \
                 WHERE step_run_id = :step_run_id \
                 ORDER BY id ASC",
                named_params![":step_run_id": step_run_id],
                fan_out_item_from_row,
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
        let rows = stmt.query_map(
            rusqlite::params_from_iter(step_run_ids.iter()),
            fan_out_item_from_row,
        )?;
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
            "SELECT item_id FROM workflow_run_step_fan_out_items WHERE step_run_id = :step_run_id",
            named_params![":step_run_id": step_run_id],
            |row| row.get("item_id"),
        )
    }

    /// Mark a fan-out item as running and set its child_run_id.
    pub fn update_fan_out_item_running(&self, item_id: &str, child_run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = 'running', child_run_id = :child_run_id, dispatched_at = :now \
             WHERE id = :id",
            named_params![":child_run_id": child_run_id, ":now": now, ":id": item_id],
        )?;
        Ok(())
    }

    /// Mark a fan-out item as terminal (completed, failed, or skipped).
    pub fn update_fan_out_item_terminal(&self, item_id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = :status, completed_at = :now \
             WHERE id = :id",
            named_params![":status": status, ":now": now, ":id": item_id],
        )?;
        Ok(())
    }

    /// Reset `running` fan-out items that have no `child_run_id` back to `pending`.
    ///
    /// These are orphaned items whose background threads died when the parent workflow
    /// was killed between setting `status='running'` and writing `child_run_id`. Called
    /// on resume, before entering the dispatch loop, so they are re-dispatched correctly.
    /// Returns the count of items reset.
    pub fn reset_running_items_without_child_run(&self, step_run_id: &str) -> Result<u64> {
        let count = self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = 'pending', dispatched_at = NULL \
             WHERE step_run_id = :step_run_id \
               AND status = 'running' \
               AND child_run_id IS NULL",
            named_params![":step_run_id": step_run_id],
        )?;
        Ok(count as u64)
    }

    /// Mark all pending/running fan-out items for a step as skipped.
    /// Used when on_child_fail = Halt to cancel remaining work.
    pub fn cancel_fan_out_items(&self, step_run_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE workflow_run_step_fan_out_items \
             SET status = 'skipped', completed_at = :now \
             WHERE step_run_id = :step_run_id AND status IN ('pending', 'running')",
            named_params![":now": now, ":step_run_id": step_run_id],
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
            &[
                &now as &dyn rusqlite::ToSql,
                &step_run_id as &dyn rusqlite::ToSql,
            ],
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
                                  WHERE step_run_id = :step_run_id AND status = 'completed'), \
             fan_out_failed = (SELECT COUNT(*) FROM workflow_run_step_fan_out_items \
                               WHERE step_run_id = :step_run_id AND status = 'failed'), \
             fan_out_skipped = (SELECT COUNT(*) FROM workflow_run_step_fan_out_items \
                                WHERE step_run_id = :step_run_id AND status = 'skipped') \
             WHERE id = :step_run_id",
            named_params![":step_run_id": step_run_id],
        )?;
        Ok(())
    }

    /// Set the fan_out_total counter on a step row.
    pub fn set_fan_out_total(&self, step_run_id: &str, total: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE workflow_run_steps SET fan_out_total = :total WHERE id = :id",
            named_params![":total": total, ":id": step_run_id],
        )?;
        Ok(())
    }

    /// Get the status of a workflow run by its ID.
    pub fn get_workflow_run_status(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = :id",
                named_params![":id": run_id],
                |row| row.get("status"),
            )
            .optional()?)
    }
}
