use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
    ResourcesCapability, ServerCapabilities, ServerInfo, ToolsCapability,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};

/// The conductor MCP server. Each request opens its own `Conductor` inside
/// `spawn_blocking` to avoid the `!Send` issue with `rusqlite::Connection`.
pub struct ConductorMcpServer {}

impl ConductorMcpServer {
    pub fn new() -> Self {
        Self {}
    }
}

impl ServerHandler for ConductorMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut caps = ServerCapabilities::default();
        caps.resources = Some(ResourcesCapability {
            subscribe: Some(false),
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
            let conductor =
                conductor_core::Conductor::open().map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
            let conductor =
                conductor_core::Conductor::open().map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
        let result = tokio::task::spawn_blocking(move || {
            let conductor =
                conductor_core::Conductor::open().map_err(|e| anyhow::anyhow!(e.to_string()));
            match conductor {
                Ok(c) => super::tools::dispatch_tool(&c, &name, &args),
                Err(e) => crate::mcp::helpers::tool_err(e),
            }
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        Ok(result)
    }
}
