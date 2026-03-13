//! `conductor mcp serve` — stdio MCP server exposing conductor resources and tools.
//!
//! All DB access runs inside `tokio::task::spawn_blocking` since `rusqlite::Connection`
//! is `!Send`. The `rmcp` library handles the stdio JSON-RPC transport.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, RawResource, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceContents, ResourcesCapability, ServerCapabilities,
    ServerInfo, Tool, ToolAnnotations, ToolsCapability,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Value};

/// Helper: turn an error into a tool result with `is_error: true`.
fn tool_err(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}

/// Helper: turn a string into a successful tool result.
fn tool_ok(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

/// Helper: build a `Resource` with a URI and human-readable name.
fn make_resource(
    uri: impl Into<String>,
    name: impl Into<String>,
    description: impl Into<String>,
) -> Resource {
    Resource {
        raw: RawResource {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: Some(description.into()),
            mime_type: Some("text/plain".into()),
            size: None,
            icons: None,
            meta: None,
        },
        annotations: None,
    }
}

/// Helper: build a JSON Schema input_schema for a Tool.
/// fields: (name, description, required)
fn schema(fields: &[(&str, &str, bool)]) -> Arc<serde_json::Map<String, Value>> {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, desc, req) in fields {
        props.insert(
            name.to_string(),
            json!({ "type": "string", "description": desc }),
        );
        if *req {
            required.push(Value::String(name.to_string()));
        }
    }
    let mut schema_obj = serde_json::Map::new();
    schema_obj.insert("type".into(), Value::String("object".into()));
    schema_obj.insert("properties".into(), Value::Object(props));
    schema_obj.insert("required".into(), Value::Array(required));
    Arc::new(schema_obj)
}

/// Helper: extract an optional string arg from tool call arguments.
fn get_arg<'a>(args: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

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
            let resources = tokio::task::spawn_blocking(move || enumerate_resources(&db_path))
                .await
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
                .map_err(|e: anyhow::Error| rmcp::ErrorData::internal_error(e.to_string(), None))?;

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
            let text = tokio::task::spawn_blocking(move || read_resource_by_uri(&db_path, &uri))
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

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        async move { Ok(ListToolsResult::with_all_items(conductor_tools())) }
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
            let result = tokio::task::spawn_blocking(move || dispatch_tool(&db_path, &name, &args))
                .await
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

            Ok(result)
        }
    }
}

// ---------------------------------------------------------------------------
// Resource enumeration
// ---------------------------------------------------------------------------

