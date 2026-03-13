//! `conductor mcp serve` — stdio MCP server exposing conductor resources and tools.
//!
//! All DB access runs inside `tokio::task::spawn_blocking` since `rusqlite::Connection`
//! is `!Send`. The `rmcp` library handles the stdio JSON-RPC transport.

use std::collections::{HashMap, VecDeque};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
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

/// Macro: extract a required string arg; returns `tool_err` early if missing.
macro_rules! require_arg {
    ($args:expr, $key:literal) => {
        match get_arg($args, $key) {
            Some(s) => s,
            None => return tool_err(concat!("Missing required argument: ", $key)),
        }
    };
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

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(conductor_tools()))
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

    // Bulk-fetch all tickets, worktrees, and workflow runs once to avoid N+1 queries.
    let syncer = TicketSyncer::new(&conn);
    let all_tickets = syncer.list(None)?;

    let wt_mgr = WorktreeManager::new(&conn, &config);
    let all_worktrees = wt_mgr.list(None, false)?;

    let wf_mgr = WorkflowManager::new(&conn);

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

        // Individual tickets (cap at 100 per repo, filtered from bulk fetch)
        for ticket in all_tickets
            .iter()
            .filter(|t| t.repo_id == repo.id)
            .take(100)
        {
            resources.push(make_resource(
                format!("conductor://ticket/{}/{}", repo.slug, ticket.source_id),
                format!("ticket:{}#{}", repo.slug, ticket.source_id),
                format!("#{} — {}", ticket.source_id, ticket.title),
            ));
        }

        // Individual worktrees (filtered from bulk fetch)
        for wt in all_worktrees.iter().filter(|w| w.repo_id == repo.id) {
            resources.push(make_resource(
                format!("conductor://worktree/{}/{}", repo.slug, wt.slug),
                format!("worktree:{}/{}", repo.slug, wt.slug),
                format!("Worktree {} in {}", wt.slug, repo.slug),
            ));
        }

        // Per-repo workflow runs: query directly by repo_id to avoid the global
        // cap silently dropping older runs when many repos are registered.
        let repo_runs = wf_mgr.list_workflow_runs_by_repo_id(&repo.id, 50, 0)?;
        for run in &repo_runs {
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
            let ticket = syncer.get_by_source_id(&repo.id, source_id)?;
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
                match syncer.get_by_id(ticket_id) {
                    Ok(ticket) => out.push_str(&format!(
                        "linked_ticket: #{} — {}\n",
                        ticket.source_id, ticket.title
                    )),
                    Err(e) => out.push_str(&format!(
                        "linked_ticket_error: could not load ticket {ticket_id}: {e}\n"
                    )),
                }
            }
            return Ok(out);
        }
    }

    if let Some(repo_slug) = uri.strip_prefix("conductor://runs/") {
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let wf_mgr = WorkflowManager::new(&conn);
        let repo_runs = wf_mgr.list_workflow_runs_by_repo_id(&repo.id, 50, 0)?;
        if repo_runs.is_empty() {
            return Ok(format!("No workflow runs for {repo_slug}."));
        }
        let mut out = String::new();
        for run in &repo_runs {
            out.push_str(&format_run_summary_line(run));
        }
        return Ok(out);
    }

    if let Some(run_id) = uri.strip_prefix("conductor://run/") {
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .get_workflow_run(run_id)?
            .ok_or_else(|| anyhow::anyhow!("Workflow run {run_id} not found"))?;
        let steps = wf_mgr.get_workflow_steps(run_id)?;
        return Ok(format_run_detail_with_log(&conn, &run, &steps));
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
// Run formatting helpers (shared between resource reader and tool handlers)
// ---------------------------------------------------------------------------

fn format_run_summary_line(run: &conductor_core::workflow::WorkflowRun) -> String {
    format!(
        "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\n\n",
        run.id, run.workflow_name, run.status, run.started_at
    )
}

fn format_run_detail(
    run: &conductor_core::workflow::WorkflowRun,
    steps: &[conductor_core::workflow::WorkflowRunStep],
) -> String {
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
    out
}

/// Resolve the worktree filesystem path for a workflow run.
fn worktree_path_for_run(
    conn: &rusqlite::Connection,
    run: &conductor_core::workflow::WorkflowRun,
) -> Option<PathBuf> {
    let wt_id = run.worktree_id.as_deref()?;
    let config = conductor_core::config::load_config().ok()?;
    let wt = conductor_core::worktree::WorktreeManager::new(conn, &config)
        .get_by_id(wt_id)
        .ok()?;
    Some(PathBuf::from(&wt.path))
}

/// Return the tail of the most recent Claude Code conversation log for a worktree.
///
/// Looks in `~/.claude/projects/<escaped>/` where `<escaped>` is the worktree
/// path with every `/` replaced by `-`. Returns `None` on any error or if no
/// relevant messages are found.
fn conversation_log_tail(worktree_path: &Path) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let escaped = worktree_path.to_str()?.replace('/', "-");
    let projects_dir = PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join(&escaped);
    conversation_log_tail_from_dir(&projects_dir)
}

