use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::workflow_dsl::ForeachScope;
use crate::worktree::{Worktree, WorktreeManager};

use super::{FanOutItem, ItemProvider, ProviderContext};

pub struct WorktreesProvider;

impl ItemProvider for WorktreesProvider {
    fn name(&self) -> &str {
        "worktrees"
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        scope: Option<&ForeachScope>,
        _filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<FanOutItem>> {
        let repo_id = ctx.repo_id.ok_or_else(|| {
            ConductorError::Workflow(
                "foreach over worktrees requires a repo_id in the execution context".to_string(),
            )
        })?;

        let wt_scope_opt = match scope {
            Some(ForeachScope::Worktree(s)) => Some(s),
            _ => None,
        };

        let base_branch_owned: String;
        let base_branch: &str = match wt_scope_opt.and_then(|s| s.base_branch.as_deref()) {
            Some(b) => b,
            None => {
                let wt_id = ctx.worktree_id.ok_or_else(|| {
                    ConductorError::Workflow(
                        "foreach over worktrees requires either scope = { base_branch = \"...\" } \
                         or a worktree_id in the execution context"
                            .to_string(),
                    )
                })?;
                let wt = WorktreeManager::new(ctx.conn, ctx.config).get_by_id(wt_id)?;
                base_branch_owned = wt.branch.clone();
                &base_branch_owned
            }
        };

        let wt_mgr = WorktreeManager::new(ctx.conn, ctx.config);
        let active_worktrees = wt_mgr.list_by_repo_id_and_base_branch(repo_id, base_branch)?;

        let mut candidates: Vec<_> = active_worktrees
            .into_iter()
            .filter(|wt| !existing_set.contains(&wt.id))
            .collect();

        if let Some(want_open_pr) = wt_scope_opt.and_then(|s| s.has_open_pr) {
            let repo = crate::repo::RepoManager::new(ctx.conn, ctx.config).get_by_id(repo_id)?;
            let open_prs = crate::github::list_open_prs(&repo.remote_url)?;
            candidates = filter_by_open_pr(candidates, want_open_pr, open_prs);
        }

        Ok(candidates
            .into_iter()
            .map(|wt| FanOutItem {
                item_type: "worktree".to_string(),
                item_id: wt.id,
                item_ref: wt.slug,
            })
            .collect())
    }

    fn supports_ordered(&self) -> bool {
        true
    }

    fn dependencies(
        &self,
        conn: &Connection,
        config: &Config,
        step_id: &str,
    ) -> Result<Vec<(String, String)>> {
        dependencies_impl(conn, config, step_id)
    }
}

fn filter_by_open_pr(
    mut candidates: Vec<Worktree>,
    want_open_pr: bool,
    open_prs: Vec<crate::github::GithubPr>,
) -> Vec<Worktree> {
    let open_branches: HashSet<String> = open_prs.into_iter().map(|pr| pr.head_ref_name).collect();
    candidates.retain(|wt| open_branches.contains(&wt.branch) == want_open_pr);
    candidates
}

fn dependencies_impl(
    conn: &Connection,
    config: &Config,
    step_id: &str,
) -> Result<Vec<(String, String)>> {
    let mgr = crate::workflow::manager::WorkflowManager::new(conn);
    let items = mgr.get_fan_out_items(step_id, None)?;
    let item_ids: Vec<String> = items.iter().map(|i| i.item_id.clone()).collect();
    if item_ids.is_empty() {
        return Ok(vec![]);
    }

    let id_set: HashSet<&String> = item_ids.iter().collect();
    let id_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
    let wt_mgr = WorktreeManager::new(conn, config);
    let worktrees = match wt_mgr.get_by_ids(&id_refs) {
        Ok(wts) => wts,
        Err(e) => {
            tracing::warn!(
                "foreach: could not fetch worktrees for dep edges: {e}; skipping ordering"
            );
            return Ok(vec![]);
        }
    };
    let mut wt_ticket_map: HashMap<String, String> = HashMap::new();
    for wt in worktrees {
        if let Some(tid) = wt.ticket_id {
            wt_ticket_map.insert(wt.id, tid);
        }
    }

    let ticket_wt_map: HashMap<String, String> = wt_ticket_map
        .iter()
        .map(|(wt_id, tid)| (tid.clone(), wt_id.clone()))
        .collect();

    let ticket_ids: Vec<String> = wt_ticket_map.values().cloned().collect();
    if ticket_ids.is_empty() {
        return Ok(vec![]);
    }
    let ticket_id_refs: Vec<&str> = ticket_ids.iter().map(String::as_str).collect();
    let syncer = crate::tickets::TicketSyncer::new(conn);
    let dep_edges = syncer
        .get_blocking_edges_for_tickets(&ticket_id_refs)
        .map_err(|e| ConductorError::Workflow(format!("foreach: dependency query failed: {e}")))?;

    let mut edges: HashSet<(String, String)> = HashSet::new();
    for (blocker_ticket_id, dependent_ticket_id) in dep_edges {
        if let (Some(blocker_wt_id), Some(dependent_wt_id)) = (
            ticket_wt_map.get(&blocker_ticket_id),
            ticket_wt_map.get(&dependent_ticket_id),
        ) {
            if id_set.contains(blocker_wt_id) && id_set.contains(dependent_wt_id) {
                edges.insert((blocker_wt_id.clone(), dependent_wt_id.clone()));
            }
        }
    }

    Ok(edges.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;
    use crate::workflow_dsl::WorktreeScope;

    #[test]
    fn test_worktrees_items_missing_repo_id_returns_error() {
        let conn = test_helpers::setup_db();
        let config = Config::default();
        let ctx = test_helpers::make_provider_ctx(&conn, &config, None, None);
        let result = WorktreesProvider.items(&ctx, None, &HashMap::new(), &HashSet::new());
        assert!(result.is_err());
        let Err(e) = result else {
            panic!("expected error")
        };
        assert!(
            e.to_string().contains("repo_id"),
            "error should mention repo_id"
        );
    }

    #[test]
    fn test_worktrees_items_no_scope_no_worktree_id_returns_error() {
        let conn = test_helpers::setup_db();
        let config = Config::default();
        // repo_id present but no scope and no worktree_id
        let ctx = test_helpers::make_provider_ctx(&conn, &config, Some("r1"), None);
        let result = WorktreesProvider.items(&ctx, None, &HashMap::new(), &HashSet::new());
        assert!(result.is_err());
        let Err(e) = result else {
            panic!("expected error")
        };
        let msg = e.to_string();
        assert!(
            msg.contains("base_branch") || msg.contains("worktree_id"),
            "error should mention missing context"
        );
    }

    fn insert_worktree_with_base_branch(
        conn: &rusqlite::Connection,
        id: &str,
        repo_id: &str,
        slug: &str,
        base_branch: &str,
    ) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, base_branch, created_at) \
             VALUES (:id, :repo_id, :slug, :base_branch, '/tmp/ws', 'active', :base_branch, '2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":id": id, ":repo_id": repo_id, ":slug": slug, ":base_branch": base_branch },
        ).unwrap();
    }

