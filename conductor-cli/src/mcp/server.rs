use std::path::PathBuf;

use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ResourcesCapability, ServerCapabilities,
    ServerInfo, ToolsCapability,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};

/// The conductor MCP server. Holds only the DB path — each request opens its
/// own connection inside `spawn_blocking` to avoid the `!Send` issue.
pub struct ConductorMcpServer {
    db_path: PathBuf,
}

impl ConductorMcpServer {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
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

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
    {
        let db_path = self.db_path.clone();
        async move {
            let resources =
                tokio::task::spawn_blocking(move || super::resources::enumerate_resources(&db_path))
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
                    .map_err(|e: anyhow::Error| {
                        rmcp::ErrorData::internal_error(e.to_string(), None)
                    })?;

            Ok(ListResourcesResult::with_all_items(resources))
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_
    {
        let db_path = self.db_path.clone();
        let uri = request.uri.clone();
        async move {
            let text = tokio::task::spawn_blocking(move || {
                super::resources::read_resource_by_uri(&db_path, &uri)
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

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_
    {
        let db_path = self.db_path.clone();
        let name = request.name.to_string();
        let args = request.arguments.unwrap_or_default();
        async move {
            let result = tokio::task::spawn_blocking(move || {
                super::tools::dispatch_tool(&db_path, &name, &args)
            })
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

            Ok(result)
        }
    }
}
