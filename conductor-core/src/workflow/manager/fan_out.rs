use chrono::Utc;
use rusqlite::{named_params, Connection};

use crate::db::{query_collect, sql_placeholders, with_in_clause};
use crate::error::{ConductorError, Result};
use runkon_flow::types::FanOutItemRow;

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

pub fn insert_fan_out_item(
    conn: &Connection,
    step_run_id: &str,
    item_type: &str,
    item_id: &str,
    item_ref: &str,
) -> Result<String> {
    let id = crate::new_id();
    conn.execute(
            "INSERT OR IGNORE INTO workflow_run_step_fan_out_items \
             (id, step_run_id, item_type, item_id, item_ref, status) \
             VALUES (:id, :step_run_id, :item_type, :item_id, :item_ref, 'pending')",
            named_params![":id": id, ":step_run_id": step_run_id, ":item_type": item_type, ":item_id": item_id, ":item_ref": item_ref],
        )?;
    Ok(id)
}

pub fn get_fan_out_items_checked(
    conn: &Connection,
    run_id: &str,
    step_id: &str,
    status_filter: Option<&str>,
) -> Result<Vec<FanOutItemRow>> {
    let step = super::queries::get_step_by_id(conn, step_id)?.ok_or_else(|| {
        ConductorError::WorkflowStepNotFound {
            id: step_id.to_string(),
        }
    })?;
    if step.workflow_run_id != run_id {
        return Err(ConductorError::WorkflowStepNotInRun {
            step_id: step_id.to_string(),
            run_id: run_id.to_string(),
        });
    }
    get_fan_out_items(conn, step_id, status_filter)
}

pub fn get_fan_out_items(
    conn: &Connection,
    step_run_id: &str,
    status_filter: Option<&str>,
) -> Result<Vec<FanOutItemRow>> {
    let status_clause = if status_filter.is_some() {
        " AND status = :status"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, step_run_id, item_type, item_id, item_ref, child_run_id, \
             status, dispatched_at, completed_at \
             FROM workflow_run_step_fan_out_items \
             WHERE step_run_id = :step_run_id{status_clause} \
             ORDER BY id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = vec![(":step_run_id", &step_run_id)];
    if let Some(ref status) = status_filter {
        params.push((":status", status));
    }
    let rows = stmt.query_map(params.as_slice(), fan_out_item_from_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_fan_out_items_for_steps(
    conn: &Connection,
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
    let mut stmt = conn.prepare(&sql)?;
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

pub fn get_existing_fan_out_item_ids(conn: &Connection, step_run_id: &str) -> Result<Vec<String>> {
    query_collect(
        conn,
        "SELECT item_id FROM workflow_run_step_fan_out_items WHERE step_run_id = :step_run_id",
        named_params![":step_run_id": step_run_id],
        |row| row.get("item_id"),
    )
}

pub fn update_fan_out_item_running(
    conn: &Connection,
    item_id: &str,
    child_run_id: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE workflow_run_step_fan_out_items \
             SET status = 'running', child_run_id = :child_run_id, dispatched_at = :now \
             WHERE id = :id",
        named_params![":child_run_id": child_run_id, ":now": now, ":id": item_id],
    )?;
    Ok(())
}

pub fn update_fan_out_item_terminal(conn: &Connection, item_id: &str, status: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE workflow_run_step_fan_out_items \
             SET status = :status, completed_at = :now \
             WHERE id = :id",
        named_params![":status": status, ":now": now, ":id": item_id],
    )?;
    Ok(())
}

pub fn reset_running_items_without_child_run(conn: &Connection, step_run_id: &str) -> Result<u64> {
    let count = conn.execute(
        "UPDATE workflow_run_step_fan_out_items \
             SET status = 'pending', dispatched_at = NULL \
             WHERE step_run_id = :step_run_id \
               AND status = 'running' \
               AND child_run_id IS NULL",
        named_params![":step_run_id": step_run_id],
    )?;
    Ok(count as u64)
}

pub fn cancel_fan_out_items(conn: &Connection, step_run_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE workflow_run_step_fan_out_items \
             SET status = 'skipped', completed_at = :now \
             WHERE step_run_id = :step_run_id AND status IN ('pending', 'running')",
        named_params![":now": now, ":step_run_id": step_run_id],
    )?;
    Ok(())
}

pub fn skip_fan_out_items_by_item_ids(
    conn: &Connection,
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
        |sql, params| conn.execute(sql, params),
    )?;
    Ok(())
}

pub fn refresh_fan_out_counters(conn: &Connection, step_run_id: &str) -> Result<()> {
    conn.execute(
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

pub fn set_fan_out_total(conn: &Connection, step_run_id: &str, total: i64) -> Result<()> {
    conn.execute(
        "UPDATE workflow_run_steps SET fan_out_total = :total WHERE id = :id",
        named_params![":total": total, ":id": step_run_id],
    )?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────

// Shim impl: keeps `WorkflowManager::<method>` callable while the free functions

// above are the canonical implementations. Removed in the final cleanup PR.

// ─────────────────────────────────────────────────────────────────────────────

impl<'a> super::WorkflowManager<'a> {
    pub fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String> {
        insert_fan_out_item(self.conn, step_run_id, item_type, item_id, item_ref)
    }

    pub fn get_fan_out_items_checked(
        &self,
        run_id: &str,
        step_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<FanOutItemRow>> {
        get_fan_out_items_checked(self.conn, run_id, step_id, status_filter)
    }

    pub fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<&str>,
    ) -> Result<Vec<FanOutItemRow>> {
        get_fan_out_items(self.conn, step_run_id, status_filter)
    }

    pub fn get_fan_out_items_for_steps(
        &self,
        step_run_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<FanOutItemRow>>> {
        get_fan_out_items_for_steps(self.conn, step_run_ids)
    }

    pub fn get_existing_fan_out_item_ids(&self, step_run_id: &str) -> Result<Vec<String>> {
        get_existing_fan_out_item_ids(self.conn, step_run_id)
    }

    pub fn update_fan_out_item_running(&self, item_id: &str, child_run_id: &str) -> Result<()> {
        update_fan_out_item_running(self.conn, item_id, child_run_id)
    }

    pub fn update_fan_out_item_terminal(&self, item_id: &str, status: &str) -> Result<()> {
        update_fan_out_item_terminal(self.conn, item_id, status)
    }

    pub fn reset_running_items_without_child_run(&self, step_run_id: &str) -> Result<u64> {
        reset_running_items_without_child_run(self.conn, step_run_id)
    }

    pub fn cancel_fan_out_items(&self, step_run_id: &str) -> Result<()> {
        cancel_fan_out_items(self.conn, step_run_id)
    }

    pub fn skip_fan_out_items_by_item_ids(
        &self,
        step_run_id: &str,
        item_ids: &[String],
    ) -> Result<()> {
        skip_fan_out_items_by_item_ids(self.conn, step_run_id, item_ids)
    }

    pub fn refresh_fan_out_counters(&self, step_run_id: &str) -> Result<()> {
        refresh_fan_out_counters(self.conn, step_run_id)
    }

    pub fn set_fan_out_total(&self, step_run_id: &str, total: i64) -> Result<()> {
        set_fan_out_total(self.conn, step_run_id, total)
    }
}
