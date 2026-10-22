use anyhow::Result;
use conductor_core::workflow::WorkflowRunStatus;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
    ResourcesCapability, ServerCapabilities, ServerInfo, SubscribeRequestParams, ToolsCapability,
    UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use std::time::Duration;

use super::subscriptions::SubscriptionHub;

const RUN_URI_PREFIX: &str = "conductor://run/";

/// Upper bound for read-only DB tools: two `busy_timeout` waits (5s each).
const READ_TOOL_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for DB-only read tools. Tools that mutate state, run workflows, or
/// shell out to the network get none: timing out would report failure for work
/// that is still in progress.
fn read_only_timeout(name: &str) -> Option<Duration> {
    match name {
        "conductor_get_run"
        | "conductor_list_runs"
        | "conductor_list_agent_runs"
        | "conductor_get_step_log"
        | "conductor_list_worktrees"
        | "conductor_get_worktree"
        | "conductor_list_tickets"
        | "conductor_get_ready_tickets"
        | "conductor_list_repos"
        | "conductor_list_workflows"
        | "conductor_list_templates" => Some(READ_TOOL_TIMEOUT),
        _ => None,
    }
}

/// Await a blocking tool task, returning a clear tool error if it exceeds `limit`.
/// On expiry the blocking task keeps running and is dropped when it finishes;
/// read-only tools hold only a read connection, so leaving it is harmless.
async fn run_with_timeout(
    name: &str,
    limit: Option<Duration>,
    handle: tokio::task::JoinHandle<CallToolResult>,
) -> Result<CallToolResult, rmcp::ErrorData> {
    let joined = match limit {
        Some(limit) => match tokio::time::timeout(limit, handle).await {
            Ok(joined) => joined,
            Err(_) => {
                return Ok(crate::mcp::helpers::tool_err(format!(
                    "{name} did not finish within {}s. The conductor database may be locked \
                     by another process (a running workflow, the TUI, or another MCP server). \
                     Retry, or check with `conductor workflow run-show <id>`.",
                    limit.as_secs()
                )))
            }
        },
        None => handle.await,
    };
    joined.map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
}

/// Strip the `conductor://run/` prefix from a resource URI, returning the run id.
fn parse_run_uri(uri: &str) -> Option<&str> {
    uri.strip_prefix(RUN_URI_PREFIX)
}

/// Whether a raw DB status string represents a terminal workflow state.
/// Defers to `WorkflowRunStatus::is_terminal()` so we don't drift from the
/// canonical status enum if new variants are added.
fn is_terminal(status: Option<&str>) -> bool {
    status
        .and_then(|s| s.parse::<WorkflowRunStatus>().ok())
        .is_some_and(|s| s.is_terminal())
}

/// Look up a workflow run's status off-thread. `Ok(None)` means the run does
/// not exist; `Err` means a join failure or DB error (both fatal to the caller's
/// intent — the caller decides whether that means "fail the request" or "drain
/// the subscriber to avoid an orphan").
async fn lookup_run_status(run_id: String) -> Result<Option<String>, anyhow::Error> {
    tokio::task::spawn_blocking(move || {
        let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from)?;
        conductor_core::workflow::get_workflow_run_status(&conductor.conn, &run_id)
            .map_err(anyhow::Error::from)
    })
    .await
    .map_err(anyhow::Error::from)?
}

/// The conductor MCP server. Each request opens its own `Conductor` inside
/// `spawn_blocking` to avoid the `!Send` issue with `rusqlite::Connection`.
pub struct ConductorMcpServer {
    hub: SubscriptionHub,
    _broadcaster_handle: tokio::task::JoinHandle<()>,
}