/// Inner implementation: read the most-recently-modified JSONL from `projects_dir`
/// and return the last 20 user/assistant messages. Separated for testability.
fn conversation_log_tail_from_dir(projects_dir: &Path) -> Option<String> {
    // Collect all .jsonl files, pick the most recently modified.
    let entries = std::fs::read_dir(projects_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, path));
            }
        }
    }
    let log_path = best?.1;

    // Ring-buffer the last 20 user/assistant messages, streaming line-by-line
    // to avoid buffering the entire (potentially large) JSONL file into memory.
    let file = std::fs::File::open(&log_path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut ring: VecDeque<String> = VecDeque::with_capacity(20);
    for line in reader.lines() {
        let line = line.ok().unwrap_or_default();
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }
        // Extract text content.
        let text = match val.get("message").and_then(|m| m.get("content")) {
            Some(Value::String(s)) => s.chars().take(500).collect::<String>(),
            Some(Value::Array(blocks)) => {
                let mut parts = String::new();
                for block in blocks {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            parts.push_str(t);
                        }
                    }
                }
                parts.chars().take(500).collect::<String>()
            }
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        if ring.len() == 20 {
            ring.pop_front();
        }
        ring.push_back(format!("[{msg_type}]\n{text}\n"));
    }

    if ring.is_empty() {
        return None;
    }
    Some(ring.into_iter().collect::<String>())
}

