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

        let wf_mgr = crate::workflow::manager::WorkflowManager::new(ctx.conn);
        let rows = wf_mgr.list_runs_by_status(&statuses, workflow_name_filter)?;

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
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);

        let run1 = wf_mgr
            .create_workflow_run("wf-a", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run1.id,
                crate::workflow::status::WorkflowRunStatus::Completed,
                None,
                None,
            )
            .unwrap();

        let run2 = wf_mgr
            .create_workflow_run("wf-b", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run2.id,
                crate::workflow::status::WorkflowRunStatus::Running,
                None,
                None,
            )
            .unwrap();

        let ctx = test_helpers::make_provider_ctx(&conn, &config, None, None);
        let provider = WorkflowRunsProvider;
        let items = provider
            .items(&ctx, None, &HashMap::new(), &HashSet::new())
            .unwrap();

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
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);

        let run1 = wf_mgr
            .create_workflow_run("wf-fail", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run1.id,
                crate::workflow::status::WorkflowRunStatus::Failed,
                None,
                None,
            )
            .unwrap();

        let run2 = wf_mgr
            .create_workflow_run("wf-ok", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run2.id,
                crate::workflow::status::WorkflowRunStatus::Completed,
                None,
                None,
            )
            .unwrap();

        let mut filter = HashMap::new();
        filter.insert("status".to_string(), "failed".to_string());

        let ctx = test_helpers::make_provider_ctx(&conn, &config, None, None);
        let provider = WorkflowRunsProvider;
        let items = provider
            .items(&ctx, None, &filter, &HashSet::new())
            .unwrap();

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
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);

        let run_a = wf_mgr
            .create_workflow_run("wf-alpha", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run_a.id,
                crate::workflow::status::WorkflowRunStatus::Completed,
                None,
                None,
            )
            .unwrap();

        let run_b = wf_mgr
            .create_workflow_run("wf-beta", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run_b.id,
                crate::workflow::status::WorkflowRunStatus::Completed,
                None,
                None,
            )
            .unwrap();

        let mut filter = HashMap::new();
        filter.insert("workflow_name".to_string(), "wf-alpha".to_string());

        let ctx = test_helpers::make_provider_ctx(&conn, &config, None, None);
        let items = WorkflowRunsProvider
            .items(&ctx, None, &filter, &HashSet::new())
            .unwrap();

        let ids: Vec<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
        assert!(ids.contains(&run_a.id.as_str()), "wf-alpha run included");
        assert!(!ids.contains(&run_b.id.as_str()), "wf-beta run excluded");
    }

    #[test]
    fn test_existing_set_deduplication() {
        let conn = test_helpers::setup_db();
        let config = Config::default();

        let parent_id = test_helpers::make_agent_parent_id(&conn);
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);

        let run1 = wf_mgr
            .create_workflow_run("wf-x", Some("w1"), &parent_id, false, "manual", None)
            .unwrap();
        wf_mgr
            .update_workflow_status(
                &run1.id,
                crate::workflow::status::WorkflowRunStatus::Completed,
                None,
                None,
            )
            .unwrap();

        let mut existing = HashSet::new();
        existing.insert(run1.id.clone());

        let ctx = test_helpers::make_provider_ctx(&conn, &config, None, None);
        let items = WorkflowRunsProvider
            .items(&ctx, None, &HashMap::new(), &existing)
            .unwrap();

        assert!(
            items.is_empty(),
            "run already in existing_set should be excluded"
        );
    }
}
