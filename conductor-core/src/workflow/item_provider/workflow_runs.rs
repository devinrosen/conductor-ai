use std::collections::HashMap;

use crate::error::Result;
use runkon_flow::dsl::ForeachScope;

use super::{collect_fan_out_items, FanOutItem, ItemProvider, ProviderContext};

pub struct WorkflowRunsProvider;

impl ItemProvider for WorkflowRunsProvider {
    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        _scope: Option<&ForeachScope>,
        filter: &HashMap<String, String>,
    ) -> Result<Vec<FanOutItem>> {
        let status_filter = filter.get("status").map(|s| s.as_str()).unwrap_or("");
        let workflow_name_filter = filter.get("workflow_name").map(|s| s.as_str());

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
        let rows = crate::workflow::list_runs_by_status(ctx.conn, &statuses, workflow_name_filter)?;

        Ok(collect_fan_out_items(rows, |(id, wf_name)| FanOutItem {
            item_type: "workflow_run".to_string(),
            item_id: id,
            item_ref: wf_name,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_helpers;

    #[test]
    fn test_default_terminal_statuses_when_no_filter() {
        let conn = test_helpers::setup_db();
        let config = Config::default();

        // Insert a completed run and a running run.
        let parent_id = test_helpers::make_agent_parent_id(&conn);

        let run1 = crate::workflow::create_workflow_run(
            &conn,
            "wf-a",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run1.id,
            crate::workflow::WorkflowRunStatus::Completed,
            None,
            None,
        )
        .unwrap();

        let run2 = crate::workflow::create_workflow_run(
            &conn,
            "wf-b",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run2.id,
            crate::workflow::WorkflowRunStatus::Running,
            None,
            None,
        )
        .unwrap();

        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let provider = WorkflowRunsProvider;
        let items = provider.items(&ctx, None, &HashMap::new()).unwrap();

        // Only the completed run should be returned (running is not terminal).
        let ids: Vec<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
        assert!(
            ids.contains(&run1.id.as_str()),
            "completed run should be included"
        );
        assert!(
            !ids.contains(&run2.id.as_str()),
            "running run should be excluded"
        );
    }

    #[test]
    fn test_status_filter_respected() {
        let conn = test_helpers::setup_db();
        let config = Config::default();

        let parent_id = test_helpers::make_agent_parent_id(&conn);

        let run1 = crate::workflow::create_workflow_run(
            &conn,
            "wf-fail",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run1.id,
            crate::workflow::WorkflowRunStatus::Failed,
            None,
            None,
        )
        .unwrap();

        let run2 = crate::workflow::create_workflow_run(
            &conn,
            "wf-ok",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run2.id,
            crate::workflow::WorkflowRunStatus::Completed,
            None,
            None,
        )
        .unwrap();

        let mut filter = HashMap::new();
        filter.insert("status".to_string(), "failed".to_string());

        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let provider = WorkflowRunsProvider;
        let items = provider.items(&ctx, None, &filter).unwrap();

        let ids: Vec<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
        assert!(
            ids.contains(&run1.id.as_str()),
            "failed run should be included"
        );
        assert!(
            !ids.contains(&run2.id.as_str()),
            "completed run should be excluded when filter=failed"
        );
    }

    #[test]
    fn test_workflow_name_filter_respected() {
        let conn = test_helpers::setup_db();
        let config = Config::default();

        let parent_id = test_helpers::make_agent_parent_id(&conn);

        let run_a = crate::workflow::create_workflow_run(
            &conn,
            "wf-alpha",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run_a.id,
            crate::workflow::WorkflowRunStatus::Completed,
            None,
            None,
        )
        .unwrap();

        let run_b = crate::workflow::create_workflow_run(
            &conn,
            "wf-beta",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run_b.id,
            crate::workflow::WorkflowRunStatus::Completed,
            None,
            None,
        )
        .unwrap();

        let mut filter = HashMap::new();
        filter.insert("workflow_name".to_string(), "wf-alpha".to_string());

        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = WorkflowRunsProvider.items(&ctx, None, &filter).unwrap();

        let ids: Vec<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
        assert!(ids.contains(&run_a.id.as_str()), "wf-alpha run included");
        assert!(!ids.contains(&run_b.id.as_str()), "wf-beta run excluded");
    }

    #[test]
    fn test_workflow_runs_returns_all_without_dedup() {
        let conn = test_helpers::setup_db();
        let config = Config::default();

        let parent_id = test_helpers::make_agent_parent_id(&conn);

        let run1 = crate::workflow::create_workflow_run(
            &conn,
            "wf-x",
            Some("w1"),
            &parent_id,
            false,
            "manual",
            None,
        )
        .unwrap();
        crate::workflow::update_workflow_status(
            &conn,
            &run1.id,
            crate::workflow::WorkflowRunStatus::Completed,
            None,
            None,
        )
        .unwrap();

        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = WorkflowRunsProvider
            .items(&ctx, None, &HashMap::new())
            .unwrap();

        // Providers return ALL items; dedup is done by the foreach executor.
        assert!(
            items.iter().any(|i| i.item_id == run1.id),
            "run should be included regardless of prior state"
        );
    }
}
