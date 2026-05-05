use std::any::Any;
use std::collections::HashMap;

use rusqlite::Connection;

use crate::error::{ConductorError, Result};

use super::{
    collect_fan_out_items, dep_query_err, fetch_dep_item_ids, ids_to_set_and_refs, require_repo_id,
    FanOutItem, ItemProvider, ProviderContext, TicketScope,
};

pub struct TicketsProvider {
    repo_id: Option<String>,
}

impl TicketsProvider {
    pub fn new(repo_id: Option<String>) -> Self {
        Self { repo_id }
    }
}

impl ItemProvider for TicketsProvider {
    fn name(&self) -> &str {
        "tickets"
    }

    fn parse_scope(
        &self,
        raw: Option<&HashMap<String, String>>,
    ) -> crate::error::Result<Option<Box<dyn Any>>> {
        let map = match raw {
            None => {
                return Err(ConductorError::Workflow(format!(
                    "foreach '{}': `scope` is required when over = tickets",
                    self.name()
                )));
            }
            Some(m) => m,
        };

        let scope = if let Some(ticket_id) = map.get("ticket_id") {
            TicketScope::TicketId(ticket_id.clone())
        } else if let Some(label) = map.get("label") {
            TicketScope::Label(label.clone())
        } else if let Some(v) = map.get("unlabeled") {
            if v == "true" {
                TicketScope::Unlabeled
            } else {
                return Err(ConductorError::Workflow(
                    "scope.unlabeled must be true".to_string(),
                ));
            }
        } else {
            return Err(ConductorError::Workflow(
                "scope must contain ticket_id, label, or unlabeled".to_string(),
            ));
        };

        Ok(Some(Box::new(scope)))
    }

    fn items(
        &self,
        ctx: &ProviderContext<'_>,
        scope: Option<&dyn Any>,
        _filter: &HashMap<String, String>,
    ) -> Result<Vec<FanOutItem>> {
        use crate::tickets::TicketSyncer;

        let syncer = TicketSyncer::new(ctx.conn);
        let repo_id = require_repo_id(&self.repo_id, "tickets")?;

        let ticket_item = |t: crate::tickets::Ticket| {
            let mut context = std::collections::HashMap::new();
            context.insert("title".to_string(), t.title.clone());
            context.insert("url".to_string(), t.url.clone());
            context.insert("source_id".to_string(), t.source_id.clone());
            context.insert("state".to_string(), t.state.clone());
            context.insert("labels".to_string(), t.labels.clone());
            FanOutItem {
                item_type: "ticket".to_string(),
                item_id: t.id,
                item_ref: t.source_id,
                context,
            }
        };

        let ts_opt = scope.and_then(|s| s.downcast_ref::<TicketScope>());

        let items = match ts_opt {
            Some(TicketScope::TicketId(ticket_id)) => {
                match syncer.get_by_id(ticket_id) {
                    Ok(t) => vec![ticket_item(t)],
                    Err(ConductorError::TicketNotFound { .. }) => {
                        return Err(ConductorError::Workflow(format!(
                            "foreach: ticket '{}' not found",
                            ticket_id
                        )));
                    }
                    Err(e) => return Err(e),
                }
            }
            Some(TicketScope::Label(label)) => {
                let tickets = syncer
                    .list_filtered(Some(repo_id), &ticket_filter(vec![label.clone()], false))?;
                collect_fan_out_items(tickets, ticket_item)
            }
            Some(TicketScope::Unlabeled) => {
                let tickets =
                    syncer.list_filtered(Some(repo_id), &ticket_filter(vec![], true))?;
                collect_fan_out_items(tickets, ticket_item)
            }
            None => {
                let tickets = syncer.list_filtered(Some(repo_id), &ticket_filter(vec![], false))?;
                collect_fan_out_items(tickets, ticket_item)
            }
        };

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
        let Some(item_ids) = fetch_dep_item_ids(conn, step_id)? else {
            return Ok(vec![]);
        };

        let syncer = crate::tickets::TicketSyncer::new(conn);
        let (id_set, ticket_id_refs) = ids_to_set_and_refs(&item_ids);
        let raw_edges = syncer
            .get_blocking_edges_for_tickets(&ticket_id_refs)
            .map_err(dep_query_err)?;

        let edges: Vec<(String, String)> = raw_edges
            .into_iter()
            .filter(|(blocker_id, dependent_id)| {
                id_set.contains(blocker_id) && id_set.contains(dependent_id)
            })
            .collect();
        Ok(edges)
    }
}