fn enumerate_resources(db_path: &Path) -> anyhow::Result<Vec<Resource>> {
    use conductor_core::config::load_config;
    use conductor_core::db::open_database;
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;

    let conn = open_database(db_path)?;
    let config = load_config()?;

    let mut resources = Vec::new();

    // conductor://repos — always present
    resources.push(make_resource(
        "conductor://repos",
        "repos",
        "All registered repos",
    ));

    let repo_mgr = RepoManager::new(&conn, &config);
    let repos = repo_mgr.list()?;

    for repo in &repos {
        resources.push(make_resource(
            format!("conductor://repo/{}", repo.slug),
            format!("repo:{}", repo.slug),
            format!("Repo details for {}", repo.slug),
        ));
        resources.push(make_resource(
            format!("conductor://tickets/{}", repo.slug),
            format!("tickets:{}", repo.slug),
            format!("Open tickets for {}", repo.slug),
        ));
        resources.push(make_resource(
            format!("conductor://worktrees/{}", repo.slug),
            format!("worktrees:{}", repo.slug),
            format!("Worktrees for {}", repo.slug),
        ));
        resources.push(make_resource(
            format!("conductor://runs/{}", repo.slug),
            format!("runs:{}", repo.slug),
            format!("Recent workflow runs for {}", repo.slug),
        ));
        resources.push(make_resource(
            format!("conductor://workflows/{}", repo.slug),
            format!("workflows:{}", repo.slug),
            format!("Available workflow definitions for {}", repo.slug),
        ));

        // Individual tickets (cap at 100 per repo)
        let syncer = TicketSyncer::new(&conn);
        let tickets = syncer.list(Some(&repo.id))?;
        for ticket in tickets.iter().take(100) {
            resources.push(make_resource(
                format!("conductor://ticket/{}/{}", repo.slug, ticket.source_id),
                format!("ticket:{}#{}", repo.slug, ticket.source_id),
                format!("#{} — {}", ticket.source_id, ticket.title),
            ));
        }

        // Individual worktrees
        let wt_mgr = WorktreeManager::new(&conn, &config);
        let worktrees = wt_mgr.list(Some(&repo.slug), false)?;
        for wt in &worktrees {
            resources.push(make_resource(
                format!("conductor://worktree/{}/{}", repo.slug, wt.slug),
                format!("worktree:{}/{}", repo.slug, wt.slug),
                format!("Worktree {} in {}", wt.slug, repo.slug),
            ));
        }

        // Recent workflow runs filtered by repo_id
        let wf_mgr = WorkflowManager::new(&conn);
        let all_runs = wf_mgr.list_all_workflow_runs(50)?;
        for run in all_runs
            .iter()
            .filter(|r| r.repo_id.as_deref() == Some(&repo.id))
            .take(50)
        {
            resources.push(make_resource(
                format!("conductor://run/{}", run.id),
                format!("run:{}", run.id),
                format!("Workflow run {} ({})", run.workflow_name, run.status),
            ));
        }
    }

    Ok(resources)
}

// ---------------------------------------------------------------------------
// Resource reading
// ---------------------------------------------------------------------------

