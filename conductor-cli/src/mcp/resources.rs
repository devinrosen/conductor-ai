use std::collections::VecDeque;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::helpers::make_resource;
use rmcp::model::Resource;

// ---------------------------------------------------------------------------
// Resource enumeration
// ---------------------------------------------------------------------------

pub(super) fn enumerate_resources(db_path: &Path) -> anyhow::Result<Vec<Resource>> {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;

    let (conn, config) = super::helpers::open_db_and_config(db_path)?;

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

pub(super) fn read_resource_by_uri(db_path: &Path, uri: &str) -> anyhow::Result<String> {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::workflow::WorkflowManager;
    use conductor_core::worktree::WorktreeManager;
    use std::collections::HashMap;

    let (conn, config) = super::helpers::open_db_and_config(db_path)?;

    if uri == "conductor://repos" {
        let mgr = RepoManager::new(&conn, &config);
        let repos = mgr.list()?;
        if repos.is_empty() {
            return Ok(
                "No repos registered. Use `conductor repo register` to register one.".into(),
            );
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
        // Cache worktree lookups so we don't hit the DB (and load_config) once per run.
        let wt_mgr = WorktreeManager::new(&conn, &config);
        let mut wt_cache: HashMap<String, (String, String)> = HashMap::new();
        let mut out = String::new();
        for run in &repo_runs {
            let (slug, branch) = if let Some(wt_id) = run.worktree_id.as_deref() {
                if let Some(cached) = wt_cache.get(wt_id) {
                    (Some(cached.0.clone()), Some(cached.1.clone()))
                } else {
                    match wt_mgr.get_by_id(wt_id) {
                        Ok(wt) => {
                            wt_cache
                                .insert(wt_id.to_string(), (wt.slug.clone(), wt.branch.clone()));
                            (Some(wt.slug), Some(wt.branch))
                        }
                        Err(_) => (None, None),
                    }
                }
            } else {
                (None, None)
            };
            out.push_str(&format_run_summary_line(
                run,
                slug.as_deref(),
                branch.as_deref(),
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
        let claude_dir = config.general.resolved_claude_config_dir().ok();
        return Ok(format_run_detail_with_log(
            &conn,
            &run,
            &steps,
            claude_dir.as_deref(),
        ));
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
                out.push_str(&format_workflow_def(&def));
            }
        }
        return Ok(out);
    }

    anyhow::bail!("Unknown conductor:// URI: {uri}")
}

// ---------------------------------------------------------------------------
// Workflow formatting helpers (shared between resource reader and tool handlers)
// ---------------------------------------------------------------------------

pub(crate) fn format_workflow_def(def: &conductor_core::workflow::WorkflowDef) -> String {
    let mut out = format!(
        "name: {}\ndescription: {}\ntrigger: {}\n",
        def.name, def.description, def.trigger
    );
    if !def.inputs.is_empty() {
        out.push_str("inputs:\n");
        for input in &def.inputs {
            out.push_str(&format!("  - name: {}\n", input.name));
            out.push_str(&format!("    required: {}\n", input.required));
            let type_str = match input.input_type {
                conductor_core::workflow::InputType::Boolean => "boolean",
                conductor_core::workflow::InputType::String => "string",
            };
            out.push_str(&format!("    type: {type_str}\n"));
            if let Some(ref default) = input.default {
                out.push_str(&format!("    default: {default}\n"));
            }
            if let Some(ref description) = input.description {
                out.push_str(&format!("    description: {description}\n"));
            }
        }
    }
    if !def.targets.is_empty() {
        out.push_str(&format!("targets: [{}]\n", def.targets.join(", ")));
    }
    if let Some(ref group) = def.group {
        out.push_str(&format!("group: {group}\n"));
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Run formatting helpers (shared between resource reader and tool handlers)
// ---------------------------------------------------------------------------

pub(crate) fn format_run_summary_line(
    run: &conductor_core::workflow::WorkflowRun,
    worktree_slug: Option<&str>,
    worktree_branch: Option<&str>,
) -> String {
    let mut out = format!(
        "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\n",
        run.id, run.workflow_name, run.status, run.started_at
    );
    if let Some(slug) = worktree_slug {
        out.push_str(&format!("worktree_slug: {slug}\n"));
    }
    if let Some(branch) = worktree_branch {
        out.push_str(&format!("worktree_branch: {branch}\n"));
    }
    out.push('\n');
    out
}

pub(crate) fn format_run_summary_line_with_repo(
    run: &conductor_core::workflow::WorkflowRun,
    repo_slug: Option<&str>,
) -> String {
    format!(
        "repo: {}\nid: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\n\n",
        repo_slug.unwrap_or("unknown"),
        run.id,
        run.workflow_name,
        run.status,
        run.started_at
    )
}

fn format_run_detail(
    run: &conductor_core::workflow::WorkflowRun,
    steps: &[conductor_core::workflow::WorkflowRunStep],
    worktree_slug: Option<&str>,
    worktree_branch: Option<&str>,
) -> String {
    let mut out = format!(
        "id: {}\nworkflow: {}\nstatus: {}\nstarted_at: {}\nended_at: {}\nsummary: {}\n",
        run.id,
        run.workflow_name,
        run.status,
        run.started_at,
        run.ended_at.as_deref().unwrap_or("running"),
        run.result_summary.as_deref().unwrap_or("none")
    );
    if let Some(slug) = worktree_slug {
        out.push_str(&format!("worktree_slug: {slug}\n"));
    }
    if let Some(branch) = worktree_branch {
        out.push_str(&format!("worktree_branch: {branch}\n"));
    }
    // Aggregated usage metrics (populated on completion)
    let has_metrics = run.total_cost_usd.is_some()
        || run.total_turns.is_some()
        || run.total_duration_ms.is_some()
        || run.total_input_tokens.is_some();
    if has_metrics {
        out.push('\n');
        if let (Some(cost), Some(turns), Some(dur_ms)) =
            (run.total_cost_usd, run.total_turns, run.total_duration_ms)
        {
            out.push_str(&format!(
                "cost: ${cost:.4}   turns: {turns}   duration: {:.1}s\n",
                dur_ms as f64 / 1000.0
            ));
        }
        if let (Some(inp), Some(out_t)) = (run.total_input_tokens, run.total_output_tokens) {
            let cache_read = run.total_cache_read_input_tokens.unwrap_or(0);
            let cache_create = run.total_cache_creation_input_tokens.unwrap_or(0);
            out.push_str(&format!(
                "tokens: {inp} in / {out_t} out (cache: {cache_read} read / {cache_create} created)\n"
            ));
        }
        if let Some(ref model) = run.model {
            out.push_str(&format!("model: {model}\n"));
        }
    }
    out.push_str("\nsteps:\n");
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

/// Return the tail of the most recent Claude Code conversation log for a worktree.
///
/// Looks in `<claude_config_dir>/projects/<escaped>/` where `<escaped>` is the
/// worktree path with every `/` replaced by `-`. Falls back to `~/.claude` when
/// `claude_config_dir` is `None`. Returns `None` on any error or if no relevant
/// messages are found.
pub(crate) fn conversation_log_tail(
    worktree_path: &Path,
    claude_config_dir: Option<&Path>,
) -> Option<String> {
    let escaped = worktree_path.to_str()?.replace('/', "-");
    let base = match claude_config_dir {
        Some(dir) => dir.to_path_buf(),
        None => {
            let home = std::env::var("HOME").ok()?;
            PathBuf::from(home).join(".claude")
        }
    };
    let projects_dir = base.join("projects").join(&escaped);
    conversation_log_tail_from_dir(&projects_dir)
}

/// Inner implementation: read the most-recently-modified JSONL from `projects_dir`
/// and return the last 20 user/assistant messages. Separated for testability.
pub(crate) fn conversation_log_tail_from_dir(projects_dir: &Path) -> Option<String> {
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
pub(crate) fn format_run_detail_with_log(
    conn: &rusqlite::Connection,
    run: &conductor_core::workflow::WorkflowRun,
    steps: &[conductor_core::workflow::WorkflowRunStep],
    claude_config_dir: Option<&Path>,
) -> String {
    let (wt_slug, wt_branch, wt_path) = resolve_worktree_info(conn, run);
    let mut out = format_run_detail(run, steps, wt_slug.as_deref(), wt_branch.as_deref());
    if let Some(path) = wt_path {
        if let Some(log) = conversation_log_tail(&path, claude_config_dir) {
            out.push_str("\nconversation log (last 20 messages):\n");
            out.push_str(&log);
        }
    }
    out
}

/// Resolve the worktree slug, branch, and filesystem path for a workflow run.
/// Returns `(slug, branch, path)` — all `None` if there is no worktree or it has been deleted.
pub(crate) fn resolve_worktree_info(
    conn: &rusqlite::Connection,
    run: &conductor_core::workflow::WorkflowRun,
) -> (Option<String>, Option<String>, Option<std::path::PathBuf>) {
    let wt_id = match run.worktree_id.as_deref() {
        Some(id) => id,
        None => return (None, None, None),
    };
    let config = match conductor_core::config::load_config() {
        Ok(c) => c,
        Err(_) => return (None, None, None),
    };
    match conductor_core::worktree::WorktreeManager::new(conn, &config).get_by_id(wt_id) {
        Ok(wt) => {
            let path = Some(std::path::PathBuf::from(&wt.path));
            (Some(wt.slug), Some(wt.branch), path)
        }
        Err(_) => (None, None, None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use conductor_core::db::open_database;
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        open_database(&path).expect("open_database");
        (file, path)
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

    // -- tool_list_workflows / resource conductor://workflows/ ---------------

    /// Write a minimal `.conductor/workflows/<name>.wf` file under a temp dir
    /// and return the temp dir (kept alive).
    fn make_wf_dir_with_workflow(name: &str, content: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let wf_dir = dir.path().join(".conductor").join("workflows");
        std::fs::create_dir_all(&wf_dir).expect("create workflow dir");
        std::fs::write(wf_dir.join(format!("{name}.wf")), content).expect("write wf file");
        dir
    }

    #[test]
    fn test_resource_workflows_includes_input_schema() {
        // The resource handler conductor://workflows/<slug> must include input schemas,
        // consistent with the tool handler.
        use conductor_core::config::load_config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let wf_content = r#"
workflow build {
    meta { description = "Build workflow" trigger = "manual" targets = ["worktree"] }
    inputs {
        branch required
    }
    call builder
}
"#;
        let wf_dir = make_wf_dir_with_workflow("build", wf_content);
        let repo_path = wf_dir.path().to_str().unwrap();

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            let config = load_config().expect("load config");
            RepoManager::new(&conn, &config)
                .register("my-repo", repo_path, "https://github.com/x/y", None)
                .expect("register repo");
        }

        let result = read_resource_by_uri(&db, "conductor://workflows/my-repo")
            .expect("resource read should succeed");

        assert!(
            result.contains("name: build"),
            "missing name; got: {result}"
        );
        assert!(
            result.contains("inputs:"),
            "resource should include inputs section; got: {result}"
        );
        assert!(
            result.contains("name: branch"),
            "missing branch input; got: {result}"
        );
        assert!(
            result.contains("required: true"),
            "branch should be required; got: {result}"
        );
        drop(wf_dir);
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
}
