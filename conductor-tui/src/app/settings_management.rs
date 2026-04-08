use conductor_core::config::{AgentPermissionMode, AutoStartAgent};
use conductor_core::notification_event::NotificationEvent;
use conductor_core::notification_hooks::HookRunner;

use crate::action::Action;
use crate::state::{
    AppState, InputAction, Modal, SettingsCategory, SettingsFocus, SettingsDisplayCache, View,
};
use crate::ui::settings::{appearance_row, general_row};

use super::App;

impl App {
    /// Transition to the Settings view and populate display cache from current config.
    pub(super) fn handle_open_settings(&mut self) {
        self.state.previous_view = Some(self.state.view);
        self.state.view = View::Settings;
        self.state.settings_focus = SettingsFocus::CategoryList;
        self.state.settings_category = SettingsCategory::General;
        self.state.settings_category_index = 0;
        self.state.settings_row_index = 0;
        self.refresh_settings_display();
    }

    /// Rebuild `state.settings_display` from `self.config`.
    pub(super) fn refresh_settings_display(&mut self) {
        let cfg = &self.config;
        let model = cfg
            .general
            .model
            .clone()
            .unwrap_or_else(|| "(not set — use claude default)".into());
        let permission_mode = match cfg.general.agent_permission_mode {
            AgentPermissionMode::SkipPermissions => "skip-permissions",
            AgentPermissionMode::AutoMode => "auto-mode",
            AgentPermissionMode::Plan => "plan",
            AgentPermissionMode::RepoSafe => "repo-safe",
        }
        .to_string();
        let auto_start = match cfg.general.auto_start_agent {
            AutoStartAgent::Ask => "ask",
            AutoStartAgent::Always => "always",
            AutoStartAgent::Never => "never",
        }
        .to_string();
        let sync_interval = cfg.general.sync_interval_minutes.to_string();
        let auto_cleanup = if cfg.general.auto_cleanup_merged_branches {
            "on"
        } else {
            "off"
        }
        .to_string();
        let theme = cfg
            .general
            .theme
            .clone()
            .unwrap_or_else(|| "conductor (default)".into());

        let hooks = cfg
            .notify
            .hooks
            .iter()
            .map(|h| {
                let cmd = h
                    .run
                    .as_deref()
                    .or(h.url.as_deref())
                    .unwrap_or("(no command)")
                    .to_string();
                (h.on.clone(), cmd)
            })
            .collect();

        self.state.settings_display = SettingsDisplayCache {
            model,
            permission_mode,
            auto_start,
            sync_interval,
            auto_cleanup,
            theme,
            hooks,
        };
    }

    /// Handle [Enter] in the Settings right pane.
    pub(super) fn handle_settings_edit(&mut self) {
        if self.state.settings_focus != SettingsFocus::SettingsList {
            return;
        }
        let row = self.state.settings_row_index;
        match self.state.settings_category {
            SettingsCategory::General => match row {
                general_row::MODEL => {
                    self.state.modal = Modal::Input {
                        title: "Set global model".into(),
                        prompt: "Model (blank to clear):".into(),
                        value: self
                            .config
                            .general
                            .model
                            .clone()
                            .unwrap_or_default(),
                        on_submit: InputAction::SettingsSetModel,
                    };
                }
                general_row::SYNC_INTERVAL => {
                    self.state.modal = Modal::Input {
                        title: "Set sync interval".into(),
                        prompt: "Sync interval (minutes):".into(),
                        value: self.config.general.sync_interval_minutes.to_string(),
                        on_submit: InputAction::SettingsSetSyncInterval,
                    };
                }
                general_row::ISSUE_SOURCES => {
                    self.handle_manage_issue_sources();
                }
                _ => {}
            },
            SettingsCategory::Appearance => {
                if row == appearance_row::THEME {
                    self.handle_show_theme_picker();
                }
            }
            SettingsCategory::Notifications => {
                // Enter on a hook row — no edit modal for hooks; [t] fires test.
            }
        }
    }

    /// Handle [c] in the Settings right pane — cycle enum or toggle bool.
    pub(super) fn handle_settings_cycle_value(&mut self) {
        if self.state.settings_focus != SettingsFocus::SettingsList {
            return;
        }
        let row = self.state.settings_row_index;
        if self.state.settings_category == SettingsCategory::General {
            match row {
                general_row::PERMISSION_MODE => {
                    self.config.general.agent_permission_mode = match self
                        .config
                        .general
                        .agent_permission_mode
                    {
                        AgentPermissionMode::SkipPermissions => AgentPermissionMode::AutoMode,
                        AgentPermissionMode::AutoMode => AgentPermissionMode::Plan,
                        AgentPermissionMode::Plan => AgentPermissionMode::RepoSafe,
                        AgentPermissionMode::RepoSafe => AgentPermissionMode::SkipPermissions,
                    };
                    self.save_config_background();
                    self.refresh_settings_display();
                }
                general_row::AUTO_START => {
                    self.config.general.auto_start_agent = match self.config.general.auto_start_agent
                    {
                        AutoStartAgent::Ask => AutoStartAgent::Always,
                        AutoStartAgent::Always => AutoStartAgent::Never,
                        AutoStartAgent::Never => AutoStartAgent::Ask,
                    };
                    self.save_config_background();
                    self.refresh_settings_display();
                }
                general_row::AUTO_CLEANUP => {
                    self.config.general.auto_cleanup_merged_branches =
                        !self.config.general.auto_cleanup_merged_branches;
                    self.save_config_background();
                    self.refresh_settings_display();
                }
                _ => {}
            }
        }
    }