fn read_resource_by_uri(db_path: &Path, uri: &str) -> anyhow::Result<String> {
    use conductor_core::config::load_config;
    use conductor_core::db::open_database;
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;

    let conn = open_database(db_path)?;
    let config = load_config()?;

    if uri == "conductor://repos" {
        let mgr = RepoManager::new(&conn, &config);
        let repos = mgr.list()?;
        if repos.is_empty() {
            return Ok("No repos registered. Use `conductor repo add` to register one.".into());
        }
        let mut out = String::new();
        for r in repos {
            out.push_str(&format!(
                "slug: {}\nlocal_path: {}\nremote_url: {}\ndefault_branch: {}\n\n",
                r.slug, r.local_path, r.remote_url, r.default_branch
            ));
        }
        return Ok(out);
    }

    if let Some(slug) = uri.strip_prefix("conductor://repo/") {
        let mgr = RepoManager::new(&conn, &config);
        let r = mgr.get_by_slug(slug)?;
        return Ok(format!(
            "slug: {}\nlocal_path: {}\nremote_url: {}\ndefault_branch: {}\nworkspace_dir: {}\ncreated_at: {}\n",
            r.slug, r.local_path, r.remote_url, r.default_branch, r.workspace_dir, r.created_at
        ));
    }

    if let Some(repo_slug) = uri.strip_prefix("conductor://tickets/") {
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let syncer = TicketSyncer::new(&conn);
        let tickets = syncer.list(Some(&repo.id))?;
        if tickets.is_empty() {
            return Ok(format!(
                "No tickets for {repo_slug}. Run `conductor tickets sync` first."
            ));
        }
        let mut out = String::new();
        for t in tickets {
            out.push_str(&format!(
                "id: {}\nsource_id: {}\ntitle: {}\nstate: {}\nlabels: {}\nurl: {}\n\n",
                t.id, t.source_id, t.title, t.state, t.labels, t.url
            ));
        }
        return Ok(out);
    }

    // conductor://ticket/{repo}/{source_id}
    if let Some(rest) = uri.strip_prefix("conductor://ticket/") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            let repo_slug = parts[0];
            let source_id = parts[1];
            let repo_mgr = RepoManager::new(&conn, &config);
            let repo = repo_mgr.get_by_slug(repo_slug)?;
            let syncer = TicketSyncer::new(&conn);
            let tickets = syncer.list(Some(&repo.id))?;
            if let Some(ticket) = tickets.iter().find(|t| t.source_id == source_id) {
                let labels = syncer.get_labels(&ticket.id)?;
                let label_str: Vec<String> = labels.iter().map(|l| l.label.clone()).collect();
                return Ok(format!(
                    "source_id: {}\ntitle: {}\nstate: {}\nlabels: {}\nassignee: {}\nurl: {}\nbody:\n{}\n",
                    ticket.source_id,
                    ticket.title,
                    ticket.state,
                    label_str.join(", "),
                    ticket.assignee.as_deref().unwrap_or("none"),
                    ticket.url,
                    ticket.body
                ));
            } else {
                anyhow::bail!("Ticket {source_id} not found in {repo_slug}");
            }
        }
    }

    if let Some(repo_slug) = uri.strip_prefix("conductor://worktrees/") {
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let wt_mgr = WorktreeManager::new(&conn, &config);
        let worktrees = wt_mgr.list(Some(&repo.slug), false)?;
        if worktrees.is_empty() {
            return Ok(format!("No worktrees for {repo_slug}."));
        }
        let mut out = String::new();
        for wt in worktrees {
            out.push_str(&format!(
                "slug: {}\nbranch: {}\npath: {}\nstatus: {}\ncreated_at: {}\nticket_id: {}\n\n",
                wt.slug,
                wt.branch,
                wt.path,
                wt.status,
                wt.created_at,
                wt.ticket_id.as_deref().unwrap_or("none")
            ));
        }
        return Ok(out);
    }

    // conductor://worktree/{repo}/{slug}
    if let Some(rest) = uri.strip_prefix("conductor://worktree/") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            let repo_slug = parts[0];
            let wt_slug = parts[1];
            let repo_mgr = RepoManager::new(&conn, &config);
            let repo = repo_mgr.get_by_slug(repo_slug)?;
            let wt_mgr = WorktreeManager::new(&conn, &config);
            let wt = wt_mgr.get_by_slug(&repo.id, wt_slug)?;
            let mut out = format!(
                "slug: {}\nbranch: {}\npath: {}\nstatus: {}\ncreated_at: {}\n",
                wt.slug, wt.branch, wt.path, wt.status, wt.created_at
            );
            if let Some(ticket_id) = &wt.ticket_id {
                let syncer = TicketSyncer::new(&conn);
                if let Ok(ticket) = syncer.get_by_id(ticket_id) {
                    out.push_str(&format!(
                        "linked_ticket: #{} — {}\n",
                        ticket.source_id, ticket.title
                    ));
                }
            }
            return Ok(out);
        }
    }

    if let Some(repo_slug) = uri.strip_prefix("conductor://runs/") {
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let wf_mgr = WorkflowManager::new(&conn);
        let runs = wf_mgr.list_all_workflow_runs(50)?;
        let repo_runs: Vec<_> = runs
            .into_iter()
            .filter(|r| r.repo_id.as_deref() == Some(&repo.id))
            .collect();
        if repo_runs.is_empty() {
            return Ok(format!("No workflow runs for {repo_slug}."));
        }
        let mut out = String::new();
        for run in repo_runs {
            out.push_str(&format!(
                "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\n\n",
                run.id, run.workflow_name, run.status, run.started_at
            ));
        }
        return Ok(out);
    }

    if let Some(run_id) = uri.strip_prefix("conductor://run/") {
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(run_id)?
            .ok_or_else(|| anyhow::anyhow!("Workflow run {run_id} not found"))?;
        let steps = wf_mgr.get_workflow_steps(run_id)?;
        let mut out = format!(
            "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\nended_at: {}\nsummary: {}\n\nsteps:\n",
            run.id,
            run.workflow_name,
            run.status,
            run.started_at,
            run.ended_at.as_deref().unwrap_or("running"),
            run.result_summary.as_deref().unwrap_or("none")
        );
        for step in steps {
            out.push_str(&format!(
                "  {} [{}]: {}\n",
                step.step_name,
                step.status,
                step.result_text.as_deref().unwrap_or("")
            ));
        }
        return Ok(out);
    }

    if let Some(repo_slug) = uri.strip_prefix("conductor://workflows/") {
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let (defs, warnings) = WorkflowManager::list_defs(&repo.local_path, &repo.local_path)?;
        let mut out = String::new();
        for w in &warnings {
            out.push_str(&format!(
                "warning: Failed to parse {}: {}\n",
                w.file, w.message
            ));
        }
        if defs.is_empty() {
            out.push_str(&format!("No workflow definitions found in {repo_slug}."));
        } else {
            for def in defs {
                out.push_str(&format!(
                    "name: {}\ndescription: {}\ntrigger: {}\n\n",
                    def.name, def.description, def.trigger
                ));
            }
        }
        return Ok(out);
    }

    anyhow::bail!("Unknown conductor:// URI: {uri}")
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

