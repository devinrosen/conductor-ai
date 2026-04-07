use conductor_core::worktree::WorktreeManager;

use crate::action::Action;
use crate::background;
use crate::state::Modal;
use crate::state::View;

use super::App;

impl App {
    pub(super) fn handle_push(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        if let Some(wt) = wt {
            let repo_slug = match self.state.data.repo_slug_map.get(&wt.repo_id) {
                Some(s) => s.clone(),
                None => {
                    self.state.status_message = Some("Cannot find repo for worktree".to_string());
                    return;
                }
            };
            let Some(bg_tx) = self.bg_tx.clone() else {
                return;
            };
            self.state.modal = Modal::Progress {
                message: "Pushing branch…".to_string(),
            };
            let config = self.config.clone();
            let wt_slug = wt.slug.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<String> {
                    let db = conductor_core::config::db_path();
                    let conn = conductor_core::db::open_database(&db)?;
                    let mgr = WorktreeManager::new(&conn, &config);
                    mgr.push(&repo_slug, &wt_slug).map_err(anyhow::Error::from)
                })();
                let _ = bg_tx.send(Action::PushComplete {
                    result: result.map_err(|e| e.to_string()),
                });
            });
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    pub(super) fn handle_create_pr(&mut self) {
        let wt = self
            .state
            .selected_worktree_id
            .as_ref()
            .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
            .cloned();

        if let Some(wt) = wt {
            let repo_slug = match self.state.data.repo_slug_map.get(&wt.repo_id) {
                Some(s) => s.clone(),
                None => {
                    self.state.status_message = Some("Cannot find repo for worktree".to_string());
                    return;
                }
            };
            let Some(bg_tx) = self.bg_tx.clone() else {
                return;
            };
            self.state.modal = Modal::Progress {
                message: "Creating PR…".to_string(),
            };
            let config = self.config.clone();
            let wt_slug = wt.slug.clone();
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<String> {
                    let db = conductor_core::config::db_path();
                    let conn = conductor_core::db::open_database(&db)?;
                    let mgr = WorktreeManager::new(&conn, &config);
                    mgr.create_pr(&repo_slug, &wt_slug, false)
                        .map_err(anyhow::Error::from)
                })();
                let _ = bg_tx.send(Action::PrCreateComplete {
                    result: result.map_err(|e| e.to_string()),
                });
            });
        } else {
            self.state.status_message = Some("Select a worktree first".to_string());
        }
    }

    pub(super) fn handle_sync_tickets(&mut self) {
        if self.state.ticket_sync_in_progress {
            self.state.status_message = Some("Sync already in progress...".to_string());
            return;
        }
        let Some(ref tx) = self.bg_tx else {
            self.state.status_message = Some("Background sender not ready".to_string());
            return;
        };
        self.state.ticket_sync_in_progress = true;

        // In RepoDetail view, scope sync to the currently focused repo.
        if self.state.view == View::RepoDetail {
            if let Some(ref repo_id) = self.state.selected_repo_id.clone() {
                if let Some(repo) = self
                    .state
                    .data
                    .repos
                    .iter()
                    .find(|r| &r.id == repo_id)
                    .cloned()
                {
                    self.state.status_message =
                        Some(format!("Syncing tickets for {}...", repo.slug));
                    background::spawn_ticket_sync_for_repo(
                        tx.clone(),
                        repo.id,
                        repo.slug,
                        repo.remote_url,
                        true,
                    );
                    return;
                }
            }
        }

        self.state.status_message = Some("Syncing tickets...".to_string());
        background::spawn_ticket_sync_once(tx.clone());
    }
}