/// Like `format_run_detail` but also appends the conversation log tail when available.
fn format_run_detail_with_log(
    conn: &rusqlite::Connection,
    run: &conductor_core::workflow::WorkflowRun,
    steps: &[conductor_core::workflow::WorkflowRunStep],
) -> String {
    let mut out = format_run_detail(run, steps);
    if let Some(wt_path) = worktree_path_for_run(conn, run) {
        if let Some(log) = conversation_log_tail(&wt_path) {
            out.push_str("\nconversation log (last 20 messages):\n");
            out.push_str(&log);
        }
    }
    out
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
                    "Ticket ID to link (optional) — accepts either the internal ULID or an external source ID (e.g. GitHub issue number '680')",
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
            {
                let mut props = serde_json::Map::new();
                props.insert(
                    "workflow".into(),
                    json!({ "type": "string", "description": "Workflow name" }),
                );
                props.insert(
                    "repo".into(),
                    json!({ "type": "string", "description": "Repo slug" }),
                );
                props.insert(
                    "worktree".into(),
                    json!({ "type": "string", "description": "Worktree slug or branch name (optional)" }),
                );
                props.insert(
                    "inputs".into(),
                    json!({
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Input key=value pairs (optional)"
                    }),
                );
                let mut s = serde_json::Map::new();
                s.insert("type".into(), Value::String("object".into()));
                s.insert("properties".into(), Value::Object(props));
                s.insert(
                    "required".into(),
                    Value::Array(vec![
                        Value::String("workflow".into()),
                        Value::String("repo".into()),
                    ]),
                );
                Arc::new(s)
            },
        ),
        Tool::new(
            "conductor_list_runs",
            "List recent workflow runs for a repo (optionally filtered by worktree and/or status). \
             Supports pagination via limit (default 50) and offset (default 0).",
            schema(&[
                ("repo", "Repo slug", true),
                (
                    "worktree",
                    "Worktree slug or branch name to filter by (optional)",
                    false,
                ),
                (
                    "status",
                    "Filter by run status: pending, running, completed, failed, cancelled, waiting (optional)",
                    false,
                ),
                ("limit", "Max runs to return (default 50)", false),
                (
                    "offset",
                    "Number of runs to skip for pagination (default 0)",
                    false,
                ),
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
            schema(&[
                ("run_id", "Workflow run ID", true),
                ("feedback", "Optional feedback or rejection reason", false),
            ]),
        ),
        Tool::new(
            "conductor_push_worktree",
            "Push the current branch of a worktree to the remote.",
            schema(&[("repo", "Repo slug", true), ("slug", "Worktree slug", true)]),
        ),
        Tool::new(
            "conductor_cancel_run",
            "Cancel a workflow run that is pending, running, or waiting. \
             In-progress steps and child agent runs are best-effort cancelled before \
             the run is marked cancelled.",
            schema(&[("run_id", "Workflow run ID to cancel", true)]),
        ),
        Tool::new(
            "conductor_list_workflows",
            "List available workflow definitions for a repo. Returns workflow names, descriptions, and trigger types.",
            schema(&[("repo", "Repo slug (e.g. my-repo)", true)]),
        ),
        Tool::new(
            "conductor_list_repos",
            "List all registered repos and their slugs. Use this to discover valid repo slugs \
             required by other conductor tools.",
            schema(&[]),
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
        "conductor_cancel_run" => tool_cancel_run(db_path, args),
        "conductor_list_workflows" => tool_list_workflows(db_path, args),
        "conductor_list_repos" => tool_list_repos(db_path),
        _ => tool_err(format!("Unknown tool: {name}")),
    }
}

fn open_db_and_config(
    db_path: &Path,
) -> anyhow::Result<(rusqlite::Connection, conductor_core::config::Config)> {
    use conductor_core::config::load_config;
    use conductor_core::db::open_database;
    let conn = open_database(db_path)?;
    let config = load_config()?;
    Ok((conn, config))
}

fn tool_list_repos(db_path: &Path) -> CallToolResult {
    use conductor_core::repo::RepoManager;

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repos = match RepoManager::new(&conn, &config).list() {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };
    if repos.is_empty() {
        return tool_ok("No repos registered. Use `conductor repo add` to register one.");
    }
    let mut out = String::new();
    for r in repos {
        out.push_str(&format!(
            "slug: {}\nlocal_path: {}\nremote_url: {}\ndefault_branch: {}\n\n",
            r.slug, r.local_path, r.remote_url, r.default_branch
        ));
    }
    tool_ok(out)
}

fn tool_list_tickets(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;

    let repo_slug = require_arg!(args, "repo");
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

fn tool_list_worktrees(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
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

/// Returns `true` if `s` looks like a ULID: exactly 26 uppercase alphanumeric chars.
/// Used to distinguish internal ULIDs (e.g. "01HXYZ...") from external source IDs (e.g. "680").
fn looks_like_ulid(s: &str) -> bool {
    s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn tool_create_worktree(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let name = require_arg!(args, "name");
    let raw_ticket_id = get_arg(args, "ticket_id");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };

    // Resolve ticket_id: if it looks like a ULID pass it through; otherwise treat
    // it as an external source_id and look up the internal ULID.
    let resolved_ticket_id: Option<String> = match raw_ticket_id {
        None => None,
        Some(id) if looks_like_ulid(id) => Some(id.to_string()),
        Some(source_id) => {
            let repo_mgr = RepoManager::new(&conn, &config);
            let repo = match repo_mgr.get_by_slug(repo_slug) {
                Ok(r) => r,
                Err(e) => return tool_err(e),
            };
            let syncer = TicketSyncer::new(&conn);
            match syncer.get_by_source_id(&repo.id, source_id) {
                Ok(ticket) => Some(ticket.id),
                Err(e) => {
                    return tool_err(format!("Could not resolve ticket ID '{source_id}': {e}"))
                }
            }
        }
    };

    let wt_mgr = WorktreeManager::new(&conn, &config);
    match wt_mgr.create(repo_slug, name, None, resolved_ticket_id.as_deref(), None) {
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

fn tool_delete_worktree(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let slug = require_arg!(args, "slug");
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

fn tool_sync_tickets(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::github;
    use conductor_core::issue_source::IssueSourceManager;
    use conductor_core::jira_acli;
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;

    let repo_slug = require_arg!(args, "repo");
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
    if errors.is_empty() {
        tool_ok(format!(
            "Synced {total_synced} tickets, {total_closed} closed for {repo_slug}."
        ))
    } else {
        let mut msg = format!(
            "Sync failed for {repo_slug}. Synced {total_synced} tickets, {total_closed} closed."
        );
        for err in errors {
            msg.push_str(&format!("\nerror: {err}"));
        }
        tool_err(msg)
    }
}

fn tool_run_workflow(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::{
        execute_workflow_standalone, WorkflowExecConfig, WorkflowExecStandalone, WorkflowManager,
    };
    use conductor_core::worktree::WorktreeManager;
    use std::sync::{Arc, Mutex};

    let workflow_name = require_arg!(args, "workflow");
    let repo_slug = require_arg!(args, "repo");
    let worktree_slug = get_arg(args, "worktree");

    // Extract optional inputs object
    let inputs: HashMap<String, String> = match args.get("inputs") {
        None => HashMap::new(),
        Some(Value::Object(map)) => {
            let mut result = HashMap::new();
            for (k, v) in map {
                match v.as_str() {
                    Some(s) => {
                        result.insert(k.clone(), s.to_string());
                    }
                    None => return tool_err(format!("inputs.{k} must be a string value")),
                }
            }
            result
        }
        Some(other) => return tool_err(format!("inputs must be an object, got: {other}")),
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
        match wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug) {
            Ok(wt) => (Some(wt.id), wt.path),
            Err(e) => return tool_err(e),
        }
    } else {
        (None, repo.local_path.clone())
    };

    // Condvar-based notification: the workflow engine writes the run ID here and
    // signals the condvar once the run record is created (before any steps execute).
    let notify_pair: Arc<(Mutex<Option<String>>, std::sync::Condvar)> =
        Arc::new((Mutex::new(None), std::sync::Condvar::new()));

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
        run_id_notify: Some(Arc::clone(&notify_pair)),
    };

    // Slot receives the error message if execute_workflow_standalone fails before
    // creating the run record (i.e., before writing to run_id_notify).
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_slot_bg = Arc::clone(&error_slot);
    let notify_pair_bg = Arc::clone(&notify_pair);

    std::thread::spawn(move || {
        if let Err(e) = execute_workflow_standalone(&standalone) {
            *error_slot_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            // Wake the waiter so it surfaces the error immediately.
            notify_pair_bg.1.notify_one();
        }
    });

    // Block (without spinning) until the run record is created or 2 s elapses.
    let (lock, cvar) = notify_pair.as_ref();
    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let (guard, _timed_out) = cvar
        .wait_timeout_while(guard, std::time::Duration::from_secs(2), |v| {
            v.is_none()
                && error_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
        })
        .unwrap_or_else(|e| e.into_inner());

    // Surface startup errors before checking for the run ID.
    if let Some(err) = error_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        return tool_err(format!("Workflow failed to start: {err}"));
    }

    let run_id = match guard.as_ref() {
        Some(id) => id.clone(),
        None => {
            return tool_err(
                "Workflow started but run ID was not available within 2 s. \
             The workflow may still be running in the background — \
             use conductor_list_runs to check status.",
            )
        }
    };

    tool_ok(format!(
        "Workflow '{workflow_name}' started.\nrun_id: {run_id}\nPoll progress with conductor_get_run."
    ))
}

fn tool_list_runs(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let worktree_slug = get_arg(args, "worktree");
    let status_str = get_arg(args, "status");

    let status: Option<WorkflowRunStatus> = match status_str {
        Some(s) => match s.parse::<WorkflowRunStatus>() {
            Ok(v) => Some(v),
            Err(e) => return tool_err(e),
        },
        None => None,
    };

    let limit: usize = get_arg(args, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let offset: usize = get_arg(args, "offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

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
        let wt = match wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug) {
            Ok(w) => w,
            Err(e) => return tool_err(e),
        };
        match wf_mgr.list_workflow_runs_filtered_paginated(&wt.id, status, limit, offset) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        }
    } else {
        match wf_mgr.list_workflow_runs_by_repo_id_filtered(&repo.id, limit, offset, status) {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        }
    };

    if runs.is_empty() {
        return tool_ok(format!("No workflow runs for {repo_slug}."));
    }
    let mut out = String::new();
    for run in &runs {
        out.push_str(&format_run_summary_line(run));
    }
    if runs.len() == limit {
        out.push_str(&format!(
            "\nShowing {offset}–{end} (limit {limit}). Pass offset={next} for more.",
            end = offset + runs.len(),
            next = offset + limit,
        ));
    }
    tool_ok(out)
}

fn tool_list_workflows(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowManager;

    let repo_slug = require_arg!(args, "repo");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };
    let (defs, warnings) = match WorkflowManager::list_defs(&repo.local_path, &repo.local_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
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
    tool_ok(out)
}

fn tool_get_run(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
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
    tool_ok(format_run_detail_with_log(&conn, &run, &steps))
}

fn tool_approve_gate(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
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

fn tool_reject_gate(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let feedback = get_arg(args, "feedback");
    let step = match wf_mgr.find_waiting_gate(run_id) {
        Ok(Some(s)) => s,
        Ok(None) => return tool_err(format!("No waiting gate found for run {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.reject_gate(&step.id, "mcp", feedback) {
        Ok(()) => tool_ok(format!("Gate rejected for run {run_id}.")),
        Err(e) => tool_err(e),
    }
}

fn tool_push_worktree(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let slug = require_arg!(args, "slug");
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

fn tool_cancel_run(db_path: &Path, args: &serde_json::Map<String, Value>) -> CallToolResult {
    use conductor_core::workflow::WorkflowManager;

    let run_id = require_arg!(args, "run_id");
    let (conn, _config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let wf_mgr = WorkflowManager::new(&conn);
    let run = match wf_mgr.get_workflow_run(run_id) {
        Ok(Some(r)) => r,
        Ok(None) => return tool_err(format!("Workflow run not found: {run_id}")),
        Err(e) => return tool_err(e),
    };
    match wf_mgr.cancel_run(run_id, "Cancelled via MCP conductor_cancel_run") {
        Ok(()) => tool_ok(format!(
            "Workflow run {} ('{}') cancelled.",
            run_id, run.workflow_name
        )),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temp file DB with migrations applied, return the file (kept alive).
    fn make_test_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use conductor_core::db::open_database;
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        open_database(&path).expect("open_database");
        (file, path)
    }

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    fn args_with(key: &str, val: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), Value::String(val.to_string()));
        m
    }

    // -- read_resource_by_uri -----------------------------------------------

    #[test]
    fn test_read_resource_unknown_uri_returns_error() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://does-not-exist/foo");
        assert!(result.is_err(), "unknown URI should be an error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Unknown conductor://"),
            "error message should mention unknown URI, got: {msg}"
        );
    }

    #[test]
    fn test_read_resource_repos_empty_db() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://repos").expect("should succeed");
        assert!(
            result.contains("No repos registered"),
            "expected empty message, got: {result}"
        );
    }

    #[test]
    fn test_read_resource_repo_not_found() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://repo/no-such-repo");
        assert!(result.is_err(), "missing repo should be an error");
    }

    #[test]
    fn test_read_resource_tickets_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://tickets/ghost-repo");
        assert!(result.is_err(), "unknown repo should produce an error");
    }

    #[test]
    fn test_read_resource_worktrees_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://worktrees/ghost-repo");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_resource_runs_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://runs/ghost-repo");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_resource_run_not_found() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://run/01HXXXXXXXXXXXXXXXXXXXXXXX");
        assert!(result.is_err(), "non-existent run_id should be an error");
    }

    #[test]
    fn test_read_resource_workflows_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = read_resource_by_uri(&db, "conductor://workflows/ghost-repo");
        assert!(result.is_err());
    }

    // -- dispatch_tool -------------------------------------------------------

    #[test]
    fn test_dispatch_unknown_tool() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_nonexistent", &empty_args());
        assert_eq!(
            result.is_error,
            Some(true),
            "unknown tool should return is_error=true"
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Unknown tool"), "got: {text}");
    }

    #[test]
    fn test_dispatch_list_tickets_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_list_tickets", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_list_worktrees_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_list_worktrees", &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_get_run_missing_run_id_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_get_run", &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_get_run_nonexistent_run() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = dispatch_tool(&db, "conductor_get_run", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_list_runs_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_list_runs", &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_run_workflow_missing_args() {
        let (_f, db) = make_test_db();
        // Missing both "workflow" and "repo"
        let result = dispatch_tool(&db, "conductor_run_workflow", &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_run_workflow_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        let result = dispatch_tool(&db, "conductor_run_workflow", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_run_workflow_inputs_as_object() {
        let (_f, db) = make_test_db();
        let mut inputs_map = serde_json::Map::new();
        inputs_map.insert("key1".to_string(), Value::String("val1".to_string()));
        inputs_map.insert("key2".to_string(), Value::String("val2".to_string()));
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("inputs".to_string(), Value::Object(inputs_map));
        // Should fail at repo lookup, not at inputs parsing
        let result = dispatch_tool(&db, "conductor_run_workflow", &args);
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            !content.contains("inputs must be an object"),
            "Should not fail on inputs parsing"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_inputs_as_string_fails() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert(
            "inputs".to_string(),
            Value::String(r#"{"key":"val"}"#.to_string()),
        );
        let result = dispatch_tool(&db, "conductor_run_workflow", &args);
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            content.contains("inputs must be an object"),
            "Should fail with inputs type error"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_inputs_non_string_value_fails() {
        let (_f, db) = make_test_db();
        let mut inputs_map = serde_json::Map::new();
        inputs_map.insert("count".to_string(), Value::Number(42.into()));
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("inputs".to_string(), Value::Object(inputs_map));
        let result = dispatch_tool(&db, "conductor_run_workflow", &args);
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            content.contains("inputs.count must be a string value"),
            "Should fail with per-key type error"
        );
    }

    // -- gate tools (approve / reject) --------------------------------------

    /// Helper: set up a workflow run with a waiting gate step. Returns (run_id, step_id).
    fn make_waiting_gate(db_path: &std::path::Path) -> (String, String) {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::{WorkflowManager, WorkflowStepStatus};

        let conn = open_database(db_path).expect("open db");

        // FK: workflow_runs.parent_run_id references agent_runs.id
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");

        let mgr = WorkflowManager::new(&conn);

        let run = mgr
            .create_workflow_run("test-wf", None, &parent.id, false, "manual", None)
            .expect("create run");

        let step_id = mgr
            .insert_step(&run.id, "human_review", "reviewer", false, 0, 0)
            .expect("insert step");

        mgr.set_step_gate_info(&step_id, "human_approval", Some("Approve?"), "24h")
            .expect("set gate info");

        mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Waiting,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("set waiting status");

        (run.id, step_id)
    }

    #[test]
    fn test_dispatch_approve_gate_missing_run_id_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_approve_gate", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_reject_gate_missing_run_id_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_reject_gate", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_approve_gate_no_waiting_gate() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = dispatch_tool(&db, "conductor_approve_gate", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_reject_gate_no_waiting_gate() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = dispatch_tool(&db, "conductor_reject_gate", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_approve_gate_success() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let args = args_with("run_id", &run_id);
        let result = dispatch_tool(&db, "conductor_approve_gate", &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "approve_gate should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("approved"), "got: {text}");
    }

    #[test]
    fn test_dispatch_reject_gate_success() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let args = args_with("run_id", &run_id);
        let result = dispatch_tool(&db, "conductor_reject_gate", &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "reject_gate should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("rejected"), "got: {text}");
    }

    #[test]
    fn test_dispatch_approve_gate_with_feedback() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id.clone()));
        args.insert("feedback".to_string(), Value::String("LGTM".to_string()));
        let result = dispatch_tool(&db, "conductor_approve_gate", &args);
        assert_ne!(result.is_error, Some(true));

        // Verify the feedback was persisted
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let steps = mgr.get_workflow_steps(&run_id).expect("get steps");
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("LGTM"));
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("mcp"));
    }

    #[test]
    fn test_dispatch_reject_gate_with_feedback() {
        let (_f, db) = make_test_db();
        let (run_id, _step_id) = make_waiting_gate(&db);

        let mut args = serde_json::Map::new();
        args.insert("run_id".to_string(), Value::String(run_id.clone()));
        args.insert(
            "feedback".to_string(),
            Value::String("Needs more work".to_string()),
        );
        let result = dispatch_tool(&db, "conductor_reject_gate", &args);
        assert_ne!(result.is_error, Some(true));

        // Verify the feedback was persisted
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let steps = mgr.get_workflow_steps(&run_id).expect("get steps");
        assert_eq!(steps[0].gate_feedback.as_deref(), Some("Needs more work"));
        assert_eq!(steps[0].gate_approved_by.as_deref(), Some("mcp"));
    }

    // -- tool_list_repos ----------------------------------------------------

    #[test]
    fn test_dispatch_list_repos_empty_db() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_list_repos", &empty_args());
        assert_ne!(result.is_error, Some(true), "empty list should succeed");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("No repos registered"),
            "expected empty message, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_list_repos_populated() {
        use conductor_core::config::load_config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            let config = load_config().expect("load config");
            RepoManager::new(&conn, &config)
                .add(
                    "my-repo",
                    "/tmp/my-repo",
                    "https://github.com/acme/my-repo",
                    None,
                )
                .expect("add repo");
        }
        let result = dispatch_tool(&db, "conductor_list_repos", &empty_args());
        assert_ne!(result.is_error, Some(true), "populated list should succeed");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("my-repo"),
            "expected slug in output, got: {text}"
        );
        assert!(
            text.contains("/tmp/my-repo"),
            "expected local_path in output, got: {text}"
        );
        assert!(
            text.contains("https://github.com/acme/my-repo"),
            "expected remote_url in output, got: {text}"
        );
    }

    // -- tool_create_worktree -----------------------------------------------

    #[test]
    fn test_dispatch_create_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_create_worktree", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_create_worktree_missing_name_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(
            &db,
            "conductor_create_worktree",
            &args_with("repo", "my-repo"),
        );
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_create_worktree_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("name".to_string(), Value::String("feat-test".to_string()));
        let result = dispatch_tool(&db, "conductor_create_worktree", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_looks_like_ulid() {
        // Valid ULID: 26 uppercase alphanumeric chars
        assert!(looks_like_ulid("01HXYZABCDEFGHJKMNPQRSTVWX"));
        assert!(looks_like_ulid("01JRKBDR0B7W72V1EHNH78WKTF"));
        // GitHub issue numbers should NOT look like ULIDs
        assert!(!looks_like_ulid("680"));
        assert!(!looks_like_ulid("42"));
        // Too short / too long
        assert!(!looks_like_ulid("01HXYZ"));
        assert!(!looks_like_ulid("01HXYZABCDEFGHJKMNPQRSTVWXYZ"));
    }

    #[test]
    fn test_create_worktree_unknown_external_ticket_id_returns_error() {
        // Passing a numeric source_id that doesn't exist should return is_error=true
        // with a clear message mentioning the source_id.
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        RepoManager::new(&conn, &config)
            .add(
                "test-repo",
                "/tmp/test-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("add repo");

        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("test-repo".to_string()));
        args.insert("name".to_string(), Value::String("feat-test".to_string()));
        args.insert("ticket_id".to_string(), Value::String("999".to_string()));
        let result = dispatch_tool(&db, "conductor_create_worktree", &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("999"),
            "error should mention the source_id, got: {text}"
        );
    }

    // -- tool_delete_worktree -----------------------------------------------

    #[test]
    fn test_dispatch_delete_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_delete_worktree", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_delete_worktree_missing_slug_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(
            &db,
            "conductor_delete_worktree",
            &args_with("repo", "my-repo"),
        );
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_delete_worktree_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("slug".to_string(), Value::String("feat-wt".to_string()));
        let result = dispatch_tool(&db, "conductor_delete_worktree", &args);
        assert_eq!(result.is_error, Some(true));
    }

    // -- tool_sync_tickets --------------------------------------------------

    #[test]
    fn test_dispatch_sync_tickets_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_sync_tickets", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_sync_tickets_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(
            &db,
            "conductor_sync_tickets",
            &args_with("repo", "ghost-repo"),
        );
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_sync_tickets_no_sources_returns_error() {
        // A repo with no issue sources configured should return is_error=true.
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        let repo_mgr = RepoManager::new(&conn, &config);
        repo_mgr
            .add(
                "test-repo",
                "/tmp/test-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("add repo");

        let result = dispatch_tool(
            &db,
            "conductor_sync_tickets",
            &args_with("repo", "test-repo"),
        );
        assert_eq!(
            result.is_error,
            Some(true),
            "no sources should yield is_error=true"
        );
    }

    // -- tool_push_worktree -------------------------------------------------

    #[test]
    fn test_dispatch_push_worktree_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_push_worktree", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_push_worktree_missing_slug_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(
            &db,
            "conductor_push_worktree",
            &args_with("repo", "my-repo"),
        );
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_push_worktree_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("slug".to_string(), Value::String("feat-wt".to_string()));
        let result = dispatch_tool(&db, "conductor_push_worktree", &args);
        assert_eq!(result.is_error, Some(true));
    }

    // -- get_by_source_id (conductor-core TicketSyncer) ----------------------

    #[test]
    fn test_get_by_source_id_not_found() {
        use conductor_core::db::open_database;
        use conductor_core::tickets::TicketSyncer;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let syncer = TicketSyncer::new(&conn);
        let result = syncer.get_by_source_id("nonexistent-repo", "999");
        assert!(result.is_err(), "should fail for unknown repo+source_id");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("999") || err.to_lowercase().contains("not found"),
            "error should mention the source_id or 'not found', got: {err}"
        );
    }

    #[test]
    fn test_get_by_source_id_success() {
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;
        use conductor_core::tickets::{TicketInput, TicketSyncer};

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        let repo = RepoManager::new(&conn, &config)
            .add("test-repo", "/tmp/test", "https://github.com/x/y", None)
            .expect("add repo");
        let ticket = TicketInput {
            source_id: "42".to_string(),
            source_type: "github".to_string(),
            title: "Test ticket".to_string(),
            body: "body".to_string(),
            state: "open".to_string(),
            labels: "".to_string(),
            assignee: None,
            priority: None,
            url: "https://github.com/x/y/issues/42".to_string(),
            raw_json: "{}".to_string(),
            label_details: vec![],
        };
        let syncer = TicketSyncer::new(&conn);
        syncer.sync_and_close_tickets(&repo.id, "github", &[ticket]);
        let found = syncer
            .get_by_source_id(&repo.id, "42")
            .expect("ticket should be found");
        assert_eq!(found.source_id, "42");
        assert_eq!(found.title, "Test ticket");
    }

    // -- list_workflow_runs_by_repo_id (conductor-core) ---------------------

    #[test]
    fn test_list_workflow_runs_by_repo_id_empty() {
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let runs = mgr
            .list_workflow_runs_by_repo_id("nonexistent-repo-id", 50, 0)
            .expect("query should succeed");
        assert!(runs.is_empty(), "expected no runs for unknown repo");
    }

    #[test]
    fn test_list_workflow_runs_by_repo_id_scoped() {
        use conductor_core::agent::AgentManager;
        use conductor_core::config::Config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;
        use conductor_core::workflow::WorkflowManager;

        let (_f, db) = make_test_db();
        let conn = open_database(&db).expect("open db");
        let config = Config::default();
        let repo_mgr = RepoManager::new(&conn, &config);
        let repo_a = repo_mgr
            .add("repo-a", "/tmp/repo-a", "https://github.com/x/a", None)
            .expect("add repo-a");
        let repo_b = repo_mgr
            .add("repo-b", "/tmp/repo-b", "https://github.com/x/b", None)
            .expect("add repo-b");

        let agent_mgr = AgentManager::new(&conn);
        let mgr = WorkflowManager::new(&conn);

        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");

        // Create one run for repo-A and one for repo-B
        let _run_a = mgr
            .create_workflow_run_with_targets(
                "wf-a",
                None,
                None,
                Some(&repo_a.id),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .expect("create run A");
        let _run_b = mgr
            .create_workflow_run_with_targets(
                "wf-b",
                None,
                None,
                Some(&repo_b.id),
                &parent.id,
                false,
                "manual",
                None,
                None,
                None,
            )
            .expect("create run B");

        let runs_a = mgr
            .list_workflow_runs_by_repo_id(&repo_a.id, 50, 0)
            .expect("query A");
        let runs_b = mgr
            .list_workflow_runs_by_repo_id(&repo_b.id, 50, 0)
            .expect("query B");

        assert_eq!(runs_a.len(), 1, "expected 1 run for repo-a");
        assert_eq!(runs_a[0].workflow_name, "wf-a");
        assert_eq!(runs_b.len(), 1, "expected 1 run for repo-b");
        assert_eq!(runs_b[0].workflow_name, "wf-b");
    }

    // -- conversation_log_tail ----------------------------------------------
    //
    // Tests call `conversation_log_tail_from_dir` directly so we can pass a temp
    // directory path without mutating the HOME env var (which is not thread-safe
    // in parallel test runs).

    /// Write a minimal JSONL conversation log to the given path.
    fn write_jsonl(path: &std::path::Path, lines: &[serde_json::Value]) {
        use std::io::Write as _;
        let mut f = std::fs::File::create(path).expect("create jsonl");
        for line in lines {
            writeln!(f, "{}", line).expect("write line");
        }
    }

    /// Create a temp dir with a single `session.jsonl` containing `messages`,
    /// then call `conversation_log_tail_from_dir` on that dir.
    fn tail_from_messages(messages: &[serde_json::Value]) -> Option<String> {
        let dir = tempfile::TempDir::new().expect("tmpdir");
        write_jsonl(&dir.path().join("session.jsonl"), messages);
        conversation_log_tail_from_dir(dir.path())
    }

    #[test]
    fn test_conversation_log_tail_nonexistent_dir() {
        // A non-existent directory returns None.
        let result = conversation_log_tail_from_dir(std::path::Path::new(
            "/tmp/no-such-conductor-test-dir-xyz",
        ));
        assert!(result.is_none());
    }

    #[test]
    fn test_conversation_log_tail_empty_log() {
        // A JSONL file with no user/assistant messages returns None.
        let result = tail_from_messages(&[
            serde_json::json!({"type": "system", "message": {"content": "setup"}}),
        ]);
        assert!(result.is_none());
    }

    #[test]
    fn test_conversation_log_tail_skips_non_user_assistant() {
        // Only "user" and "assistant" type entries should appear in the tail.
        let result = tail_from_messages(&[
            serde_json::json!({"type": "system", "message": {"content": "sys"}}),
            serde_json::json!({"type": "tool_result", "message": {"content": "tool"}}),
        ]);
        assert!(result.is_none(), "no user/assistant messages → None");
    }

    #[test]
    fn test_conversation_log_tail_string_content() {
        // Messages with string content are included.
        let result = tail_from_messages(&[
            serde_json::json!({"type": "user", "message": {"content": "Hello from user"}}),
            serde_json::json!({"type": "assistant", "message": {"content": "Hello from assistant"}}),
        ])
        .expect("should return Some");
        assert!(result.contains("Hello from user"), "got: {result}");
        assert!(result.contains("Hello from assistant"), "got: {result}");
        assert!(result.contains("[user]"), "got: {result}");
        assert!(result.contains("[assistant]"), "got: {result}");
    }

    #[test]
    fn test_conversation_log_tail_array_content_blocks() {
        // Messages with array content blocks (type=text) are concatenated; other
        // block types (e.g. tool_use) are ignored.
        let result = tail_from_messages(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "block one "},
                    {"type": "tool_use", "id": "xyz"},
                    {"type": "text", "text": "block two"}
                ]
            }
        })])
        .expect("should return Some");
        assert!(result.contains("block one"), "got: {result}");
        assert!(result.contains("block two"), "got: {result}");
        assert!(
            !result.contains("xyz"),
            "tool_use id should not appear; got: {result}"
        );
    }

    #[test]
    fn test_conversation_log_tail_ring_buffer_cap() {
        // Only the last 20 messages should be retained.
        let messages: Vec<_> = (0..30_u32)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"content": format!("msg-{i:03}")}
                })
            })
            .collect();
        let result = tail_from_messages(&messages).expect("should return Some");
        // First 10 messages (000–009) should have been evicted.
        for i in 0..10_u32 {
            assert!(
                !result.contains(&format!("msg-{i:03}")),
                "msg-{i:03} should have been evicted; got: {result}"
            );
        }
        // Last 20 messages (010–029) should be present.
        for i in 10..30_u32 {
            assert!(
                result.contains(&format!("msg-{i:03}")),
                "msg-{i:03} should be present; got: {result}"
            );
        }
    }

    #[test]
    fn test_conversation_log_tail_truncates_long_text() {
        // Individual message text is capped at 500 chars.
        let long_text = "x".repeat(1000);
        let result = tail_from_messages(&[
            serde_json::json!({"type": "user", "message": {"content": long_text}}),
        ])
        .expect("should return Some");
        let x_count = result.chars().filter(|&c| c == 'x').count();
        assert_eq!(x_count, 500, "expected 500 chars of text, got {x_count}");
    }

    #[test]
    fn test_conversation_log_tail_skips_empty_text() {
        // Messages that produce empty text (e.g. only tool_use blocks) are skipped.
        let result = tail_from_messages(&[serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "tool_use", "id": "abc"}]
            }
        })]);
        assert!(result.is_none(), "only tool_use content → no text → None");
    }

    #[test]
    fn test_conversation_log_tail_picks_most_recent_file() {
        // When multiple JSONL files exist, the most recently modified one is used.
        let dir = tempfile::TempDir::new().expect("tmpdir");

        // Write the older file first so its mtime is earlier.
        let old_path = dir.path().join("old.jsonl");
        write_jsonl(
            &old_path,
            &[serde_json::json!({"type": "user", "message": {"content": "from old file"}})],
        );
        // Sleep briefly to ensure mtime differs between files.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let new_path = dir.path().join("new.jsonl");
        write_jsonl(
            &new_path,
            &[serde_json::json!({"type": "user", "message": {"content": "from new file"}})],
        );

        let result = conversation_log_tail_from_dir(dir.path()).expect("should return Some");
        assert!(
            result.contains("from new file"),
            "should use newest file; got: {result}"
        );
        assert!(
            !result.contains("from old file"),
            "should not use old file; got: {result}"
        );
    }

    // -- tool_cancel_run ----------------------------------------------------

    /// Helper: create a workflow run in the given status. Returns the run id.
    fn make_workflow_run_with_status(
        db_path: &std::path::Path,
        status: conductor_core::workflow::WorkflowRunStatus,
    ) -> String {
        use conductor_core::agent::AgentManager;
        use conductor_core::db::open_database;
        use conductor_core::workflow::WorkflowManager;

        let conn = open_database(db_path).expect("open db");
        let agent_mgr = AgentManager::new(&conn);
        let parent = agent_mgr
            .create_run(None, "workflow", None, None)
            .expect("create agent run");
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .create_workflow_run("test-wf", None, &parent.id, false, "manual", None)
            .expect("create workflow run");
        if !matches!(status, conductor_core::workflow::WorkflowRunStatus::Pending) {
            mgr.update_workflow_status(&run.id, status, None)
                .expect("update status");
        }
        run.id
    }

    #[test]
    fn test_dispatch_cancel_run_missing_arg() {
        let (_f, db) = make_test_db();
        let result = dispatch_tool(&db, "conductor_cancel_run", &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_not_found() {
        let (_f, db) = make_test_db();
        let args = args_with("run_id", "01HXXXXXXXXXXXXXXXXXXXXXXX");
        let result = dispatch_tool(&db, "conductor_cancel_run", &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not found"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_already_completed() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Completed);
        let args = args_with("run_id", &run_id);
        let result = dispatch_tool(&db, "conductor_cancel_run", &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("terminal state"), "got: {text}");
    }

    #[test]
    fn test_dispatch_cancel_run_already_failed() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Failed);
        let args = args_with("run_id", &run_id);
        let result = dispatch_tool(&db, "conductor_cancel_run", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_cancel_run_already_cancelled() {
        use conductor_core::workflow::WorkflowRunStatus;
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Cancelled);
        let args = args_with("run_id", &run_id);
        let result = dispatch_tool(&db, "conductor_cancel_run", &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_cancel_run_running() {
        use conductor_core::db::open_database;
        use conductor_core::workflow::{WorkflowManager, WorkflowRunStatus};
        let (_f, db) = make_test_db();
        let run_id = make_workflow_run_with_status(&db, WorkflowRunStatus::Running);
        let args = args_with("run_id", &run_id);
        let result = dispatch_tool(&db, "conductor_cancel_run", &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "cancel_run should succeed; got: {:?}",
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| &t.text)
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("cancelled"), "got: {text}");

        // Verify the run status was updated in the DB.
        let conn = open_database(&db).expect("open db");
        let mgr = WorkflowManager::new(&conn);
        let run = mgr
            .get_workflow_run(&run_id)
            .expect("query")
            .expect("run exists");
        assert_eq!(run.status, WorkflowRunStatus::Cancelled);
        assert_eq!(
            run.result_summary.as_deref(),
            Some("Cancelled via MCP conductor_cancel_run")
        );
    }
}