    #[test]
    fn test_worktrees_items_with_base_branch_scope() {
        let conn = test_helpers::setup_db();
        insert_worktree_with_base_branch(&conn, "w2", "r1", "feat-child", "main");
        let config = Config::default();
        let scope = ForeachScope::Worktree(WorktreeScope {
            base_branch: Some("main".to_string()),
            has_open_pr: None,
        });
        let ctx = test_helpers::make_provider_ctx(&conn, &config, Some("r1"), None);
        let items = WorktreesProvider
            .items(&ctx, Some(&scope), &HashMap::new(), &HashSet::new())
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "w2");
        assert_eq!(items[0].item_type, "worktree");
    }

    #[test]
    fn test_worktrees_items_skips_existing_set() {
        let conn = test_helpers::setup_db();
        insert_worktree_with_base_branch(&conn, "w2", "r1", "feat-child", "main");
        let config = Config::default();
        let scope = ForeachScope::Worktree(WorktreeScope {
            base_branch: Some("main".to_string()),
            has_open_pr: None,
        });
        let mut existing = HashSet::new();
        existing.insert("w2".to_string());
        let ctx = test_helpers::make_provider_ctx(&conn, &config, Some("r1"), None);
        let items = WorktreesProvider
            .items(&ctx, Some(&scope), &HashMap::new(), &existing)
            .unwrap();
        assert!(
            items.is_empty(),
            "worktree already in existing_set should be skipped"
        );
    }

    #[test]
    fn test_worktrees_items_worktree_id_fallback() {
        // w1 from setup_db has branch='feat/test'; insert w2 with base_branch='feat/test'
        let conn = test_helpers::setup_db();
        insert_worktree_with_base_branch(&conn, "w2", "r1", "feat-child", "feat/test");
        let config = Config::default();
        // No scope — should resolve base_branch from w1.branch
        let ctx = test_helpers::make_provider_ctx(&conn, &config, Some("r1"), Some("w1"));
        let items = WorktreesProvider
            .items(&ctx, None, &HashMap::new(), &HashSet::new())
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "w2");
    }

    #[test]
    fn test_filter_by_open_pr_keeps_matching() {
        use crate::github::GithubPr;
        use crate::worktree::WorktreeStatus;

        let wts = vec![
            Worktree {
                id: "w1".into(),
                repo_id: "r1".into(),
                slug: "s1".into(),
                branch: "feat/a".into(),
                path: "/tmp/a".into(),
                ticket_id: None,
                status: WorktreeStatus::Active,
                created_at: "2024-01-01T00:00:00Z".into(),
                completed_at: None,
                model: None,
                base_branch: None,
            },
            Worktree {
                id: "w2".into(),
                repo_id: "r1".into(),
                slug: "s2".into(),
                branch: "feat/b".into(),
                path: "/tmp/b".into(),
                ticket_id: None,
                status: WorktreeStatus::Active,
                created_at: "2024-01-01T00:00:00Z".into(),
                completed_at: None,
                model: None,
                base_branch: None,
            },
        ];
        let prs = vec![GithubPr {
            number: 1,
            title: "PR A".into(),
            url: String::new(),
            author: String::new(),
            state: "OPEN".into(),
            head_ref_name: "feat/a".into(),
            is_draft: false,
            review_decision: None,
            ci_status: String::new(),
        }];

        let with_pr = filter_by_open_pr(wts.clone(), true, prs.clone());
        assert_eq!(with_pr.len(), 1);
        assert_eq!(with_pr[0].id, "w1");

        let without_pr = filter_by_open_pr(wts, false, prs);
        assert_eq!(without_pr.len(), 1);
        assert_eq!(without_pr[0].id, "w2");
    }

    #[test]
    fn test_worktrees_dependencies_empty_when_no_items() {
        let conn = test_helpers::setup_db();
        let config = Config::default();
        let edges = WorktreesProvider
            .dependencies(&conn, &config, "nonexistent-step")
            .unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn test_worktrees_dependencies_returns_edges_via_tickets() {
        let conn = test_helpers::setup_db();
        let config = Config::default();

        // Insert tickets: t2 blocked by t1.
        let syncer = crate::tickets::TicketSyncer::new(&conn);
        let t1_input = test_helpers::make_ticket("t1", "Blocker");
        let t2_input = crate::tickets::TicketInput {
            blocked_by: vec!["t1".to_string()],
            ..test_helpers::make_ticket("t2", "Dependent")
        };
        syncer.upsert_tickets("r1", &[t1_input, t2_input]).unwrap();

        let all_tickets = syncer
            .list_filtered(
                Some("r1"),
                &crate::tickets::TicketFilter {
                    labels: vec![],
                    search: None,
                    include_closed: false,
                    unlabeled_only: false,
                },
            )
            .unwrap();
        let by_src: std::collections::HashMap<_, _> = all_tickets
            .iter()
            .map(|t| (t.source_id.as_str(), t.id.as_str()))
            .collect();
        let tid1 = by_src["t1"].to_string();
        let tid2 = by_src["t2"].to_string();

        // Insert worktrees linked to the tickets.
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, ticket_id, base_branch, created_at) \
             VALUES ('wt1', 'r1', 'wt1-slug', 'feat/wt1', '/tmp/wt1', 'active', :tid1, 'main', '2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":tid1": &tid1 },
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, ticket_id, base_branch, created_at) \
             VALUES ('wt2', 'r1', 'wt2-slug', 'feat/wt2', '/tmp/wt2', 'active', :tid2, 'main', '2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":tid2": &tid2 },
        ).unwrap();

        // Create workflow run + step + fan-out items.
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(Some("w1"), "workflow", None, None)
            .unwrap();
        let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        let step_id = wf_mgr
            .insert_step(&run.id, "foreach-step", "foreach", false, 0, 0)
            .unwrap();
        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt1", "wt1-slug")
            .unwrap();
        wf_mgr
            .insert_fan_out_item(&step_id, "worktree", "wt2", "wt2-slug")
            .unwrap();

        let edges = WorktreesProvider
            .dependencies(&conn, &config, &step_id)
            .unwrap();
        assert_eq!(edges.len(), 1, "one blocking edge expected");
        assert_eq!(
            edges[0],
            ("wt1".to_string(), "wt2".to_string()),
            "wt1 blocks wt2"
        );
    }
}