fn conductor_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "conductor_list_tickets",
            "List open tickets for a repo.",
            schema(&[("repo", "Repo slug (e.g. my-repo)", true)]),
        ),
        Tool::new(
            "conductor_list_worktrees",
            "List active worktrees for a repo.",
            schema(&[("repo", "Repo slug", true)]),
        ),
        Tool::new(
            "conductor_create_worktree",
            "Create a new worktree (git branch + working directory) for a repo.",
            schema(&[
                ("repo", "Repo slug", true),
                (
                    "name",
                    "Worktree name (e.g. feat-my-feature or fix-bug-123)",
                    true,
                ),
                (
                    "ticket_id",
                    "Internal ticket ULID to link (optional)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_delete_worktree",
            "Delete a worktree (destructive — removes the git branch and working directory).",
            schema(&[
                ("repo", "Repo slug", true),
                ("slug", "Worktree slug to delete", true),
            ]),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).read_only(false)),
        Tool::new(
            "conductor_sync_tickets",
            "Sync tickets from the configured issue source (GitHub/Jira) for a repo.",
            schema(&[("repo", "Repo slug", true)]),
        ),
        Tool::new(
            "conductor_run_workflow",
            "Start a workflow. Returns run_id immediately; poll with conductor_get_run. \
             Provide worktree or inputs to target a specific context.",
            schema(&[
                ("workflow", "Workflow name", true),
                ("repo", "Repo slug", true),
                ("worktree", "Worktree slug (optional)", false),
                (
                    "inputs",
                    "JSON object of input key=value pairs (optional)",
                    false,
                ),
            ]),
        ),
        Tool::new(
            "conductor_list_runs",
            "List recent workflow runs for a repo (optionally filtered by worktree).",
            schema(&[
                ("repo", "Repo slug", true),
                ("worktree", "Worktree slug to filter by (optional)", false),
            ]),
        ),
        Tool::new(
            "conductor_get_run",
            "Get the status and step details of a workflow run.",
            schema(&[("run_id", "Workflow run ID", true)]),
        ),
        Tool::new(
            "conductor_approve_gate",
            "Approve a waiting gate in a workflow run.",
            schema(&[
                ("run_id", "Workflow run ID", true),
                ("feedback", "Optional feedback or approval message", false),
            ]),
        ),
        Tool::new(
            "conductor_reject_gate",
            "Reject a waiting gate in a workflow run.",
            schema(&[("run_id", "Workflow run ID", true)]),
        ),
        Tool::new(
            "conductor_push_worktree",
            "Push the current branch of a worktree to the remote.",
            schema(&[("repo", "Repo slug", true), ("slug", "Worktree slug", true)]),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Tool dispatch
// ---------------------------------------------------------------------------

fn dispatch_tool(
    db_path: &Path,
    name: &str,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    match name {
        "conductor_list_tickets" => tool_list_tickets(db_path, args),
        "conductor_list_worktrees" => tool_list_worktrees(db_path, args),
        "conductor_create_worktree" => tool_create_worktree(db_path, args),
        "conductor_delete_worktree" => tool_delete_worktree(db_path, args),
        "conductor_sync_tickets" => tool_sync_tickets(db_path, args),
        "conductor_run_workflow" => tool_run_workflow(db_path, args),
        "conductor_list_runs" => tool_list_runs(db_path, args),
        "conductor_get_run" => tool_get_run(db_path, args),
        "conductor_approve_gate" => tool_approve_gate(db_path, args),
        "conductor_reject_gate" => tool_reject_gate(db_path, args),
        "conductor_push_worktree" => tool_push_worktree(db_path, args),
        _ => tool_err(format!("Unknown tool: {name}")),
    }
}

fn open_db_and_config(
    db_path: &PathBuf,
) -> anyhow::Result<(rusqlite::Connection, conductor_core::config::Config)> {
    use conductor_core::config::load_config;
    use conductor_core::db::open_database;
    let conn = open_database(db_path)?;
    let config = load_config()?;
    Ok((conn, config))
}

fn tool_list_tickets(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };
    let syncer = TicketSyncer::new(&conn);
    let tickets = match syncer.list(Some(&repo.id)) {
        Ok(t) => t,
        Err(e) => return tool_err(e),
    };
    if tickets.is_empty() {
        return tool_ok(format!(
            "No tickets for {repo_slug}. Run `conductor tickets sync` first."
        ));
    }
    let mut out = String::new();
    for t in tickets {
        out.push_str(&format!("#{} — {} [{}]\n", t.source_id, t.title, t.state));
    }
    tool_ok(out)
}

fn tool_list_worktrees(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    let worktrees = match wt_mgr.list(Some(repo_slug), true) {
        Ok(w) => w,
        Err(e) => return tool_err(e),
    };
    if worktrees.is_empty() {
        return tool_ok(format!("No active worktrees for {repo_slug}."));
    }
    let mut out = String::new();
    for wt in worktrees {
        out.push_str(&format!(
            "slug: {}\nbranch: {}\nstatus: {}\npath: {}\n\n",
            wt.slug, wt.branch, wt.status, wt.path
        ));
    }
    tool_ok(out)
}

fn tool_create_worktree(
    db_path: &PathBuf,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let name = match get_arg(args, "name") {
        Some(s) => s,
        None => return tool_err("Missing required argument: name"),
    };
    let ticket_id = get_arg(args, "ticket_id");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.create(repo_slug, name, None, ticket_id, None) {
        Ok((wt, warnings)) => {
            let mut msg = format!(
                "Worktree created.\nslug: {}\nbranch: {}\npath: {}\n",
                wt.slug, wt.branch, wt.path
            );
            for w in warnings {
                msg.push_str(&format!("warning: {w}\n"));
            }
            tool_ok(msg)
        }
        Err(e) => tool_err(e),
    }
}

fn tool_delete_worktree(
    db_path: &PathBuf,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let slug = match get_arg(args, "slug") {
        Some(s) => s,
        None => return tool_err("Missing required argument: slug"),
    };
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.delete(repo_slug, slug) {
        Ok(wt) => tool_ok(format!("Deleted worktree {}.", wt.slug)),
        Err(e) => tool_err(e),
    }
}

fn tool_sync_tickets(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::github;
    use conductor_core::issue_source::IssueSourceManager;
    use conductor_core::jira_acli;
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };
    let source_mgr = IssueSourceManager::new(&conn);
    let sources = match source_mgr.list(&repo.id) {
        Ok(s) => s,
        Err(e) => return tool_err(e),
    };
    if sources.is_empty() {
        return tool_err(format!(
            "No issue sources configured for {repo_slug}. Use `conductor repo sources add` to configure one."
        ));
    }
    let syncer = TicketSyncer::new(&conn);
    let mut total_synced = 0usize;
    let mut total_closed = 0usize;
    let mut errors = Vec::new();

    for source in sources {
        let fetch_result = match source.source_type.as_str() {
            "github" => {
                let cfg: conductor_core::issue_source::GitHubConfig =
                    match serde_json::from_str(&source.config_json) {
                        Ok(c) => c,
                        Err(e) => {
                            errors.push(format!("github config parse error: {e}"));
                            continue;
                        }
                    };
                github::sync_github_issues(&cfg.owner, &cfg.repo, None)
            }
            "jira" => {
                let cfg: conductor_core::issue_source::JiraConfig =
                    match serde_json::from_str(&source.config_json) {
                        Ok(c) => c,
                        Err(e) => {
                            errors.push(format!("jira config parse error: {e}"));
                            continue;
                        }
                    };
                jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url)
            }
            other => {
                errors.push(format!("Unknown source type: {other}"));
                continue;
            }
        };
        match fetch_result {
            Ok(tickets) => {
                let (synced, closed) =
                    syncer.sync_and_close_tickets(&repo.id, &source.source_type, &tickets);
                total_synced += synced;
                total_closed += closed;
            }
            Err(e) => errors.push(format!("{}: {e}", source.source_type)),
        }
    }
    let mut msg = format!("Synced {total_synced} tickets, {total_closed} closed for {repo_slug}.");
    for err in errors {
        msg.push_str(&format!("\nerror: {err}"));
    }
    tool_ok(msg)
}

