use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};

pub(super) fn tool_list_tickets(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::{TicketFilter, TicketSyncer};

    let repo_slug = require_arg!(args, "repo");

    let labels: Vec<String> = get_arg(args, "label")
        .map(|s| {
            s.split(',')
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let search = get_arg(args, "search").map(|s| s.to_string());
    let include_closed = get_arg(args, "include_closed") == Some("true");

    let filter = TicketFilter {
        labels,
        search,
        include_closed,
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
    let tickets = match syncer.list_filtered(Some(&repo.id), &filter) {
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

pub(super) fn tool_sync_tickets(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::github;
    use conductor_core::issue_source::IssueSourceManager;
    use conductor_core::jira_acli;
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;
    use conductor_core::worktree::WorktreeManager;

    let repo_slug = require_arg!(args, "repo");
    let ticket_id_arg = get_arg(args, "ticket_id");
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

    // Single-ticket sync path
    if let Some(ticket_id_str) = ticket_id_arg {
        let worktree_mgr = WorktreeManager::new(&conn, &config);
        let (source_type, source_id) =
            match syncer.resolve_ticket_id(&worktree_mgr, &repo, ticket_id_str) {
                Ok(v) => v,
                Err(e) => return tool_err(e),
            };

        for source in &sources {
            if source.source_type != source_type {
                continue;
            }
            let fetch_result = match source.source_type.as_str() {
                "github" => {
                    let cfg: conductor_core::issue_source::GitHubConfig =
                        match serde_json::from_str(&source.config_json) {
                            Ok(c) => c,
                            Err(e) => return tool_err(format!("github config parse error: {e}")),
                        };
                    let issue_number: i64 = match source_id.parse() {
                        Ok(n) => n,
                        Err(_) => {
                            return tool_err(format!("invalid GitHub issue number: {source_id}"))
                        }
                    };
                    github::fetch_github_issue(&cfg.owner, &cfg.repo, issue_number, None)
                }
                "jira" => {
                    let cfg: conductor_core::issue_source::JiraConfig =
                        match serde_json::from_str(&source.config_json) {
                            Ok(c) => c,
                            Err(e) => return tool_err(format!("jira config parse error: {e}")),
                        };
                    jira_acli::fetch_jira_issue(&source_id, &cfg.url)
                }
                "vantage" => {
                    let cfg: conductor_core::issue_source::VantageConfig =
                        match serde_json::from_str(&source.config_json) {
                            Ok(c) => c,
                            Err(e) => return tool_err(format!("vantage config parse error: {e}")),
                        };
                    conductor_core::vantage::fetch_vantage_deliverable(&source_id, &cfg.sdlc_root)
                }
                other => return tool_err(format!("Unknown source type: {other}")),
            };
            match fetch_result {
                Ok(ticket) => {
                    if let Err(e) = syncer.upsert_tickets(&repo.id, &[ticket]) {
                        return tool_err(format!("upsert failed: {e}"));
                    }
                    let warn = if let Err(e) = syncer.mark_worktrees_for_closed_tickets(&repo.id) {
                        format!(" Warning: mark_worktrees_for_closed_tickets failed: {e}")
                    } else {
                        String::new()
                    };
                    return tool_ok(format!("Synced 1 ticket for {repo_slug}.{warn}"));
                }
                Err(e) => return tool_err(format!("{source_type}: {e}")),
            }
        }
        return tool_err(format!(
            "No {source_type} issue source configured for {repo_slug}."
        ));
    }

    // Full-sync path (unchanged)
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
            "vantage" => {
                let cfg: conductor_core::issue_source::VantageConfig =
                    match serde_json::from_str(&source.config_json) {
                        Ok(c) => c,
                        Err(e) => {
                            errors.push(format!("vantage config parse error: {e}"));
                            continue;
                        }
                    };
                conductor_core::vantage::sync_vantage_deliverables(
                    &cfg.project_id,
                    &cfg.sdlc_root,
                    repo_slug,
                )
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

pub(super) fn tool_upsert_ticket(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::{TicketInput, TicketSyncer};

    let repo_slug = require_arg!(args, "repo");
    let source_type = require_arg!(args, "source_type");
    let source_id = require_arg!(args, "source_id");
    let title = require_arg!(args, "title");
    let state = require_arg!(args, "state");
    let body = get_arg(args, "body").unwrap_or("").to_string();
    let url = get_arg(args, "url").unwrap_or("").to_string();
    let labels_raw = get_arg(args, "labels").unwrap_or("");
    let labels: Vec<String> = labels_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let assignee = get_arg(args, "assignee").map(|s| s.to_string());
    let priority = get_arg(args, "priority").map(|s| s.to_string());

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo = match RepoManager::new(&conn, &config).get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let ticket_input = TicketInput {
        source_type: source_type.to_string(),
        source_id: source_id.to_string(),
        title: title.to_string(),
        body,
        state: state.to_string(),
        labels,
        label_details: vec![],
        assignee,
        priority,
        url,
        raw_json: "{}".to_string(),
    };

    let syncer = TicketSyncer::new(&conn);
    match syncer.upsert_tickets(&repo.id, &[ticket_input]) {
        Ok(_) => tool_ok(format!(
            "Upserted ticket {source_type}#{source_id} into {repo_slug}."
        )),
        Err(e) => tool_err(format!("upsert failed: {e}")),
    }
}

pub(super) fn tool_delete_ticket(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::tickets::TicketSyncer;

    let repo_slug = require_arg!(args, "repo");
    let source_type = require_arg!(args, "source_type");
    let source_id = require_arg!(args, "source_id");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo = match RepoManager::new(&conn, &config).get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let syncer = TicketSyncer::new(&conn);
    match syncer.delete_ticket(&repo.id, source_type, source_id) {
        Ok(()) => tool_ok(format!(
            "Deleted ticket {source_type}#{source_id} from {repo_slug}."
        )),
        Err(e) => tool_err(format!("delete failed: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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

    fn full_ticket_args(repo: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("repo".to_string(), Value::String(repo.to_string()));
        m.insert(
            "source_type".to_string(),
            Value::String("github".to_string()),
        );
        m.insert("source_id".to_string(), Value::String("42".to_string()));
        m.insert(
            "title".to_string(),
            Value::String("Test ticket".to_string()),
        );
        m.insert("state".to_string(), Value::String("open".to_string()));
        m
    }

    fn seed_test_repo(db: &std::path::Path) {
        use conductor_core::db::open_database;
        let conn = open_database(db).expect("open db");
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
    }

    #[test]
    fn test_dispatch_list_tickets_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_list_tickets(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_sync_tickets_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_sync_tickets(&db, &empty_args());
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
        let result = tool_sync_tickets(&db, &args_with("repo", "ghost-repo"));
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
            .register(
                "test-repo",
                "/tmp/test-repo",
                "https://github.com/x/y",
                None,
            )
            .expect("register repo");

        let result = tool_sync_tickets(&db, &args_with("repo", "test-repo"));
        assert_eq!(
            result.is_error,
            Some(true),
            "no sources should yield is_error=true"
        );
    }

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
            .register("test-repo", "/tmp/test", "https://github.com/x/y", None)
            .expect("register repo");
        let ticket = TicketInput {
            source_id: "42".to_string(),
            source_type: "github".to_string(),
            title: "Test ticket".to_string(),
            body: "body".to_string(),
            state: "open".to_string(),
            labels: vec![],
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

    #[test]
    fn test_upsert_ticket_missing_repo() {
        let (_f, db) = make_test_db();
        let result = tool_upsert_ticket(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_missing_source_type() {
        let (_f, db) = make_test_db();
        let mut args = full_ticket_args("test-repo");
        args.remove("source_type");
        let result = tool_upsert_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_missing_source_id() {
        let (_f, db) = make_test_db();
        let mut args = full_ticket_args("test-repo");
        args.remove("source_id");
        let result = tool_upsert_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_missing_title() {
        let (_f, db) = make_test_db();
        let mut args = full_ticket_args("test-repo");
        args.remove("title");
        let result = tool_upsert_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_missing_state() {
        let (_f, db) = make_test_db();
        let mut args = full_ticket_args("test-repo");
        args.remove("state");
        let result = tool_upsert_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_invalid_state() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let mut args = full_ticket_args("test-repo");
        args.insert("state".to_string(), Value::String("pending".to_string()));
        let result = tool_upsert_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Invalid ticket state"),
            "expected invalid state error, got: {text}"
        );
        assert!(
            text.contains("open"),
            "should list valid states, got: {text}"
        );
        assert!(
            text.contains("in_progress"),
            "should list valid states, got: {text}"
        );
        assert!(
            text.contains("closed"),
            "should list valid states, got: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_unknown_repo() {
        let (_f, db) = make_test_db();
        let args = full_ticket_args("ghost-repo");
        let result = tool_upsert_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_upsert_ticket_success_minimal() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let args = full_ticket_args("test-repo");
        let result = tool_upsert_ticket(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "expected success, got: {:?}",
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
        assert!(
            text.contains("Upserted ticket github#42 into test-repo"),
            "unexpected success message: {text}"
        );
    }

    #[test]
    fn test_upsert_ticket_idempotent() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let args = full_ticket_args("test-repo");
        let result1 = tool_upsert_ticket(&db, &args);
        assert_ne!(result1.is_error, Some(true));
        let result2 = tool_upsert_ticket(&db, &args);
        assert_ne!(
            result2.is_error,
            Some(true),
            "second upsert should also succeed"
        );
    }

    #[test]
    fn test_upsert_ticket_optional_fields_default() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        // Only required fields — no body, url, assignee, priority, labels
        let args = full_ticket_args("test-repo");
        let result = tool_upsert_ticket(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "should succeed without optional fields"
        );
    }

    #[test]
    fn test_upsert_ticket_labels_comma_separated() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let mut args = full_ticket_args("test-repo");
        args.insert(
            "labels".to_string(),
            Value::String("bug,enhancement,help wanted".to_string()),
        );
        let result = tool_upsert_ticket(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "should succeed with comma-separated labels"
        );
    }

    #[test]
    fn test_upsert_ticket_labels_empty_string() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let mut args = full_ticket_args("test-repo");
        args.insert("labels".to_string(), Value::String("".to_string()));
        let result = tool_upsert_ticket(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "empty labels string should succeed"
        );
    }

    #[test]
    fn test_upsert_ticket_labels_whitespace_trimmed() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let mut args = full_ticket_args("test-repo");
        args.insert(
            "labels".to_string(),
            Value::String(" bug , enhancement ".to_string()),
        );
        let result = tool_upsert_ticket(&db, &args);
        assert_ne!(
            result.is_error,
            Some(true),
            "whitespace in labels should be trimmed and succeed"
        );
    }

    // ---- delete ticket tests ----

    fn delete_args(
        repo: &str,
        source_type: &str,
        source_id: &str,
    ) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert("repo".to_string(), Value::String(repo.to_string()));
        m.insert(
            "source_type".to_string(),
            Value::String(source_type.to_string()),
        );
        m.insert(
            "source_id".to_string(),
            Value::String(source_id.to_string()),
        );
        m
    }

    #[test]
    fn test_delete_ticket_missing_repo() {
        let (_f, db) = make_test_db();
        let result = tool_delete_ticket(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_delete_ticket_missing_source_type() {
        let (_f, db) = make_test_db();
        let result = tool_delete_ticket(&db, &args_with("repo", "test-repo"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Missing required argument"),
            "expected missing arg error, got: {text}"
        );
    }

    #[test]
    fn test_delete_ticket_unknown_repo() {
        let (_f, db) = make_test_db();
        let args = delete_args("ghost-repo", "github", "42");
        let result = tool_delete_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_delete_ticket_not_found() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        let args = delete_args("test-repo", "github", "999");
        let result = tool_delete_ticket(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("not found") || text.contains("Not found"),
            "expected not-found error, got: {text}"
        );
    }

    #[test]
    fn test_delete_ticket_success() {
        let (_f, db) = make_test_db();
        seed_test_repo(&db);
        // First upsert a ticket
        let upsert_args = full_ticket_args("test-repo");
        let upsert_result = tool_upsert_ticket(&db, &upsert_args);
        assert_ne!(upsert_result.is_error, Some(true), "upsert should succeed");

        // Now delete it
        let del_args = delete_args("test-repo", "github", "42");
        let result = tool_delete_ticket(&db, &del_args);
        assert_ne!(
            result.is_error,
            Some(true),
            "delete should succeed, got: {:?}",
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
        assert!(
            text.contains("Deleted ticket github#42 from test-repo"),
            "unexpected success message: {text}"
        );

        // Deleting again should fail (not found)
        let result2 = tool_delete_ticket(&db, &del_args);
        assert_eq!(
            result2.is_error,
            Some(true),
            "second delete should fail (not found)"
        );
    }

    #[test]
    fn test_delete_ticket_nullifies_workflow_run_ticket_id() {
        use conductor_core::db::open_database;

        let (_f, db) = make_test_db();
        seed_test_repo(&db);

        // Upsert a ticket
        let upsert_args = full_ticket_args("test-repo");
        let upsert_result = tool_upsert_ticket(&db, &upsert_args);
        assert_ne!(upsert_result.is_error, Some(true), "upsert should succeed");

        let conn = open_database(&db).expect("open db");

        // Get the ticket id
        let ticket_id: String = conn
            .query_row(
                "SELECT id FROM tickets WHERE repo_id = 'r1' AND source_type = 'github' AND source_id = '42'",
                [],
                |row| row.get(0),
            )
            .expect("ticket should exist");

        // Insert a worktree so we can create an agent_run
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, created_at) \
             VALUES ('wt1', 'r1', 'test-wt', 'feat/test', '/tmp/wt', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        // Insert an agent_run (required by workflow_runs.parent_run_id FK)
        conn.execute(
            "INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at) \
             VALUES ('ar1', 'wt1', 'test', 'completed', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        // Insert a workflow_run linked to the ticket
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, parent_run_id, status, started_at, ticket_id, repo_id) \
             VALUES ('wr1', 'test-wf', 'ar1', 'completed', '2024-01-01T00:00:00Z', ?1, 'r1')",
            rusqlite::params![ticket_id],
        )
        .unwrap();

        // Verify ticket_id is set
        let before: Option<String> = conn
            .query_row(
                "SELECT ticket_id FROM workflow_runs WHERE id = 'wr1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before.as_deref(), Some(ticket_id.as_str()));

        drop(conn);

        // Delete the ticket
        let del_args = delete_args("test-repo", "github", "42");
        let result = tool_delete_ticket(&db, &del_args);
        assert_ne!(result.is_error, Some(true), "delete should succeed");

        // Verify the workflow_run's ticket_id is now NULL
        let conn = open_database(&db).expect("open db");
        let after: Option<String> = conn
            .query_row(
                "SELECT ticket_id FROM workflow_runs WHERE id = 'wr1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            after.is_none(),
            "workflow_run ticket_id should be NULL after ticket deletion, got: {after:?}"
        );
    }
}
