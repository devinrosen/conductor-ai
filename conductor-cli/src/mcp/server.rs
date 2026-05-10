use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
    ResourcesCapability, ServerCapabilities, ServerInfo, SubscribeRequestParams, ToolsCapability,
    UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};

use super::subscriptions::{spawn_broadcaster, SubscriptionHub};

fn is_terminal(status: Option<&str>) -> bool {
    status.is_some_and(|s| matches!(s, "completed" | "failed" | "cancelled"))
}

/// The conductor MCP server. Each request opens its own `Conductor` inside
/// `spawn_blocking` to avoid the `!Send` issue with `rusqlite::Connection`.
pub struct ConductorMcpServer {
    hub: SubscriptionHub,
    _broadcaster_handle: tokio::task::JoinHandle<()>,
}

impl ConductorMcpServer {
    pub fn new() -> Self {
        let (hub, rx) = SubscriptionHub::new();
        let handle = spawn_broadcaster(rx, hub.registry.clone());
        Self {
            hub,
            _broadcaster_handle: handle,
        }
    }
}

impl ServerHandler for ConductorMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut caps = ServerCapabilities::default();
        caps.resources = Some(ResourcesCapability {
            subscribe: Some(true),
            list_changed: Some(false),
        });
        caps.tools = Some(ToolsCapability {
            list_changed: Some(false),
        });
        ServerInfo::new(caps)
            .with_server_info(Implementation::new("conductor", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Conductor MCP server: access repos, tickets, worktrees, and workflow runs.",
            )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        let resources = tokio::task::spawn_blocking(move || {
            let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from)?;
            super::resources::enumerate_resources(&conductor)
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
        .map_err(|e: anyhow::Error| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::ErrorData> {
        let uri = request.uri.clone();
        let text = tokio::task::spawn_blocking(move || {
            let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from)?;
            super::resources::read_resource_by_uri(&conductor, &uri)
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
        .map_err(|e: anyhow::Error| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: request.uri,
                mime_type: Some("text/plain".into()),
                text,
                meta: None,
            },
        ]))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(
            super::tools::conductor_tools(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let name = request.name.to_string();
        let args = request.arguments.unwrap_or_default();
        let hub = self.hub.clone();
        let result = tokio::task::spawn_blocking(move || {
            let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from);
            match conductor {
                Ok(c) => super::tools::dispatch_tool(&c, &name, &args, Some(&hub)),
                Err(e) => crate::mcp::helpers::tool_err(e),
            }
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        Ok(result)
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let uri = request.uri.clone();

        let run_id = uri
            .strip_prefix("conductor://run/")
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    "URI must be conductor://run/{run_id}".to_string(),
                    None,
                )
            })?
            .to_string();

        let run_id_for_lookup = run_id.clone();
        let status = tokio::task::spawn_blocking(move || {
            let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from)?;
            conductor_core::workflow::get_workflow_run_status(&conductor.conn, &run_id_for_lookup)
                .map_err(anyhow::Error::from)
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
        .map_err(|e: anyhow::Error| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        if status.is_none() {
            return Err(rmcp::ErrorData::invalid_params(
                format!("unknown run_id: {run_id}"),
                None,
            ));
        }

        if is_terminal(status.as_deref()) {
            // Run is already terminal — fire one notification immediately (option a from the ticket).
            // Don't insert into the registry; client will re-read the resource.
            let _ = context
                .peer
                .notify_resource_updated(rmcp::model::ResourceUpdatedNotificationParam::new(uri))
                .await;
        } else {
            let run_id_recheck = run_id.clone();
            self.hub
                .registry
                .insert(run_id.clone(), context.peer.clone(), uri);
            // Close TOCTOU: re-check status after insert to catch terminal transition in the gap.
            let recheck = tokio::task::spawn_blocking(move || {
                let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from)?;
                conductor_core::workflow::get_workflow_run_status(&conductor.conn, &run_id_recheck)
                    .map_err(anyhow::Error::from)
            })
            .await;
            match recheck {
                Err(join_err) => {
                    tracing::warn!(
                        "TOCTOU recheck task panicked for run {run_id}: {join_err}; \
                         draining subscriber to avoid orphan"
                    );
                    self.hub.notify_and_drain(&run_id).await;
                }
                Ok(Err(db_err)) => {
                    tracing::warn!(
                        "TOCTOU recheck DB error for run {run_id}: {db_err}; \
                         draining subscriber to avoid orphan"
                    );
                    self.hub.notify_and_drain(&run_id).await;
                }
                Ok(Ok(status)) => {
                    if is_terminal(status.as_deref()) {
                        self.hub.notify_and_drain(&run_id).await;
                    }
                }
            }
        }

        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let uri = request.uri;
        if let Some(run_id) = uri.strip_prefix("conductor://run/") {
            self.hub.registry.remove(run_id, &uri);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_terminal_recognises_all_terminal_states() {
        assert!(is_terminal(Some("completed")));
        assert!(is_terminal(Some("failed")));
        assert!(is_terminal(Some("cancelled")));
    }

    #[test]
    fn test_is_terminal_rejects_nonterminal_and_none() {
        assert!(!is_terminal(Some("running")));
        assert!(!is_terminal(Some("pending")));
        assert!(!is_terminal(Some("queued")));
        assert!(!is_terminal(None));
    }
}