fn tool_run_workflow(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::agent::AgentManager;
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::{
        execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone, WorkflowManager,
    };
    use conductor_core::worktree::WorktreeManager;

    let workflow_name = match get_arg(args, "workflow") {
        Some(s) => s,
        None => return tool_err("Missing required argument: workflow"),
    };
    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let worktree_slug = get_arg(args, "worktree");

    // Parse optional inputs JSON
    let inputs: HashMap<String, String> = if let Some(inputs_str) = get_arg(args, "inputs") {
        match serde_json::from_str::<HashMap<String, String>>(inputs_str) {
            Ok(m) => m,
            Err(e) => return tool_err(format!("Invalid inputs JSON: {e}")),
        }
    } else {
        HashMap::new()
    };

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    // Load the workflow definition
    let workflow = match WorkflowManager::load_def_by_name(
        &repo.local_path,
        &repo.local_path,
        workflow_name,
    ) {
        Ok(w) => w,
        Err(e) => return tool_err(format!("Failed to load workflow '{workflow_name}': {e}")),
    };

    let (worktree_id, working_dir) = if let Some(wt_slug) = worktree_slug {
        let wt_mgr = WorktreeManager::new(&conn, &config);
        match wt_mgr.get_by_slug(&repo.id, wt_slug) {
            Ok(wt) => (Some(wt.id), wt.path),
            Err(e) => return tool_err(e),
        }
    } else {
        (None, repo.local_path.clone())
    };

    // Create a placeholder agent run that acts as the "parent" for the workflow run
    let agent_mgr = AgentManager::new(&conn);
    let parent_run = match agent_mgr.create_run(worktree_id.as_deref(), "mcp-workflow", None, None)
    {
        Ok(r) => r,
        Err(e) => return tool_err(format!("Failed to create agent run: {e}")),
    };

    // Create the workflow run record
    let wf_mgr = WorkflowManager::new(&conn);
    let wf_run = match wf_mgr.create_workflow_run_with_targets(
        &workflow.name,
        worktree_id.as_deref(),
        None,
        Some(&repo.id),
        &parent_run.id,
        false,
        "mcp",
        None,
        None,
        Some(repo_slug),
    ) {
        Ok(r) => r,
        Err(e) => return tool_err(format!("Failed to create workflow run: {e}")),
    };

    let run_id = wf_run.id.clone();

    // Fire-and-forget: execute in a background thread
    let standalone = WorkflowExecStandalone {
        config,
        workflow,
        worktree_id,
        working_dir,
        repo_path: repo.local_path,
        ticket_id: None,
        repo_id: Some(repo.id),
        model: None,
        exec_config: WorkflowExecConfig::default(),
        inputs,
        target_label: Some(repo_slug.to_string()),
    };

    std::thread::spawn(move || {
        let _ = execute_workflow_standalone(&standalone);
    });

    tool_ok(format!(
        "Workflow '{workflow_name}' started.\nrun_id: {run_id}\nPoll progress with conductor_get_run."
    ))
}

