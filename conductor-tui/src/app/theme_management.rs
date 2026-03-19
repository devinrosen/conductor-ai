use crate::action::Action;
use crate::state::{DashboardRow, InputAction, Modal, View};

use super::App;

impl App {
    pub(super) fn handle_show_theme_picker(&mut self) {
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot open theme picker: background sender not ready.".into(),
            };
            return;
        };
        // Show a non-blocking progress modal while scanning ~/.conductor/themes/
        // off the TUI main thread, as required by the threading rule in CLAUDE.md.
        self.state.modal = Modal::Progress {
            message: "Loading themes…".into(),
        };
        std::thread::spawn(move || {
            let (all, mut warnings) = crate::theme::all_themes();
            // Pre-load all Theme objects so keypress preview is an in-memory
            // lookup with no file I/O on the TUI main thread.
            // Themes that fail to re-parse are excluded from both lists so the
            // picker never shows an entry with silently-incorrect preview colors.
            let mut themes: Vec<(String, String)> = Vec::new();
            let mut loaded_themes: Vec<crate::theme::Theme> = Vec::new();
            for (name, label) in all {
                match crate::theme::Theme::from_name(&name) {
                    Ok(t) => {
                        themes.push((name, label));
                        loaded_themes.push(t);
                    }
                    Err(e) => warnings.push(e),
                }
            }
            let _ = bg_tx.send(Action::ThemesLoaded {
                themes,
                loaded_themes,
                warnings,
            });
        });
    }

    pub(super) fn handle_themes_loaded(
        &mut self,
        themes: Vec<(String, String)>,
        loaded_themes: Vec<crate::theme::Theme>,
        warnings: Vec<String>,
    ) {
        let current_name = self
            .config
            .general
            .theme
            .clone()
            .unwrap_or_else(|| "conductor".to_string());
        let selected = themes
            .iter()
            .position(|(name, _)| name == current_name.as_str())
            .unwrap_or(0);
        self.state.modal = Modal::ThemePicker {
            themes,
            loaded_themes,
            selected,
            original_theme: self.state.theme,
            original_name: current_name,
        };
        // Surface any broken theme files as a status warning (non-fatal).
        if !warnings.is_empty() {
            self.state.status_message = Some(format!(
                "Warning: {} theme file(s) failed to parse — check your ~/.conductor/themes/ directory",
                warnings.len()
            ));
        }
    }

    pub(super) fn handle_theme_preview(&mut self, idx: usize) {
        // Use the pre-loaded Theme objects stored in the modal — no file I/O on
        // the TUI main thread.
        if let Modal::ThemePicker {
            ref loaded_themes,
            ref mut selected,
            ..
        } = self.state.modal
        {
            if let Some(theme) = loaded_themes.get(idx) {
                self.state.theme = *theme;
            }
            *selected = idx;
        }
    }

    pub(super) fn handle_theme_picker_confirm(&mut self, selected: usize) {
        let name_opt = if let Modal::ThemePicker { ref themes, .. } = self.state.modal {
            themes.get(selected).map(|(n, _)| n.clone())
        } else {
            None
        };
        let Some(name) = name_opt else {
            self.state.modal = Modal::None;
            return;
        };
        let Some(bg_tx) = self.bg_tx.clone() else {
            self.state.modal = Modal::Error {
                message: "Cannot save theme: background sender not ready.".into(),
            };
            return;
        };
        // Update in-memory config immediately (non-blocking).
        self.config.general.theme = Some(name.clone());
        // Write the updated config to disk off the TUI main thread to avoid
        // blocking the render loop.
        let config = self.config.clone();
        self.state.modal = Modal::Progress {
            message: format!("Saving theme \"{name}\"…"),
        };
        std::thread::spawn(move || {
            let result = conductor_core::config::save_config(&config)
                .map(|()| format!("Theme set to \"{name}\""))
                .map_err(|e| e.to_string());
            let _ = bg_tx.send(Action::ThemeSaveComplete { result });
        });
    }

    pub(super) fn handle_set_model(&mut self) {
        // Helper to compute effective default and source for a worktree context
        let resolve_wt_effective =
            |wt: &conductor_core::worktree::Worktree,
             config: &conductor_core::config::Config,
             repos: &[conductor_core::repo::Repo]| {
                let repo_model = repos
                    .iter()
                    .find(|r| r.id == wt.repo_id)
                    .and_then(|r| r.model.clone());
                if let Some(ref m) = wt.model {
                    (Some(m.clone()), "worktree".to_string())
                } else if let Some(ref m) = repo_model {
                    (Some(m.clone()), "repo".to_string())
                } else if let Some(ref m) = config.general.model {
                    (Some(m.clone()), "global config".to_string())
                } else {
                    (None, "not set".to_string())
                }
            };

        // Helper to find the initial selected index matching current model
        let initial_selected = |current: &Option<String>| -> usize {
            match current {
                Some(m) => conductor_core::models::KNOWN_MODELS
                    .iter()
                    .position(|km| km.id == m.as_str() || km.alias == m.as_str())
                    .unwrap_or(conductor_core::models::KNOWN_MODELS.len()),
                None => {
                    // Default to sonnet (index 1)
                    1
                }
            }
        };

        match self.state.view {
            View::Dashboard => {
                let rows = self.state.dashboard_rows();
                match rows.get(self.state.dashboard_index) {
                    Some(&DashboardRow::Worktree { idx: wt_idx, .. }) => {
                        let Some(wt) = self.state.data.worktrees.get(wt_idx).cloned() else {
                            return;
                        };
                        let repo_slug = self
                            .state
                            .data
                            .repo_slug_map
                            .get(&wt.repo_id)
                            .cloned()
                            .unwrap_or_default();
                        let (effective, source) =
                            resolve_wt_effective(&wt, &self.config, &self.state.data.repos);
                        let selected = initial_selected(&wt.model);
                        self.state.modal = Modal::ModelPicker {
                            context_label: format!("worktree: {}", wt.slug),
                            effective_default: effective,
                            effective_source: source,
                            selected,
                            custom_input: String::new(),
                            custom_active: false,
                            suggested: None,
                            allow_default: false,
                            on_submit: InputAction::SetWorktreeModel {
                                worktree_id: wt.id.clone(),
                                repo_slug,
                                slug: wt.slug.clone(),
                            },
                        };
                    }
                    Some(&DashboardRow::Repo(repo_idx)) => {
                        let Some(repo) = self.state.data.repos.get(repo_idx).cloned() else {
                            return;
                        };
                        let (effective, source) = if let Some(ref m) = repo.model {
                            (Some(m.clone()), "repo".to_string())
                        } else if let Some(ref m) = self.config.general.model {
                            (Some(m.clone()), "global config".to_string())
                        } else {
                            (None, "not set".to_string())
                        };
                        let selected = initial_selected(&repo.model);
                        self.state.modal = Modal::ModelPicker {
                            context_label: format!("repo: {}", repo.slug),
                            effective_default: effective,
                            effective_source: source,
                            selected,
                            custom_input: String::new(),
                            custom_active: false,
                            suggested: None,
                            allow_default: false,
                            on_submit: InputAction::SetRepoModel {
                                slug: repo.slug.clone(),
                            },
                        };
                    }
                    None => (),
                }
            }
            View::WorktreeDetail => {
                let Some(wt) = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                    .cloned()
                else {
                    return;
                };
                let repo_slug = self
                    .state
                    .data
                    .repo_slug_map
                    .get(&wt.repo_id)
                    .cloned()
                    .unwrap_or_default();
                let (effective, source) =
                    resolve_wt_effective(&wt, &self.config, &self.state.data.repos);
                let selected = initial_selected(&wt.model);
                self.state.modal = Modal::ModelPicker {
                    context_label: format!("worktree: {}", wt.slug),
                    effective_default: effective,
                    effective_source: source,
                    selected,
                    custom_input: String::new(),
                    custom_active: false,
                    suggested: None,
                    allow_default: false,
                    on_submit: InputAction::SetWorktreeModel {
                        worktree_id: wt.id.clone(),
                        repo_slug,
                        slug: wt.slug.clone(),
                    },
                };
            }
            View::RepoDetail => {
                let Some(repo) = self
                    .state
                    .selected_repo_id
                    .as_ref()
                    .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
                    .cloned()
                else {
                    return;
                };
                let (effective, source) = if let Some(ref m) = repo.model {
                    (Some(m.clone()), "repo".to_string())
                } else if let Some(ref m) = self.config.general.model {
                    (Some(m.clone()), "global config".to_string())
                } else {
                    (None, "not set".to_string())
                };
                let selected = initial_selected(&repo.model);
                self.state.modal = Modal::ModelPicker {
                    context_label: format!("repo: {}", repo.slug),
                    effective_default: effective,
                    effective_source: source,
                    selected,
                    custom_input: String::new(),
                    custom_active: false,
                    suggested: None,
                    allow_default: false,
                    on_submit: InputAction::SetRepoModel {
                        slug: repo.slug.clone(),
                    },
                };
            }
            _ => {}
        }
    }
}
