//! Open / focus a worktree's interactive Claude Code session in tmux (RFC 021, #3158).

use conductor_core::session::launch::{
    open_session, FocusMode, OpenSessionOutcome, OpenSessionRequest, RealTmux,
};

use crate::action::Action;
use crate::state::{DashboardRow, Modal, View};

use super::App;

impl App {
    /// The worktree the `C` key applies to: the detail view's, or the selected dashboard row's.
    fn session_target_worktree(&self) -> Option<conductor_core::worktree::Worktree> {
        let wt_id = match self.state.view {
            View::WorktreeDetail => self.state.selected_worktree_id.clone()?,
            View::Dashboard => {
                let rows = self.state.dashboard_rows();
                match rows.get(self.state.dashboard_index) {
                    Some(&DashboardRow::Worktree { idx, .. }) => {
                        self.state.data.worktrees.get(idx)?.id.clone()
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        self.state
            .data
            .worktrees
            .iter()
            .find(|w| w.id == wt_id)
            .cloned()
    }

    pub(super) fn handle_open_session(&mut self) {
        let Some(wt) = self.session_target_worktree() else {
            self.state.status_message = Some("Select a worktree first".to_string());
            return;
        };
        if !wt.is_active() {
            self.state.status_message = Some("Cannot open a session in an archived worktree".into());
            return;
        }
        let Some(repo_slug) = self.state.data.repo_slug_map.get(&wt.repo_id).cloned() else {
            self.state.status_message = Some("Cannot find repo for worktree".to_string());
            return;
        };
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot open session: background sender not ready.".into(),
            };
            return;
        };
        self.state.modal = Modal::Progress {
            message: "Opening session…".to_string(),
        };
        let config = self.config.clone();
        // The thread opens its own connection on the caller's database, never the real home's.
        let db_path = conductor_core::db::connection_db_path(&self.conn)
            .unwrap_or_else(conductor_core::config::db_path);
        let focus = if std::env::var_os("TMUX").is_some() {
            FocusMode::Switch
        } else {
            FocusMode::Report
        };
        let wt_slug = wt.slug.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<OpenSessionOutcome> {
                let conn = conductor_core::db::open_database(&db_path)?;
                let req = OpenSessionRequest {
                    repo_slug,
                    worktree_slug: wt_slug,
                    new_session: false,
                    prompt: None,
                    window: None,
                    focus,
                };
                Ok(open_session(&conn, &config, &req, &RealTmux)?)
            })();
            let _ = bg_tx.send(Action::OpenSessionComplete {
                result: result.map_err(|e| e.to_string()),
            });
        });
    }

    pub(super) fn handle_open_session_complete(
        &mut self,
        result: Result<OpenSessionOutcome, String>,
    ) {
        self.state.modal = Modal::None;
        match result {
            Err(e) => {
                self.state.modal = Modal::Error {
                    message: format!("Open session failed: {e}"),
                }
            }
            Ok(outcome) => {
                let summary = outcome.summary();
                match outcome.attach_command() {
                    Some(cmd) if !outcome.switched() => {
                        let cmd = cmd.to_string();
                        self.copy_text_to_clipboard(cmd.clone());
                        self.state.status_message = Some(format!("{summary}. Copied: {cmd}"));
                    }
                    _ => self.state.status_message = Some(summary),
                }
                self.refresh_data();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::map_key;
    use crate::state::AppState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn capital_c_maps_to_open_session() {
        let state = AppState::new();
        let key = KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT);
        assert!(matches!(map_key(key, &state), Action::OpenSession));
    }
}