fn tool_list_runs(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let worktree_slug = get_arg(args, "worktree");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let wf_mgr = WorkflowManager::new(&conn);
    let runs = if let Some(wt_slug) = worktree_slug {
        let wt_mgr = WorktreeManager::new(&conn, &config);
        let wt = match wt_mgr.get_by_slug(&repo.id, wt_slug) {
            Ok(w) => w,
            Err(e) => return tool_err(e),
        };
        match wf_mgr.list_workflow_runs(&wt.id) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        }
    } else {
        match wf_mgr.list_all_workflow_runs(50) {
            Ok(r) => r
                .into_iter()
                .filter(|r| r.repo_id.as_deref() == Some(&repo.id))
                .collect(),
            Err(e) => return tool_err(e),
        }
    };

    if runs.is_empty() {
        return tool_ok(format!("No workflow runs for {repo_slug}."));
    }
    let mut out = String::new();
    for run in runs {
        out.push_str(&format!(
            "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\n\n",
            run.id, run.workflow_name, run.status, run.started_at
        ));
    }
    tool_ok(out)
}

fn tool_get_run(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = match get_arg(args, "run_id") {
        Some(s) => s,
        None => return tool_err("Missing required argument: run_id"),
    };
    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let run = match wf_mgr.get_workflow_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run {run_id} not found")),
        Err(e) => return tool_err(e),
    };
    let steps = match wf_mgr.get_workflow_steps(run_id) {
        Ok(s) => s,
        Err(e) => return tool_err(e),
    };
    let mut out = format!(
        "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\nended_at: {}\nsummary: {}\n\nsteps:\n",
        run.id,
        run.workflow_name,
        run.status,
        run.started_at,
        run.ended_at.as_deref().unwrap_or("running"),
        run.result_summary.as_deref().unwrap_or("none")
    );
    for step in steps {
        out.push_str(&format!(
            "  {} [{}]: {}\n",
            step.step_name,
            step.status,
            step.result_text.as_deref().unwrap_or("")
        ));
    }
    tool_ok(out)
}