fn ticket_filter(labels: Vec<String>, unlabeled_only: bool) -> crate::tickets::TicketFilter {
    crate::tickets::TicketFilter {
        labels,
        search: None,
        include_closed: false,
        unlabeled_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers;
    use crate::tickets::TicketSyncer;

    #[test]
    fn test_tickets_items_missing_repo_id_returns_error() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let result = TicketsProvider::new(None).items(&ctx, None, &HashMap::new());
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
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = TicketsProvider::new(Some("r1".into()))
            .items(&ctx, None, &HashMap::new())
            .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.item_type == "ticket"));
    }

    #[test]
    fn test_tickets_parse_scope_rejects_worktree_keys() {
        let mut raw = HashMap::new();
        raw.insert("base_branch".to_string(), "main".to_string());
        let result = TicketsProvider::new(Some("r1".into())).parse_scope(Some(&raw));
        // "base_branch" is not ticket_id/label/unlabeled → error
        assert!(result.is_err());
    }

    #[test]
    fn test_tickets_parse_scope_rejects_none() {
        let result = TicketsProvider::new(Some("r1".into())).parse_scope(None);
        assert!(result.is_err(), "tickets requires a scope");
    }

    #[test]
    fn test_tickets_items_ticket_id_scope() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        syncer
            .upsert_tickets("r1", &[test_helpers::make_ticket("42", "Target")])
            .unwrap();
        let all = syncer
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
        assert_eq!(all.len(), 1);
        let internal_id = all[0].id.clone();

        let scope = TicketScope::TicketId(internal_id.clone());
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = TicketsProvider::new(Some("r1".into()))
            .items(&ctx, Some(&scope as &dyn std::any::Any), &HashMap::new())
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, internal_id);
    }

    #[test]
    fn test_tickets_items_label_scope() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        let labeled = crate::tickets::TicketInput {
            labels: vec!["bug".to_string()],
            label_details: vec![crate::tickets::TicketLabelInput {
                name: "bug".to_string(),
                color: None,
            }],
            ..test_helpers::make_ticket("10", "Labeled")
        };
        syncer
            .upsert_tickets("r1", &[labeled, test_helpers::make_ticket("11", "Plain")])
            .unwrap();

        let scope = TicketScope::Label("bug".to_string());
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = TicketsProvider::new(Some("r1".into()))
            .items(&ctx, Some(&scope as &dyn std::any::Any), &HashMap::new())
            .unwrap();
        assert_eq!(items.len(), 1, "only the labeled ticket returned");
        assert_eq!(items[0].item_ref, "10");
    }

    #[test]
    fn test_tickets_items_unlabeled_scope() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);
        let labeled = crate::tickets::TicketInput {
            labels: vec!["enhancement".to_string()],
            label_details: vec![crate::tickets::TicketLabelInput {
                name: "enhancement".to_string(),
                color: None,
            }],
            ..test_helpers::make_ticket("20", "Labeled")
        };
        syncer
            .upsert_tickets(
                "r1",
                &[labeled, test_helpers::make_ticket("21", "Unlabeled")],
            )
            .unwrap();

        let scope = TicketScope::Unlabeled;
        let ctx = test_helpers::make_provider_ctx(&conn, &config);
        let items = TicketsProvider::new(Some("r1".into()))
            .items(&ctx, Some(&scope as &dyn std::any::Any), &HashMap::new())
            .unwrap();
        assert_eq!(items.len(), 1, "only unlabeled ticket returned");
        assert_eq!(items[0].item_ref, "21");
    }

    #[test]
    fn test_tickets_dependencies_empty_when_no_items() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let edges = TicketsProvider::new(None)
            .dependencies(&conn, &config, "nonexistent-step")
            .unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn test_tickets_dependencies_returns_edges_within_set() {
        let conn = test_helpers::setup_db();
        let config = crate::config::Config::default();
        let syncer = TicketSyncer::new(&conn);

        // ticket "2" is blocked by ticket "1"
        let t1 = test_helpers::make_ticket("1", "Blocker");
        let t2 = crate::tickets::TicketInput {
            blocked_by: vec!["1".to_string()],
            ..test_helpers::make_ticket("2", "Dependent")
        };
        syncer.upsert_tickets("r1", &[t1, t2]).unwrap();

        let all = syncer
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
        assert_eq!(all.len(), 2);
        let by_src: std::collections::HashMap<_, _> = all
            .iter()
            .map(|t| (t.source_id.as_str(), t.id.as_str()))
            .collect();
        let id1 = by_src["1"].to_string();
        let id2 = by_src["2"].to_string();

        let step_id = test_helpers::make_foreach_step(&conn);
        crate::workflow::insert_fan_out_item(&conn, &step_id, "ticket", &id1, "1").unwrap();
        crate::workflow::insert_fan_out_item(&conn, &step_id, "ticket", &id2, "2").unwrap();

        let edges = TicketsProvider::new(None)
            .dependencies(&conn, &config, &step_id)
            .unwrap();
        assert_eq!(edges.len(), 1, "one blocking edge expected");
        assert_eq!(edges[0], (id1, id2), "blocker → dependent");
    }
}
