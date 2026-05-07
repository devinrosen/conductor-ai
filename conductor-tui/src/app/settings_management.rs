use conductor_core::config::{AgentPermissionMode, AutoStartAgent};
use conductor_core::notification_event::NotificationEvent;
use conductor_core::notification_hooks::HookRunner;

use crate::action::Action;
use crate::state::{
    AppState, ConfirmAction, InputAction, Modal, SettingsCategory, SettingsDisplayCache,
    SettingsFocus, View,
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
        let stall_timeout = match cfg.agents.stall_threshold_secs {
            None => "300 (default)".to_string(),
            Some(n) => n.to_string(),
        };
        let theme = self
            .tui_config
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

        let custom_models = self.config.general.custom_models.clone();

        // Build the runtimes display list. The built-in `claude` runtime is always
        // shown first; user-defined runtimes follow in sorted order by name.
        let mut runtimes: Vec<(String, String, usize, usize, bool)> = Vec::new();
        let claude_entry = self.config.runtimes.get("claude");
        let claude_models = claude_entry
            .map(|rt| rt.supported_models.len())
            .unwrap_or(0);
        let claude_env = claude_entry.map(|rt| rt.env.len()).unwrap_or(0);
        runtimes.push((
            "claude".to_string(),
            "claude".to_string(),
            claude_models,
            claude_env,
            true,
        ));
        let mut other: Vec<(&String, &conductor_core::config::RuntimeConfig)> = self
            .config
            .runtimes
            .iter()
            .filter(|(k, _)| k.as_str() != "claude")
            .collect();
        other.sort_by_key(|(k, _)| k.as_str());
        for (name, rt) in other {
            let type_hint = rt
                .runtime_type
                .clone()
                .unwrap_or_else(|| "claude".to_string());
            runtimes.push((
                name.clone(),
                type_hint,
                rt.supported_models.len(),
                rt.env.len(),
                false,
            ));
        }

        self.state.settings_display = SettingsDisplayCache {
            model,
            permission_mode,
            auto_start,
            sync_interval,
            auto_cleanup,
            stall_timeout,
            theme,
            hooks,
            custom_models,
            runtimes,
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
                        value: self.config.general.model.clone().unwrap_or_default(),
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
                general_row::STALL_TIMEOUT => {
                    let current = self
                        .config
                        .agents
                        .stall_threshold_secs
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    self.state.modal = Modal::Input {
                        title: "Set stall timeout".into(),
                        prompt: "Stall timeout (seconds, blank to reset to default):".into(),
                        value: current,
                        on_submit: InputAction::SettingsSetStallTimeout,
                    };
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
            SettingsCategory::Models => {
                // Enter in Models pane is a no-op; [a]/[d] are the actions.
            }
            SettingsCategory::Runtimes => {
                // Enter in Runtimes pane is a no-op; [a]/[e]/[d] are the actions.
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
                    self.config.general.agent_permission_mode =
                        match self.config.general.agent_permission_mode {
                            AgentPermissionMode::SkipPermissions => AgentPermissionMode::AutoMode,
                            AgentPermissionMode::AutoMode => AgentPermissionMode::Plan,
                            AgentPermissionMode::Plan => AgentPermissionMode::RepoSafe,
                            AgentPermissionMode::RepoSafe => AgentPermissionMode::SkipPermissions,
                        };
                    self.save_config_background();
                    self.refresh_settings_display();
                }
                general_row::AUTO_START => {
                    self.config.general.auto_start_agent =
                        match self.config.general.auto_start_agent {
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
    pub(super) fn save_config_background(&mut self) {
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
            self.state.modal = Modal::Error {
                message: "Cannot test hook: background sender not ready.".into(),
            };
            return;
        };

        self.state.status_message = Some(format!("Testing hook #{hook_index}…"));
        self.state.status_message_at = Some(std::time::Instant::now());

        std::thread::spawn(move || {
            let now = chrono::Utc::now().to_rfc3339();
            let event = NotificationEvent::synthetic_for_pattern(&hook.on, now);
            let runner = HookRunner::new(&[hook]);
            let result = runner.run_test(&event);
            let _ = bg_tx.send(Action::SettingsHookTestComplete { hook_index, result });
        });
    }

    /// Handle [o] — open the selected hook's local script file with `open`.
    pub(super) fn handle_settings_open_hook_script(&mut self, hook_index: usize) {
        let Some(hook) = self.config.notify.hooks.get(hook_index).cloned() else {
            self.state.status_message = Some(format!("Hook index {hook_index} not found."));
            self.state.status_message_at = Some(std::time::Instant::now());
            return;
        };

        let run = match hook.run.as_deref() {
            Some(r) => r.trim().to_string(),
            None => {
                self.state.status_message =
                    Some("Not a local script — edit config.toml to modify".into());
                self.state.status_message_at = Some(std::time::Instant::now());
                return;
            }
        };

        if !run.starts_with("~/") && !run.starts_with('/') && !run.starts_with("./") {
            self.state.status_message =
                Some("Not a local script — edit config.toml to modify".into());
            self.state.status_message_at = Some(std::time::Instant::now());
            return;
        }

        let resolved = if run.starts_with("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "~".into());
            format!("{}{}", home, &run[1..])
        } else {
            run.clone()
        };

        if !std::path::Path::new(&resolved).exists() {
            self.state.status_message = Some(format!("Script not found: {resolved}"));
            self.state.status_message_at = Some(std::time::Instant::now());
            return;
        }

        let resolved_for_spawn = resolved.clone();
        std::thread::spawn(move || {
            std::process::Command::new("open")
                .arg(&resolved_for_spawn)
                .status()
                .ok();
        });

        self.state.status_message = Some(format!("Opening {resolved}…"));
        self.state.status_message_at = Some(std::time::Instant::now());
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
                self.state.settings_category_index = (self.state.settings_category_index + 1) % len;
                self.update_selected_category();
            }
            SettingsFocus::SettingsList => {
                let count = self.settings_row_count();
                if count == 0 {
                    return;
                }
                self.state.settings_row_index = (self.state.settings_row_index + 1) % count;
            }
        }
    }

    fn update_selected_category(&mut self) {
        self.state.settings_category = SettingsCategory::all()[self.state.settings_category_index];
        self.state.settings_row_index = 0;
    }

    fn settings_row_count(&self) -> usize {
        match self.state.settings_category {
            SettingsCategory::General => general_row::COUNT,
            SettingsCategory::Appearance => appearance_row::COUNT,
            SettingsCategory::Notifications => self.state.settings_display.hooks.len().max(1),
            SettingsCategory::Models => self.state.settings_display.custom_models.len().max(1),
            SettingsCategory::Runtimes => self.state.settings_display.runtimes.len().max(1),
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
            InputAction::SettingsAddCustomModel => {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    self.state.modal = Modal::None;
                    return;
                }
                if !self.config.general.custom_models.contains(&trimmed) {
                    self.config.general.custom_models.push(trimmed);
                    self.save_config_background();
                    self.refresh_settings_display();
                }
            }
            InputAction::SettingsAddRuntime => {
                let name = value.trim().to_string();
                if name.is_empty() {
                    self.state.modal = Modal::None;
                    return;
                }
                if name == "claude" {
                    self.state.modal = Modal::Error {
                        message: "\"claude\" is the built-in runtime and cannot be added manually."
                            .into(),
                    };
                    return;
                }
                if self.config.runtimes.contains_key(&name) {
                    self.state.modal = Modal::Error {
                        message: format!("Runtime \"{name}\" already exists."),
                    };
                    return;
                }
                // Step 2: prompt for comma-separated supported models
                self.state.modal = Modal::Input {
                    title: format!("Add runtime: {name}"),
                    prompt: "Supported models (comma-separated):".into(),
                    value: String::new(),
                    on_submit: InputAction::SettingsAddRuntimeModels { name },
                };
                return;
            }
            InputAction::SettingsAddRuntimeModels { name } => {
                let models = parse_comma_models(&value);
                let rt = conductor_core::config::RuntimeConfig {
                    runtime_type: Some("claude".to_string()),
                    supported_models: models,
                    ..Default::default()
                };
                self.config.runtimes.insert(name, rt);
                self.save_config_background();
                self.refresh_settings_display();
            }
            InputAction::SettingsEditRuntimeModels { name } => {
                let models = parse_comma_models(&value);
                if let Some(rt) = self.config.runtimes.get_mut(&name) {
                    rt.supported_models = models;
                } else {
                    let rt = conductor_core::config::RuntimeConfig {
                        runtime_type: Some("claude".to_string()),
                        supported_models: models,
                        ..Default::default()
                    };
                    self.config.runtimes.insert(name, rt);
                }
                self.save_config_background();
                self.refresh_settings_display();
            }
            InputAction::SettingsSetStallTimeout => {
                if value.trim().is_empty() {
                    self.config.agents.stall_threshold_secs = None;
                } else {
                    match value.trim().parse::<u64>() {
                        Ok(n) if n > 0 => {
                            self.config.agents.stall_threshold_secs = Some(n);
                        }
                        _ => {
                            self.state.modal = Modal::Error {
                                message:
                                    "Stall timeout must be a positive integer or blank to reset."
                                        .into(),
                            };
                            return;
                        }
                    }
                }
                self.save_config_background();
                self.refresh_settings_display();
            }
            _ => {}
        }
        self.state.modal = Modal::None;
    }

    /// Open the Input modal to add a new custom model entry.
    pub(super) fn handle_models_add(&mut self) {
        self.state.modal = Modal::Input {
            title: "Add custom model".into(),
            prompt: "Model ID (e.g. claude-opus-4-7):".into(),
            value: String::new(),
            on_submit: InputAction::SettingsAddCustomModel,
        };
    }

    /// Open the Input modal to add a new runtime entry (step 1: name).
    pub(super) fn handle_runtimes_add(&mut self) {
        self.state.modal = Modal::Input {
            title: "Add runtime".into(),
            prompt: "Runtime name (e.g. claude-qwen-local):".into(),
            value: String::new(),
            on_submit: InputAction::SettingsAddRuntime,
        };
    }

    /// Open the Input modal to edit the currently selected runtime's models.
    pub(super) fn handle_runtimes_edit(&mut self) {
        let runtimes = &self.state.settings_display.runtimes;
        if runtimes.is_empty() {
            return;
        }
        let idx = self
            .state
            .settings_row_index
            .min(runtimes.len().saturating_sub(1));
        let (name, _, _, _, is_built_in) = &runtimes[idx];
        if *is_built_in {
            self.state.status_message = Some(
                "Built-in claude runtime is read-only \u{2014} use Models pane to add custom models"
                    .into(),
            );
            return;
        }
        let current = self
            .config
            .runtimes
            .get(name)
            .map(|rt| rt.supported_models.join(", "))
            .unwrap_or_default();
        self.state.modal = Modal::Input {
            title: format!("Edit models: {name}"),
            prompt: "Supported models (comma-separated):".into(),
            value: current,
            on_submit: InputAction::SettingsEditRuntimeModels { name: name.clone() },
        };
    }

    /// Open the Confirm modal to delete the currently selected runtime.
    pub(super) fn handle_runtimes_delete(&mut self) {
        let runtimes = &self.state.settings_display.runtimes;
        if runtimes.is_empty() {
            return;
        }
        let idx = self
            .state
            .settings_row_index
            .min(runtimes.len().saturating_sub(1));
        let (name, _, _, _, is_built_in) = &runtimes[idx];
        if *is_built_in {
            self.state.status_message = Some("Cannot delete built-in claude runtime".into());
            return;
        }
        let name = name.clone();
        self.state.modal = Modal::Confirm {
            title: "Delete runtime".into(),
            message: format!("Remove runtime \"{name}\" from config?"),
            on_confirm: ConfirmAction::DeleteRuntime { name },
        };
    }

    /// Open the Confirm modal to delete the currently selected custom model entry.
    pub(super) fn handle_models_delete(&mut self) {
        let models = &self.state.settings_display.custom_models;
        if models.is_empty() {
            return;
        }
        let idx = self
            .state
            .settings_row_index
            .min(models.len().saturating_sub(1));
        let model = models[idx].clone();
        self.state.modal = Modal::Confirm {
            title: "Delete custom model".into(),
            message: format!("Remove \"{model}\" from saved models?"),
            on_confirm: ConfirmAction::DeleteCustomModel { model },
        };
    }

    /// Toggle pane focus within the Settings view.
    pub(super) fn settings_toggle_focus(&mut self) {
        self.state.settings_focus = match self.state.settings_focus {
            SettingsFocus::CategoryList => SettingsFocus::SettingsList,
            SettingsFocus::SettingsList => SettingsFocus::CategoryList,
        };
    }
}

/// Parse a comma-separated list of model strings into a `Vec<String>`,
/// trimming whitespace and dropping empty entries.
fn parse_comma_models(s: &str) -> Vec<String> {
    s.split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect()
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
