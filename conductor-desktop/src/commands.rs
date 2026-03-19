//! Tauri command bridge — exposes conductor-core managers to the frontend.
//!
//! Each function is a `#[tauri::command]` that Tauri's IPC layer exposes to
//! the JavaScript frontend via `invoke()`.

use conductor_core::milestone::{Milestone, MilestoneManager, MilestoneProgress};
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
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mgr = RepoManager::new(&db, &config);
    let repos = mgr.list().map_err(|e| e.to_string())?;
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
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let repo_mgr = RepoManager::new(&db, &config);
    let repo = repo_mgr.get_by_slug(repo_slug).map_err(|e| e.to_string())?;
    let wt_mgr = WorktreeManager::new(&db, &config);
    wt_mgr
        .list_by_repo_id(&repo.id, false)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Milestone commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_milestones(
    state: State<'_, AppState>,
    repo_slug: &str,
) -> Result<Vec<Milestone>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let repo_mgr = RepoManager::new(&db, &config);
    let repo = repo_mgr.get_by_slug(repo_slug).map_err(|e| e.to_string())?;
    let mgr = MilestoneManager::new(&db, &config);
    mgr.list_milestones(&repo.id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_milestone(
    state: State<'_, AppState>,
    repo_slug: &str,
    name: &str,
    description: &str,
    target_date: Option<&str>,
) -> Result<Milestone, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let repo_mgr = RepoManager::new(&db, &config);
    let repo = repo_mgr.get_by_slug(repo_slug).map_err(|e| e.to_string())?;
    let mgr = MilestoneManager::new(&db, &config);
    mgr.create_milestone(&repo.id, name, description, target_date)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn milestone_progress(
    state: State<'_, AppState>,
    milestone_id: &str,
) -> Result<MilestoneProgress, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mgr = MilestoneManager::new(&db, &config);
    mgr.milestone_progress(milestone_id)
        .map_err(|e| e.to_string())
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
        let cargo_bin = std::env::var("HOME")
            .map(|h| format!("{h}/.cargo/bin"))
            .unwrap_or_default();
        let extra_paths = ["/opt/homebrew/bin", "/usr/local/bin", cargo_bin.as_str()];
        if let Ok(current) = std::env::var("PATH") {
            let mut parts: Vec<&str> = extra_paths.to_vec();
            parts.push(&current);
            std::env::set_var("PATH", parts.join(":"));
        }
    }
}