fn tool_approve_gate(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = match get_arg(args, "run_id") {
        Some(s) => s,
        None => return tool_err("Missing required argument: run_id"),
    };
    let feedback = get_arg(args, "feedback");

    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let step = match wf_mgr.find_waiting_gate(run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.approve_gate(&step.id, "mcp", feedback) {
        Ok(()) => tool_ok(format!("Gate approved for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

fn tool_reject_gate(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = match get_arg(args, "run_id") {
        Some(s) => s,
        None => return tool_err("Missing required argument: run_id"),
    };
    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let step = match wf_mgr.find_waiting_gate(run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.reject_gate(&step.id, "mcp") {
        Ok(()) => tool_ok(format!("Gate rejected for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

fn tool_push_worktree(db_path: &PathBuf, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = match get_arg(args, "repo") {
        Some(s) => s,
        None => return tool_err("Missing required argument: repo"),
    };
    let slug = match get_arg(args, "slug") {
        Some(s) => s,
        None => return tool_err("Missing required argument: slug"),
    };
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.push(repo_slug, slug) {
        Ok(msg) => tool_ok(msg),
        Err(e) => tool_err(e),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Start the stdio MCP server and block until the client disconnects.
pub async fn serve() -> anyhow::Result<()> {
    let db_path = conductor_core::config::db_path();
    let server = ConductorMcpServer::new(db_path);
    let service = rmcp::serve_server(server, rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
