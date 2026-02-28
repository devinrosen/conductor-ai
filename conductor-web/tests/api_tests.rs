use std::sync::Arc;

use conductor_core::config::Config;
use conductor_core::db::migrations;
use rusqlite::Connection;
use tokio::sync::Mutex;

use conductor_web::routes::api_router;
use conductor_web::state::AppState;

/// Spawn a test server on a random port and return the base URL.
async fn spawn_test_server() -> String {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "on").unwrap();
    migrations::run(&conn).unwrap();

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        config: Arc::new(Config::default()),
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
async fn test_session_lifecycle() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // List sessions (empty)
    let sessions: Vec<serde_json::Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sessions.is_empty());

    // Start a session
    let resp = client
        .post(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let session: serde_json::Value = resp.json().await.unwrap();
    let session_id = session["id"].as_str().unwrap().to_string();
    assert!(session["ended_at"].is_null());

    // List sessions (one active)
    let sessions: Vec<serde_json::Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);

    // End the session
    let resp = client
        .post(format!("{base}/api/sessions/{session_id}/end"))
        .json(&serde_json::json!({ "notes": "test session" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // List sessions (one ended)
    let sessions: Vec<serde_json::Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(!sessions[0]["ended_at"].is_null());
    assert_eq!(sessions[0]["notes"], "test session");
}
