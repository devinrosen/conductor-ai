use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::error::{ConductorError, Result};
use crate::workflow_dsl::{ForeachScope, TicketScope};

use super::{FanOutItem, ItemProvider, ProviderContext};

pub struct TicketsProvider;

impl ItemProvider for TicketsProvider {
    fn name(&self) -> &str {
        "tickets"
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        scope: Option<&ForeachScope>,
        _filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<FanOutItem>> {
        use crate::tickets::{TicketFilter, TicketSyncer};

        let syncer = TicketSyncer::new(ctx.conn);
        let repo_id = ctx.repo_id.ok_or_else(|| {
            ConductorError::Workflow(
                "foreach over tickets requires a repo_id in the execution context".to_string(),
            )
        })?;

        let mut items = Vec::new();

        match scope {
            Some(ForeachScope::Ticket(ts)) => match ts {
                TicketScope::TicketId(ticket_id) => match syncer.get_by_id(ticket_id) {
                    Ok(t) if !existing_set.contains(&t.id) => {
                        items.push(FanOutItem {
                            item_type: "ticket".to_string(),
                            item_id: t.id.clone(),
                            item_ref: t.source_id.clone(),
                        });
                    }
                    Ok(_) => {}
                    Err(ConductorError::TicketNotFound { .. }) => {
                        return Err(ConductorError::Workflow(format!(
                            "foreach: ticket '{}' not found",
                            ticket_id
                        )));
                    }
                    Err(e) => return Err(e),
                },
                TicketScope::Label(label) => {
                    let filter = TicketFilter {
                        labels: vec![label.clone()],
                        search: None,
                        include_closed: false,
                        unlabeled_only: false,
                    };
                    let tickets = syncer.list_filtered(Some(repo_id), &filter)?;
                    for t in tickets {
                        if !existing_set.contains(&t.id) {
                            items.push(FanOutItem {
                                item_type: "ticket".to_string(),
                                item_id: t.id.clone(),
                                item_ref: t.source_id.clone(),
                            });
                        }
                    }
                }
                TicketScope::Unlabeled => {
                    let filter = TicketFilter {
                        labels: vec![],
                        search: None,
                        include_closed: false,
                        unlabeled_only: true,
                    };
                    let tickets = syncer.list_filtered(Some(repo_id), &filter)?;
                    for t in tickets {
                        if !existing_set.contains(&t.id) {
                            items.push(FanOutItem {
                                item_type: "ticket".to_string(),
                                item_id: t.id.clone(),
                                item_ref: t.source_id.clone(),
                            });
                        }
                    }
                }
            },
            Some(ForeachScope::Worktree(_)) => {
                return Err(ConductorError::Workflow(
                    "foreach over = tickets does not accept a worktree scope; use over = worktrees instead".to_string(),
                ));
            }
            None => {
                let filter = TicketFilter {
                    labels: vec![],
                    search: None,
                    include_closed: false,
                    unlabeled_only: false,
                };
                let tickets = syncer.list_filtered(Some(repo_id), &filter)?;
                for t in tickets {
                    if !existing_set.contains(&t.id) {
                        items.push(FanOutItem {
                            item_type: "ticket".to_string(),
                            item_id: t.id.clone(),
                            item_ref: t.source_id.clone(),
                        });
                    }
                }
            }
        }

        Ok(items)
    }

    fn supports_ordered(&self) -> bool {
        true
    }

    fn dependencies(
        &self,
        conn: &Connection,
        _config: &crate::config::Config,
        step_id: &str,
    ) -> Result<Vec<(String, String)>> {
        use std::collections::HashSet;

        let mgr = crate::workflow::manager::WorkflowManager::new(conn);
        let items = mgr.get_fan_out_items(step_id, None)?;
        let item_ids: Vec<String> = items.iter().map(|i| i.item_id.clone()).collect();
        if item_ids.is_empty() {
            return Ok(vec![]);
        }

        let syncer = crate::tickets::TicketSyncer::new(conn);
        let id_set: HashSet<&String> = item_ids.iter().collect();
        let ticket_id_refs: Vec<&str> = item_ids.iter().map(String::as_str).collect();
        let raw_edges = syncer
            .get_blocking_edges_for_tickets(&ticket_id_refs)
            .map_err(|e| {
                ConductorError::Workflow(format!("foreach: dependency query failed: {e}"))
            })?;

        let edges: Vec<(String, String)> = raw_edges
            .into_iter()
            .filter(|(blocker_id, dependent_id)| {
                id_set.contains(blocker_id) && id_set.contains(dependent_id)
            })
            .collect();
        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;
    use crate::tickets::TicketSyncer;

    fn make_ctx<'a>(
        conn: &'a rusqlite::Connection,
        config: &'a crate::config::Config,
        repo_id: Option<&'a str>,
    ) -> ProviderContext<'a> {
        ProviderContext {
            conn,
            config,
            repo_id,
            worktree_id: None,
        }
    }

    #[test]
    fn test_tickets_items_missing_repo_id_returns_error() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = make_ctx(&conn, &config, None);
        let result = TicketsProvider.items(&ctx, None, &HashMap::new(), &HashSet::new());
        assert!(
            result.is_err(),
            "items() without repo_id should return an error"
        );
        let Err(e) = result else {
            panic!("expected error")
        };
        let msg = e.to_string();
        assert!(
            msg.contains("repo_id"),
            "error message should mention repo_id"
        );
    }

    #[test]
    fn test_tickets_items_no_scope_returns_all() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        syncer
            .upsert_tickets(
                "r1",
                &[
                    test_helpers::make_ticket("1", "A"),
                    test_helpers::make_ticket("2", "B"),
                ],
            )
            .unwrap();
        let ctx = make_ctx(&conn, &config, Some("r1"));
        let items = TicketsProvider
            .items(&ctx, None, &HashMap::new(), &HashSet::new())
            .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.item_type == "ticket"));
    }

    #[test]
    fn test_tickets_items_skips_existing_set() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        syncer
            .upsert_tickets("r1", &[test_helpers::make_ticket("1", "A")])
            .unwrap();
        // Fetch the inserted ticket to get its ID.
        use crate::tickets::TicketFilter;
        let all = syncer
            .list_filtered(
                Some("r1"),
                &TicketFilter {
                    labels: vec![],
                    search: None,
                    include_closed: false,
                    unlabeled_only: false,
                },
            )
            .unwrap();
        assert_eq!(all.len(), 1);
        let mut existing = HashSet::new();
        existing.insert(all[0].id.clone());
        let ctx = make_ctx(&conn, &config, Some("r1"));
        let items = TicketsProvider
            .items(&ctx, None, &HashMap::new(), &existing)
            .unwrap();
        assert!(
            items.is_empty(),
            "ticket already in existing_set should be skipped"
        );
    }

    #[test]
    fn test_tickets_items_worktree_scope_returns_error() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = make_ctx(&conn, &config, Some("r1"));
        let scope = ForeachScope::Worktree(crate::workflow_dsl::WorktreeScope {
            base_branch: None,
            has_open_pr: None,
        });
        let result = TicketsProvider.items(&ctx, Some(&scope), &HashMap::new(), &HashSet::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_tickets_dependencies_empty_when_no_items() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let edges = TicketsProvider
            .dependencies(&conn, &config, "nonexistent-step")
            .unwrap();
        assert!(edges.is_empty());
    }
}
