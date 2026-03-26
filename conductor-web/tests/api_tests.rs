use std::sync::Arc;

use conductor_core::agent::AgentManager;
use conductor_core::config::Config;
use conductor_core::notification_manager::{
    CreateNotification, NotificationManager, NotificationSeverity,
};
use conductor_core::repo::RepoManager;
use rusqlite::Connection;
use tokio::sync::{Mutex, RwLock};

use conductor_web::events::EventBus;
use conductor_web::routes::api_router;
use conductor_web::state::AppState;

/// Spawn a test server on a random port and return the base URL.
async fn spawn_test_server() -> String {
    spawn_test_server_with_setup(|_| {}).await
}

/// Spawn a test server with a DB setup callback invoked after migrations.
async fn spawn_test_server_with_setup(setup: impl Fn(&Connection)) -> String {
    let conn = conductor_core::test_helpers::create_test_conn();
    setup(&conn);

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(RwLock::new(Config::default())),
        events: EventBus::new(64),
        workflow_done_notify: None,
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

fn seed_worktrees_with_completed(conn: &Connection) {
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w1', 'r1', 'feat-active', 'feat/active', '/tmp/ws/feat-active', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, completed_at) \
         VALUES ('w2', 'r1', 'feat-merged', 'feat/merged', '/tmp/ws/feat-merged', 'merged', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at, completed_at) \
         VALUES ('w3', 'r1', 'feat-abandoned', 'feat/abandoned', '/tmp/ws/feat-abandoned', 'abandoned', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z')",
        [],
    ).unwrap();
}

#[tokio::test]
async fn test_list_worktrees_default_hides_completed() {
    let base = spawn_test_server_with_setup(seed_worktrees_with_completed).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/repos/r1/worktrees"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["slug"], "feat-active");
}

#[tokio::test]
async fn test_list_worktrees_show_completed_true_includes_all() {
    let base = spawn_test_server_with_setup(seed_worktrees_with_completed).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/repos/r1/worktrees?show_completed=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 3);
}

#[tokio::test]
async fn test_list_worktrees_show_completed_false_explicit_hides_completed() {
    let base = spawn_test_server_with_setup(seed_worktrees_with_completed).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base}/api/repos/r1/worktrees?show_completed=false"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["slug"], "feat-active");
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

// ── Repo-scoped agent cross-repo isolation tests ──────────────────────

/// Shared setup for cross-repo IDOR tests: spawns a server with one repo + one
/// agent run, then returns (base_url, repo_id, run_id).
async fn setup_repo_agent_run() -> (String, String, String) {
    use conductor_core::agent::AgentManager;
    use conductor_core::config::Config;
    use conductor_core::repo::RepoManager;

    let base = spawn_test_server_with_setup(|conn| {
        let config = Config::default();
        let repo_mgr = RepoManager::new(conn, &config);
        let repo = repo_mgr
            .register(
                "repo-a",
                "/tmp/repo-a",
                "https://github.com/test/a.git",
                None,
            )
            .unwrap();
        let mgr = AgentManager::new(conn);
        mgr.create_repo_run(&repo.id, "test prompt", None, None)
            .unwrap();
    })
    .await;

    let client = reqwest::Client::new();

    let repos: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo_id = repos[0]["id"].as_str().unwrap().to_string();

    let runs: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos/{repo_id}/agent/runs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs.len(), 1);
    let run_id = runs[0]["id"].as_str().unwrap().to_string();

    (base, repo_id, run_id)
}

#[tokio::test]
async fn test_stop_repo_agent_rejects_wrong_repo_id() {
    let (base, _repo_id, run_id) = setup_repo_agent_run().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{base}/api/repos/nonexistent-repo/agent/{run_id}/stop"
        ))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "cross-repo stop should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn test_repo_agent_events_rejects_wrong_repo_id() {
    let (base, _repo_id, run_id) = setup_repo_agent_run().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base}/api/repos/nonexistent-repo/agent/{run_id}/events"
        ))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "cross-repo events should be rejected, got {}",
        resp.status()
    );
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

// ── Seed helpers ──────────────────────────────────────────────────────

