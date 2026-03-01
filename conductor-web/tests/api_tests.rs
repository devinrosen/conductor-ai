use std::sync::Arc;

use conductor_core::config::Config;
use conductor_core::db::migrations;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};

use conductor_web::events::EventBus;
use conductor_web::routes::api_router;
use conductor_web::state::AppState;

/// Spawn a test server on a random port and return the base URL.
async fn spawn_test_server() -> String {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "on").unwrap();
    migrations::run(&conn).unwrap();

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(Config::default())),
        events: EventBus::new(64),
    };

    let app = api_router().with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{}", addr)
}

#[tokio::test]
async fn test_list_repos_empty() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/repos")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_create_and_list_repo() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Create a repo
    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "remote_url": "https://github.com/test/repo.git",
            "slug": "test-repo",
            "local_path": "/tmp/test-repo"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(repo["slug"], "test-repo");
    assert_eq!(repo["remote_url"], "https://github.com/test/repo.git");
    assert!(!repo["id"].as_str().unwrap().is_empty());

    // List repos
    let repos: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["slug"], "test-repo");
}

#[tokio::test]
async fn test_create_repo_slug_inferred() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "remote_url": "https://github.com/org/my-project.git",
            "local_path": "/tmp/my-project"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let repo: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(repo["slug"], "my-project");
}

#[tokio::test]
async fn test_create_duplicate_repo_409() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "remote_url": "https://github.com/test/dup.git",
        "slug": "dup",
        "local_path": "/tmp/dup"
    });

    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn test_delete_repo() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Create
    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "remote_url": "https://github.com/test/del.git",
            "slug": "del",
            "local_path": "/tmp/del"
        }))
        .send()
        .await
        .unwrap();
    let repo: serde_json::Value = resp.json().await.unwrap();
    let id = repo["id"].as_str().unwrap();

    // Delete
    let resp = client
        .delete(format!("{base}/api/repos/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Confirm empty
    let repos: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(repos.is_empty());
}

#[tokio::test]
async fn test_delete_nonexistent_repo_404() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .delete(format!("{base}/api/repos/nonexistent-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_list_worktrees_empty() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Create a repo first
    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "remote_url": "https://github.com/test/wt.git",
            "slug": "wt",
            "local_path": "/tmp/wt"
        }))
        .send()
        .await
        .unwrap();
    let repo: serde_json::Value = resp.json().await.unwrap();
    let repo_id = repo["id"].as_str().unwrap();

    // List worktrees
    let resp = client
        .get(format!("{base}/api/repos/{repo_id}/worktrees"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_list_worktrees_nonexistent_repo_404() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/repos/nonexistent-id/worktrees"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_list_tickets_empty() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Create a repo
    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "remote_url": "https://github.com/test/tix.git",
            "slug": "tix",
            "local_path": "/tmp/tix"
        }))
        .send()
        .await
        .unwrap();
    let repo: serde_json::Value = resp.json().await.unwrap();
    let repo_id = repo["id"].as_str().unwrap();

    let resp = client
        .get(format!("{base}/api/repos/{repo_id}/tickets"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_list_all_tickets_empty() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/tickets")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_ticket_detail_empty() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Call detail for a ticket with no agent runs or linked worktrees
    let resp = client
        .get(format!("{base}/api/tickets/nonexistent-id/detail"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["agent_totals"].is_null());
    assert_eq!(body["worktrees"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_list_work_targets_default() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/config/work-targets"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let targets: Vec<serde_json::Value> = resp.json().await.unwrap();
    // Default config includes "VS Code"
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["name"], "VS Code");
    assert_eq!(targets[0]["command"], "code");
    assert_eq!(targets[0]["type"], "editor");
}

#[tokio::test]
async fn test_create_work_target() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/config/work-targets"))
        .json(&serde_json::json!({
            "name": "iTerm",
            "command": "open -a iTerm",
            "type": "terminal"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let targets: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[1]["name"], "iTerm");
    assert_eq!(targets[1]["type"], "terminal");
}

#[tokio::test]
async fn test_delete_work_target() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Delete the default VS Code target (index 0)
    let resp = client
        .delete(format!("{base}/api/config/work-targets/0"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let targets: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(targets.is_empty());
}

#[tokio::test]
async fn test_delete_work_target_out_of_range() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .delete(format!("{base}/api/config/work-targets/99"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
}

#[tokio::test]
async fn test_replace_work_targets() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/api/config/work-targets"))
        .json(&serde_json::json!([
            {"name": "Zed", "command": "zed", "type": "editor"},
            {"name": "Terminal", "command": "terminal", "type": "terminal"}
        ]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let targets: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0]["name"], "Zed");
    assert_eq!(targets[1]["name"], "Terminal");
}

// --- Issue Source tests ---

/// Helper: create a repo and return (repo_id, base_url)
async fn create_test_repo(base: &str, slug: &str) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "remote_url": format!("https://github.com/test/{slug}.git"),
            "slug": slug,
            "local_path": format!("/tmp/{slug}")
        }))
        .send()
        .await
        .unwrap();
    let repo: serde_json::Value = resp.json().await.unwrap();
    repo["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_list_issue_sources_empty() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "src-empty").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/repos/{repo_id}/sources"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let sources: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(sources.is_empty());
}

#[tokio::test]
async fn test_create_github_source_auto_infer() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "gh-infer").await;
    let client = reqwest::Client::new();

    // Create without config_json — should auto-infer from remote URL
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({
            "source_type": "github"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let source: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(source["source_type"], "github");

    // Verify config was auto-inferred
    let config: serde_json::Value =
        serde_json::from_str(source["config_json"].as_str().unwrap()).unwrap();
    assert_eq!(config["owner"], "test");
    assert_eq!(config["repo"], "gh-infer");
}

#[tokio::test]
async fn test_create_github_source_explicit_config() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "gh-explicit").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({
            "source_type": "github",
            "config_json": "{\"owner\":\"custom\",\"repo\":\"project\"}"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let source: serde_json::Value = resp.json().await.unwrap();
    let config: serde_json::Value =
        serde_json::from_str(source["config_json"].as_str().unwrap()).unwrap();
    assert_eq!(config["owner"], "custom");
    assert_eq!(config["repo"], "project");
}

#[tokio::test]
async fn test_create_jira_source() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "jira-src").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({
            "source_type": "jira",
            "config_json": "{\"jql\":\"project = TEST\",\"url\":\"https://jira.example.com\"}"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let source: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(source["source_type"], "jira");
}

#[tokio::test]
async fn test_create_jira_source_requires_config() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "jira-noconfig").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({
            "source_type": "jira"
        }))
        .send()
        .await
        .unwrap();
    // Should fail — Jira requires config_json
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
}

