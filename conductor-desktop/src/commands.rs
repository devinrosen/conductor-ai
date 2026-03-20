//! Tauri command bridge — exposes conductor-core managers to the frontend.
//!
//! Each function is a `#[tauri::command]` that Tauri's IPC layer exposes to
//! the JavaScript frontend via `invoke()`.

use conductor_core::repo::RepoManager;
use conductor_core::worktree::{Worktree, WorktreeManager};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct RepoInfo {
    pub id: String,
    pub slug: String,
    pub remote_url: String,
    pub default_branch: String,
}

// ---------------------------------------------------------------------------
// Repo commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_repos(state: State<'_, AppState>) -> Result<Vec<RepoInfo>, String> {
    let (db, config) = state.lock_both()?;
    let mgr = RepoManager::new(&db, &config);
    let repos = mgr.list().map_err(|e| format!("list repos: {e}"))?;
    Ok(repos
        .into_iter()
        .map(|r| RepoInfo {
            id: r.id,
            slug: r.slug,
            remote_url: r.remote_url,
            default_branch: r.default_branch,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Worktree commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_worktrees(
    state: State<'_, AppState>,
    repo_slug: &str,
) -> Result<Vec<Worktree>, String> {
    let (db, config) = state.lock_both()?;
    let repo_mgr = RepoManager::new(&db, &config);
    let repo = repo_mgr
        .get_by_slug(repo_slug)
        .map_err(|e| format!("repo '{}': {}", repo_slug, e))?;
    let wt_mgr = WorktreeManager::new(&db, &config);
    wt_mgr
        .list_by_repo_id(&repo.id, false)
        .map_err(|e| format!("list worktrees for repo '{}': {}", repo_slug, e))
}

// ---------------------------------------------------------------------------
// macOS PATH fixup
// ---------------------------------------------------------------------------

/// On macOS, GUI apps don't inherit the user's shell PATH. This function
/// resolves common tool locations (homebrew, cargo) and prepends them.
///
/// Learned from global-sdlc's Wails desktop implementation.
pub fn fixup_macos_path() {
    if cfg!(target_os = "macos") {
        let cargo_bin = match std::env::var("HOME") {
            Ok(h) => format!("{h}/.cargo/bin"),
            Err(_) => String::new(),
        };
        let mut extra_paths: Vec<&str> = vec!["/opt/homebrew/bin", "/usr/local/bin"];
        if !cargo_bin.is_empty() {
            extra_paths.push(cargo_bin.as_str());
        }
        let current = std::env::var("PATH").unwrap_or_default();
        let mut parts: Vec<&str> = extra_paths.to_vec();
        if !current.is_empty() {
            parts.push(&current);
        }
        std::env::set_var("PATH", parts.join(":"));
    }
}
