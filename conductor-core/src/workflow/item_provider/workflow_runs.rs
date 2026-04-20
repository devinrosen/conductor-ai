use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::workflow_dsl::ForeachScope;

use super::{FanOutItem, ItemProvider, ProviderContext};

pub struct WorkflowRunsProvider;

impl ItemProvider for WorkflowRunsProvider {
    fn name(&self) -> &str {
        "workflow_runs"
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        _scope: Option<&ForeachScope>,
        filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<FanOutItem>> {
        let status_filter = filter.get("status").map(|s| s.as_str()).unwrap_or("");
        let workflow_name_filter = filter
            .get("workflow_name")
            .map(|s| s.as_str())
            .unwrap_or("");

        let terminal_statuses = ["completed", "failed", "cancelled"];
        let statuses: Vec<&str> = if status_filter.is_empty() {
            terminal_statuses.to_vec()
        } else {
            status_filter
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        };

        let mut conditions: Vec<String> = Vec::new();
        let placeholder_list: String = statuses
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        conditions.push(format!("status IN ({placeholder_list})"));

        let mut param_values: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();

        if !workflow_name_filter.is_empty() {
            param_values.push(workflow_name_filter.to_string());
            conditions.push(format!("workflow_name = ?{}", param_values.len()));
        }

        let sql = format!(
            "SELECT id, workflow_name FROM workflow_runs WHERE {} ORDER BY started_at ASC",
            conditions.join(" AND ")
        );
        let rows: Vec<(String, String)> = crate::db::query_collect(
            ctx.conn,
            &sql,
            rusqlite::params_from_iter(param_values.iter()),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;

        Ok(rows
            .into_iter()
            .filter(|(id, _)| !existing_set.contains(id))
            .map(|(id, wf_name)| FanOutItem {
                item_type: "workflow_run".to_string(),
                item_id: id,
                item_ref: wf_name,
            })
            .collect())
    }
}