impl ConductorMcpServer {
    pub fn new() -> Self {
        let (hub, handle) = SubscriptionHub::new();
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
        let event_sinks = vec![self.hub.channel_sink()];
        let limit = read_only_timeout(&name);
        let tool_name = name.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let conductor = conductor_core::Conductor::open().map_err(anyhow::Error::from);
            match conductor {
                Ok(c) => super::tools::dispatch_tool(&c, &name, &args, &event_sinks),
                Err(e) => crate::mcp::helpers::tool_err(e),
            }
        });
        run_with_timeout(&tool_name, limit, handle).await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let uri = request.uri.clone();

        let run_id = parse_run_uri(&uri)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    format!("URI must be {RUN_URI_PREFIX}{{run_id}}"),
                    None,
                )
            })?
            .to_string();

        let status = lookup_run_status(run_id.clone()).await.map_err(|e| {
            rmcp::ErrorData::internal_error(format!("failed to look up run {run_id}: {e}"), None)
        })?;

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
            self.hub
                .subscribe(run_id.clone(), context.peer.clone(), uri);
            // Close TOCTOU: re-check status after insert to catch terminal transition in the gap.
            // Both join failures and DB errors are non-fatal to the subscribe request itself —
            // we drain the just-inserted subscriber so it isn't orphaned, and let the client retry.
            match lookup_run_status(run_id.clone()).await {
                Err(e) => {
                    tracing::warn!(
                        "TOCTOU recheck failed for run {run_id}: {e}; \
                         draining subscriber to avoid orphan"
                    );
                    self.hub.notify_and_drain(&run_id).await;
                }
                Ok(status) if is_terminal(status.as_deref()) => {
                    self.hub.notify_and_drain(&run_id).await;
                }
                Ok(_) => {}
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
        if let Some(run_id) = parse_run_uri(&uri) {
            self.hub.unsubscribe(run_id, &uri);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_text(r: &CallToolResult) -> String {
        serde_json::to_string(r).unwrap()
    }

    #[tokio::test]
    async fn test_run_with_timeout_errors_when_task_is_too_slow() {
        let handle = tokio::task::spawn_blocking(|| {
            std::thread::sleep(Duration::from_millis(500));
            crate::mcp::helpers::tool_ok("late")
        });
        let r = run_with_timeout(
            "conductor_get_run",
            Some(Duration::from_millis(50)),
            handle,
        )
        .await
        .unwrap();
        assert_eq!(r.is_error, Some(true));
        let text = result_text(&r);
        assert!(text.contains("conductor_get_run"), "{text}");
        assert!(text.contains("did not finish within"), "{text}");
    }

    #[tokio::test]
    async fn test_run_with_timeout_passes_through_fast_result() {
        let handle = tokio::task::spawn_blocking(|| crate::mcp::helpers::tool_ok("fast"));
        let r = run_with_timeout("conductor_get_run", Some(Duration::from_secs(5)), handle)
            .await
            .unwrap();
        assert_ne!(r.is_error, Some(true));
        assert!(result_text(&r).contains("fast"));
    }

    #[test]
    fn test_read_only_timeout_classification() {
        assert_eq!(
            read_only_timeout("conductor_get_run"),
            Some(READ_TOOL_TIMEOUT)
        );
        assert_eq!(read_only_timeout("conductor_run_workflow"), None);
        assert_eq!(read_only_timeout("conductor_create_worktree"), None);
        assert_eq!(read_only_timeout("conductor_list_prs"), None);
    }

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
        assert!(!is_terminal(Some("waiting")));
        assert!(!is_terminal(Some("needs_resume")));
        assert!(!is_terminal(Some("cancelling")));
        assert!(!is_terminal(None));
    }

    #[test]
    fn test_is_terminal_rejects_unparseable_status() {
        // Unknown strings (e.g., DB corruption, schema drift) are treated as
        // non-terminal — the safer default for subscribe TOCTOU logic.
        assert!(!is_terminal(Some("queued")));
        assert!(!is_terminal(Some("")));
        assert!(!is_terminal(Some("garbage")));
    }

    #[test]
    fn test_parse_run_uri_strips_prefix() {
        assert_eq!(
            parse_run_uri("conductor://run/01HXYZ"),
            Some("01HXYZ"),
            "should strip the conductor://run/ prefix"
        );
    }

    #[test]
    fn test_parse_run_uri_rejects_other_schemes() {
        assert_eq!(parse_run_uri("conductor://ticket/abc"), None);
        assert_eq!(parse_run_uri("https://example.com/run/abc"), None);
        assert_eq!(parse_run_uri("run/abc"), None);
        assert_eq!(parse_run_uri(""), None);
    }

    #[test]
    fn test_parse_run_uri_accepts_empty_run_id() {
        // Empty run_id passes the prefix check; the run-lookup step is what
        // actually validates the id, and an empty/unknown id will be rejected
        // there with `unknown run_id`.
        assert_eq!(parse_run_uri("conductor://run/"), Some(""));
    }
}