    /// Spawn a background thread to save the current config (non-blocking).
    fn save_config_background(&mut self) {
        let config = self.config.clone();
        std::thread::spawn(move || {
            if let Err(e) = conductor_core::config::save_config(&config) {
                tracing::warn!("failed to save config from settings: {e}");
            }
        });
    }

    /// Handle [t] — fire a synthetic test event through the hook at `hook_index`.
    pub(super) fn handle_settings_test_hook(&mut self, hook_index: usize) {
        let Some(hook) = self.config.notify.hooks.get(hook_index).cloned() else {
            self.state.modal = Modal::Error {
                message: format!("Hook index {hook_index} not found in config."),
            };
            return;
        };
        let Some(bg_tx) = self.bg_tx.clone() else {
            return;
        };

        self.state.status_message = Some(format!("Testing hook #{hook_index}…"));
        self.state.status_message_at = Some(std::time::Instant::now());

        std::thread::spawn(move || {
            let now = chrono::Utc::now().to_rfc3339();
            let event = NotificationEvent::synthetic_for_pattern(&hook.on, now);
            let runner = HookRunner::new(&[hook]);
            runner.fire(&event);
            // fire() is fire-and-forget; we report "fired" immediately.
            let _ = bg_tx.send(Action::SettingsHookTestComplete {
                hook_index,
                result: Ok(()),
            });
        });
    }

    /// Handle the background result of a hook test.
    pub(super) fn handle_settings_hook_test_complete(
        &mut self,
        hook_index: usize,
        result: Result<(), String>,
    ) {
        let msg = match &result {
            Ok(()) => format!("Hook #{hook_index} fired successfully."),
            Err(e) => format!("Hook #{hook_index} error: {e}"),
        };
        self.state
            .settings_hook_test_results
            .insert(hook_index, result);
        self.state.status_message = Some(msg);
        self.state.status_message_at = Some(std::time::Instant::now());
    }

    /// Handle category navigation in the Settings left pane.
    pub(super) fn settings_move_up(&mut self) {
        match self.state.settings_focus {
            SettingsFocus::CategoryList => {
                let len = SettingsCategory::all().len();
                if len == 0 {
                    return;
                }
                self.state.settings_category_index = self
                    .state
                    .settings_category_index
                    .checked_sub(1)
                    .unwrap_or(len - 1);
                self.update_selected_category();
            }
            SettingsFocus::SettingsList => {
                let count = self.settings_row_count();
                if count == 0 {
                    return;
                }
                self.state.settings_row_index = self
                    .state
                    .settings_row_index
                    .checked_sub(1)
                    .unwrap_or(count - 1);
            }
        }
    }

    /// Handle category navigation in the Settings left pane.
    pub(super) fn settings_move_down(&mut self) {
        match self.state.settings_focus {
            SettingsFocus::CategoryList => {
                let len = SettingsCategory::all().len();
                if len == 0 {
                    return;
                }
                self.state.settings_category_index =
                    (self.state.settings_category_index + 1) % len;
                self.update_selected_category();
            }
            SettingsFocus::SettingsList => {
                let count = self.settings_row_count();
                if count == 0 {
                    return;
                }
                self.state.settings_row_index =
                    (self.state.settings_row_index + 1) % count;
            }
        }
    }

    fn update_selected_category(&mut self) {
        self.state.settings_category =
            SettingsCategory::all()[self.state.settings_category_index];
        self.state.settings_row_index = 0;
    }

    fn settings_row_count(&self) -> usize {
        match self.state.settings_category {
            SettingsCategory::General => general_row::COUNT,
            SettingsCategory::Appearance => appearance_row::COUNT,
            SettingsCategory::Notifications => self.state.settings_display.hooks.len().max(1),
        }
    }

    /// Apply a submitted string value from a settings modal.
    pub(super) fn handle_settings_input_submit(&mut self, action: InputAction, value: String) {
        match action {
            InputAction::SettingsSetModel => {
                self.config.general.model = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.trim().to_string())
                };
                self.save_config_background();
                self.refresh_settings_display();
            }
            InputAction::SettingsSetSyncInterval => {
                if let Ok(mins) = value.trim().parse::<u32>() {
                    self.config.general.sync_interval_minutes = mins;
                    self.save_config_background();
                    self.refresh_settings_display();
                } else {
                    self.state.modal = Modal::Error {
                        message: format!("Invalid interval: \"{}\" — must be a number.", value),
                    };
                    return;
                }
            }
            _ => {}
        }
        self.state.modal = Modal::None;
    }

    /// Toggle pane focus within the Settings view.
    pub(super) fn settings_toggle_focus(&mut self) {
        self.state.settings_focus = match self.state.settings_focus {
            SettingsFocus::CategoryList => SettingsFocus::SettingsList,
            SettingsFocus::SettingsList => SettingsFocus::CategoryList,
        };
    }
}

impl AppState {
    /// Returns the currently selected hook index in the Notifications pane.
    pub fn settings_selected_hook_index(&self) -> Option<usize> {
        if self.settings_category != SettingsCategory::Notifications {
            return None;
        }
        if self.settings_display.hooks.is_empty() {
            return None;
        }
        Some(self.settings_row_index)
    }
}