#[tokio::test]
async fn test_create_duplicate_source_409() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "dup-src").await;
    let client = reqwest::Client::new();

    // First GitHub source
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({ "source_type": "github" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Duplicate should fail with 409
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({ "source_type": "github" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn test_delete_issue_source() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "del-src").await;
    let client = reqwest::Client::new();

    // Create
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({ "source_type": "github" }))
        .send()
        .await
        .unwrap();
    let source: serde_json::Value = resp.json().await.unwrap();
    let source_id = source["id"].as_str().unwrap();

    // Delete
    let resp = client
        .delete(format!("{base}/api/repos/{repo_id}/sources/{source_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Confirm empty
    let sources: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos/{repo_id}/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sources.is_empty());
}

#[tokio::test]
async fn test_both_source_types_allowed() {
    let base = spawn_test_server().await;
    let repo_id = create_test_repo(&base, "both-src").await;
    let client = reqwest::Client::new();

    // Add GitHub
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({ "source_type": "github" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Add Jira
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/sources"))
        .json(&serde_json::json!({
            "source_type": "jira",
            "config_json": "{\"jql\":\"project = X\",\"url\":\"https://j.example.com\"}"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // List should have 2
    let sources: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos/{repo_id}/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sources.len(), 2);
}

#[tokio::test]
async fn test_sse_endpoint_returns_event_stream() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/events"))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/event-stream"));
}