fn seed_repo_and_worktree(conn: &Connection) {
    conn.execute(
        "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
         VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
}

fn seed_agent_run(conn: &Connection) {
    seed_repo_and_worktree(conn);
    let mgr = AgentManager::new(conn);
    mgr.create_run(Some("w1"), "test prompt", None, None)
        .unwrap();
}

fn seed_repo_agent_run(conn: &Connection) {
    let config = Config::default();
    let repo_mgr = RepoManager::new(conn, &config);
    let repo = repo_mgr
        .register(
            "test-repo",
            "/tmp/repo",
            "https://github.com/test/repo.git",
            None,
        )
        .unwrap();
    let mgr = AgentManager::new(conn);
    mgr.create_repo_run(&repo.id, "repo prompt", None, None)
        .unwrap();
}

fn seed_notification(conn: &Connection) {
    let mgr = NotificationManager::new(conn);
    mgr.create_notification(&CreateNotification {
        kind: "test",
        title: "Test notification",
        body: "This is a test",
        severity: NotificationSeverity::Info,
        entity_id: None,
        entity_type: None,
    })
    .unwrap();
}

// ── Agent read/query route tests ──────────────────────────────────────

#[tokio::test]
async fn test_list_agent_runs_empty() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent-runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

/// Fetch the first agent run ID for worktree w1 from the API.
async fn fetch_run_id(base: &str) -> String {
    let runs: Vec<serde_json::Value> = reqwest::get(format!("{base}/api/worktrees/w1/agent-runs"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    runs[0]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_list_agent_runs_with_data() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent-runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
}

#[tokio::test]
async fn test_latest_run_none() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/latest"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_null());
}

#[tokio::test]
async fn test_latest_run_with_data() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/latest"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn test_latest_runs_by_worktree() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/agent/latest-runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
    assert_eq!(body.as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn test_latest_runs_by_worktree_for_repo() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let resp = reqwest::get(format!("{base}/api/repos/r1/agent/latest-runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
    assert_eq!(body.as_object().unwrap().len(), 1);
}

#[tokio::test]
async fn test_ticket_totals_empty() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/agent/ticket-totals"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
    assert_eq!(body.as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn test_ticket_totals_for_repo() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/repos/r1/agent/ticket-totals"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
}

#[tokio::test]
async fn test_get_agent_prompt_no_ticket() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/prompt"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["prompt"], "");
}

#[tokio::test]
async fn test_get_events_empty() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/events"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_get_run_events_empty() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let run_id = fetch_run_id(&base).await;
    let resp = reqwest::get(format!(
        "{base}/api/worktrees/w1/agent/runs/{run_id}/events"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_list_child_runs_empty() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let run_id = fetch_run_id(&base).await;
    let resp = reqwest::get(format!(
        "{base}/api/worktrees/w1/agent/runs/{run_id}/children"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_get_run_tree() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let run_id = fetch_run_id(&base).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/runs/{run_id}/tree"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1); // root run only
}

#[tokio::test]
async fn test_get_run_tree_totals() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let run_id = fetch_run_id(&base).await;
    let resp = reqwest::get(format!(
        "{base}/api/worktrees/w1/agent/runs/{run_id}/tree-totals"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
}

#[tokio::test]
async fn test_list_created_issues_empty() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/created-issues"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_list_repo_agent_runs_empty() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/repos/r1/agent/runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_list_repo_agent_runs_with_data() {
    let base = spawn_test_server_with_setup(seed_repo_agent_run).await;
    // Get repo id
    let repos: Vec<serde_json::Value> = reqwest::get(format!("{base}/api/repos"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo_id = repos[0]["id"].as_str().unwrap();
    let resp = reqwest::get(format!("{base}/api/repos/{repo_id}/agent/runs"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
}

// ── Agent error/IDOR tests ───────────────────────────────────────────

#[tokio::test]
async fn test_stop_repo_agent_nonexistent_run() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/repos/r1/agent/bad-id/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_repo_agent_events_nonexistent_run() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/repos/r1/agent/bad-id/events"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// ── Agent feedback route tests ───────────────────────────────────────

#[tokio::test]
async fn test_get_pending_feedback_none() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/worktrees/w1/agent/feedback"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_null());
}

#[tokio::test]
async fn test_submit_feedback_nonexistent() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{base}/api/worktrees/w1/agent/feedback/bad-id/respond"
        ))
        .json(&serde_json::json!({ "response": "test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_dismiss_feedback_nonexistent() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{base}/api/worktrees/w1/agent/feedback/bad-id/dismiss"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_list_run_feedback_empty() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    // Get the run id
    let runs: Vec<serde_json::Value> = reqwest::get(format!("{base}/api/worktrees/w1/agent/runs"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = runs[0]["id"].as_str().unwrap();
    let resp = reqwest::get(format!(
        "{base}/api/worktrees/w1/agent/runs/{run_id}/feedback"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

// ── Restart agent route tests ────────────────────────────────────────

fn seed_failed_agent_run(conn: &Connection) {
    seed_repo_and_worktree(conn);
    let mgr = AgentManager::new(conn);
    let run = mgr
        .create_run(Some("w1"), "test prompt", Some("feat-test"), None)
        .unwrap();
    mgr.update_run_failed(&run.id, "crashed").unwrap();
}

#[tokio::test]
async fn test_restart_agent_unknown_run() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{base}/api/worktrees/w1/agent/runs/nonexistent-id/restart"
        ))
        .send()
        .await
        .unwrap();
    // Unknown run_id → error from restart_run
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
}

#[tokio::test]
async fn test_restart_agent_rejects_active_run() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let client = reqwest::Client::new();
    let run_id = fetch_run_id(&base).await;

    let resp = client
        .post(format!(
            "{base}/api/worktrees/w1/agent/runs/{run_id}/restart"
        ))
        .send()
        .await
        .unwrap();
    // Active run → cannot restart
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
}

#[tokio::test]
async fn test_restart_agent_wrong_worktree_id() {
    let base = spawn_test_server_with_setup(seed_failed_agent_run).await;
    let client = reqwest::Client::new();
    let run_id = fetch_run_id(&base).await;

    // Use a different worktree_id than the one the run belongs to
    let resp = client
        .post(format!(
            "{base}/api/worktrees/wrong-wt/agent/runs/{run_id}/restart"
        ))
        .send()
        .await
        .unwrap();
    // IDOR guard → error
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
}

// ── Notification route tests ─────────────────────────────────────────

#[tokio::test]
async fn test_list_notifications_empty() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/notifications"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_list_notifications_with_data() {
    let base = spawn_test_server_with_setup(seed_notification).await;
    let resp = reqwest::get(format!("{base}/api/notifications"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["title"], "Test notification");
}

#[tokio::test]
async fn test_list_notifications_unread_only() {
    let base = spawn_test_server_with_setup(|conn| {
        let mgr = NotificationManager::new(conn);
        // Create two notifications
        let id1 = mgr
            .create_notification(&CreateNotification {
                kind: "test",
                title: "Read one",
                body: "body",
                severity: NotificationSeverity::Info,
                entity_id: None,
                entity_type: None,
            })
            .unwrap();
        mgr.create_notification(&CreateNotification {
            kind: "test",
            title: "Unread one",
            body: "body",
            severity: NotificationSeverity::Warning,
            entity_id: None,
            entity_type: None,
        })
        .unwrap();
        // Mark first as read
        mgr.mark_read(&id1).unwrap();
    })
    .await;
    let resp = reqwest::get(format!("{base}/api/notifications?unread_only=true"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["title"], "Unread one");
}

#[tokio::test]
async fn test_unread_count_zero() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/notifications/unread-count"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn test_unread_count_with_data() {
    let base = spawn_test_server_with_setup(|conn| {
        seed_notification(conn);
        seed_notification(conn);
    })
    .await;
    let resp = reqwest::get(format!("{base}/api/notifications/unread-count"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 2);
}

#[tokio::test]
async fn test_mark_read() {
    let base = spawn_test_server_with_setup(seed_notification).await;
    // Fetch the notification ID from the list endpoint
    let notifs: Vec<serde_json::Value> = reqwest::get(format!("{base}/api/notifications"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let notif_id = notifs[0]["id"].as_str().unwrap();
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/notifications/{notif_id}/read"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn test_mark_read_nonexistent_404() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/notifications/bad-id/read"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_mark_all_read() {
    let base = spawn_test_server_with_setup(|conn| {
        seed_notification(conn);
        seed_notification(conn);
    })
    .await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/notifications/read-all"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify count is now 0
    let resp = reqwest::get(format!("{base}/api/notifications/unread-count"))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 0);
}

// ── Model config route tests ─────────────────────────────────────────

#[tokio::test]
async fn test_get_global_model_default() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/config/model"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["model"].is_null());
}

#[tokio::test]
async fn test_list_known_models() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/config/known-models"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!body.is_empty());
    // Each model should have id and alias fields
    assert!(body[0]["id"].is_string());
    assert!(body[0]["alias"].is_string());
}

#[tokio::test]
async fn test_suggest_model() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/config/suggest-model"))
        .json(&serde_json::json!({ "prompt": "fix a bug" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["suggested"].is_string());
}

#[tokio::test]
async fn test_patch_global_model_set_and_clear() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Set a model
    let resp = client
        .patch(format!("{base}/api/config/model"))
        .json(&serde_json::json!({ "model": "claude-sonnet-4-20250514" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "claude-sonnet-4-20250514");

    // Clear the model by sending null
    let resp = client
        .patch(format!("{base}/api/config/model"))
        .json(&serde_json::json!({ "model": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["model"].is_null());
}

// ── Feature route tests ──────────────────────────────────────────────

#[tokio::test]
async fn test_list_features_empty() {
    let base = spawn_test_server_with_setup(seed_repo_and_worktree).await;
    let resp = reqwest::get(format!("{base}/api/repos/test-repo/features"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["features"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_features_nonexistent_repo() {
    let base = spawn_test_server().await;
    let resp = reqwest::get(format!("{base}/api/repos/bad/features"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Stop agent (worktree-scoped) tests ──────────────────────────────

#[tokio::test]
async fn test_stop_agent_happy_path() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let client = reqwest::Client::new();
    let run_id = fetch_run_id(&base).await;

    // Verify the run is active before stopping
    let runs: Vec<serde_json::Value> = reqwest::get(format!("{base}/api/worktrees/w1/agent-runs"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs[0]["status"], "running");

    // Stop the agent
    let resp = client
        .post(format!("{base}/api/worktrees/w1/agent/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "stop_agent should succeed for active run"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], run_id);
    assert_eq!(
        body["status"], "cancelled",
        "run should be marked cancelled"
    );
}

#[tokio::test]
async fn test_stop_agent_already_stopped() {
    let base = spawn_test_server_with_setup(seed_agent_run).await;
    let client = reqwest::Client::new();

    // First stop succeeds
    let resp = client
        .post(format!("{base}/api/worktrees/w1/agent/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Second stop should fail (not running)
    let resp = client
        .post(format!("{base}/api/worktrees/w1/agent/stop"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "stopping an already-cancelled run should error"
    );
}

// ── Stop repo agent helpers ─────────────────────────────────────────

/// Fetch the first repo ID and its first agent run ID from the API.
async fn fetch_repo_and_run_id(base: &str) -> (String, String) {
    let repos: Vec<serde_json::Value> = reqwest::get(format!("{base}/api/repos"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo_id = repos[0]["id"].as_str().unwrap().to_string();

    let runs: Vec<serde_json::Value> =
        reqwest::get(format!("{base}/api/repos/{repo_id}/agent/runs"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let run_id = runs[0]["id"].as_str().unwrap().to_string();
    (repo_id, run_id)
}

// ── Stop repo agent happy-path tests ────────────────────────────────

#[tokio::test]
async fn test_stop_repo_agent_happy_path() {
    let base = spawn_test_server_with_setup(seed_repo_agent_run).await;
    let client = reqwest::Client::new();
    let (repo_id, run_id) = fetch_repo_and_run_id(&base).await;

    // Stop the repo agent
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/agent/{run_id}/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "stop_repo_agent should succeed for active run"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], run_id);
    assert_eq!(
        body["status"], "cancelled",
        "run should be marked cancelled"
    );
}

#[tokio::test]
async fn test_stop_repo_agent_already_stopped() {
    let base = spawn_test_server_with_setup(seed_repo_agent_run).await;
    let client = reqwest::Client::new();
    let (repo_id, run_id) = fetch_repo_and_run_id(&base).await;

    // First stop succeeds
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/agent/{run_id}/stop"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Second stop should fail
    let resp = client
        .post(format!("{base}/api/repos/{repo_id}/agent/{run_id}/stop"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "stopping an already-cancelled run should error"
    );
}
