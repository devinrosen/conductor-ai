use conductor_core::config::{AgentPermissionMode, AutoStartAgent};
use conductor_core::notification_event::NotificationEvent;
use conductor_core::notification_hooks::HookRunner;

use crate::action::Action;
use crate::state::{
    AppState, ConfirmAction, FormAction, FormField, FormFieldType, InputAction, Modal,
    RuntimeDetailFocus, RuntimeDetailState, RuntimeDisplayRow, SettingsCategory,
    SettingsDisplayCache, SettingsFocus, View,
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

        // Build the runtimes display list. The built-in `claude` runtime is always
        // shown first; user-defined runtimes follow in sorted order by name.
        let runtime_row = |name: &str,
                           rt: Option<&conductor_core::config::RuntimeConfig>,
                           type_hint: String,
                           is_built_in: bool|
         -> RuntimeDisplayRow {
            let (models, env_count, env) = match rt {
                Some(rt) => {
                    let mut env: Vec<(String, String)> =
                        rt.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    env.sort_by(|a, b| a.0.cmp(&b.0));
                    (rt.supported_models.clone(), rt.env.len(), env)
                }
                None => (Vec::new(), 0, Vec::new()),
            };
            RuntimeDisplayRow {
                name: name.to_string(),
                type_hint,
                model_count: models.len(),
                env_count,
                is_built_in,
                models,
                env,
            }
        };

        let mut runtimes: Vec<RuntimeDisplayRow> = Vec::new();
        let claude_entry = self.config.runtimes.get("claude");
        runtimes.push(runtime_row(
            "claude",
            claude_entry,
            "claude".to_string(),
            true,
        ));
        for (name, rt) in crate::state::user_runtimes_sorted(&self.config.runtimes) {
            let type_hint = rt
                .runtime_type
                .clone()
                .unwrap_or_else(|| "claude".to_string());
            runtimes.push(runtime_row(name, Some(rt), type_hint, false));
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
            SettingsCategory::Runtimes => {
                // Enter on a runtime row drills into its detail view.
                self.handle_runtimes_edit();
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
    ///
    /// Skipped under `cfg(test)`: `save_config` writes to the user's real
    /// `~/.conductor/config.toml` (the global path resolved by
    /// `conductor_dir()`), and unit tests construct an in-memory `Config`
    /// with fixture data — running them would clobber the developer's
    /// actual config. Production builds (the `conductor-tui` binary) do
    /// not set `cfg(test)` and persist normally.
    pub(super) fn save_config_background(&mut self) {
        if cfg!(test) {
            return;
        }
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
        if self.runtime_detail_move(-1) {
            return;
        }
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
        if self.runtime_detail_move(1) {
            return;
        }
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

    /// If the runtime detail pane is open, move the focused-section selection
    /// by `delta` (wrapping). Returns true when handled (caller should not
    /// fall through to normal Settings navigation).
    fn runtime_detail_move(&mut self, delta: i32) -> bool {
        if self.state.settings_focus != SettingsFocus::SettingsList {
            return false;
        }
        let Some(detail) = self.state.settings_runtime_detail.as_ref() else {
            return false;
        };
        let focus = detail.focus;
        let Some(rt) = self.config.runtimes.get(&detail.name) else {
            return false;
        };
        let len = match focus {
            RuntimeDetailFocus::Models => rt.supported_models.len(),
            RuntimeDetailFocus::Environment => rt.env.len(),
        };
        if len == 0 {
            return true;
        }
        let Some(detail) = self.state.settings_runtime_detail.as_mut() else {
            return false;
        };
        let current = match focus {
            RuntimeDetailFocus::Models => detail.model_index,
            RuntimeDetailFocus::Environment => detail.env_index,
        };
        let next = if delta < 0 {
            current.checked_sub(1).unwrap_or(len - 1)
        } else {
            (current + 1) % len
        };
        match focus {
            RuntimeDetailFocus::Models => detail.model_index = next,
            RuntimeDetailFocus::Environment => detail.env_index = next,
        }
        true
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
                let rt = conductor_core::config::RuntimeConfig {
                    runtime_type: Some("claude".to_string()),
                    ..Default::default()
                };
                self.config.runtimes.insert(name.clone(), rt);
                self.save_config_background();
                self.refresh_settings_display();
                self.state.modal = Modal::None;
                self.enter_runtime_detail(&name);
                return;
            }
            InputAction::SettingsAddModel { runtime } => {
                let model = value.trim().to_string();
                if model.is_empty() {
                    self.state.modal = Modal::None;
                    return;
                }
                if let Some(rt) = self.config.runtimes.get_mut(&runtime) {
                    if rt.supported_models.iter().any(|m| m == &model) {
                        self.state.modal = Modal::Error {
                            message: format!("Model \"{model}\" already in this runtime."),
                        };
                        return;
                    }
                    rt.supported_models.push(model);
                    let new_index = rt.supported_models.len().saturating_sub(1);
                    self.save_config_background();
                    self.refresh_settings_display();
                    if let Some(detail) = self.state.settings_runtime_detail.as_mut() {
                        if detail.name == runtime {
                            detail.model_index = new_index;
                        }
                    }
                }
            }
            InputAction::SettingsEditModel { runtime, index } => {
                let model = value.trim().to_string();
                if model.is_empty() {
                    self.state.modal = Modal::None;
                    return;
                }
                if let Some(rt) = self.config.runtimes.get_mut(&runtime) {
                    if rt
                        .supported_models
                        .iter()
                        .enumerate()
                        .any(|(i, m)| i != index && m == &model)
                    {
                        self.state.modal = Modal::Error {
                            message: format!("Model \"{model}\" already in this runtime."),
                        };
                        return;
                    }
                    if let Some(slot) = rt.supported_models.get_mut(index) {
                        *slot = model;
                        self.save_config_background();
                        self.refresh_settings_display();
                    }
                }
            }
            InputAction::SettingsEditEnvValue { runtime, key } => {
                if let Some(rt) = self.config.runtimes.get_mut(&runtime) {
                    rt.env.insert(key, value);
                    self.save_config_background();
                    self.refresh_settings_display();
                }
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

    /// Open the Input modal to add a new runtime entry (step 1: name).
    pub(super) fn handle_runtimes_add(&mut self) {
        self.state.modal = Modal::Input {
            title: "Add runtime".into(),
            prompt: "Runtime name (e.g. claude-qwen-local):".into(),
            value: String::new(),
            on_submit: InputAction::SettingsAddRuntime,
        };
    }

    /// Drill into the currently selected runtime's detail view.
    pub(super) fn handle_runtimes_edit(&mut self) {
        let runtimes = &self.state.settings_display.runtimes;
        if runtimes.is_empty() {
            return;
        }
        let idx = self
            .state
            .settings_row_index
            .min(runtimes.len().saturating_sub(1));
        let name = runtimes[idx].name.clone();
        self.enter_runtime_detail(&name);
    }

    /// Set up the detail view for `name`. The runtime must already exist in
    /// `self.config.runtimes` (or be the implicit "claude" entry).
    pub(super) fn enter_runtime_detail(&mut self, name: &str) {
        self.state.settings_runtime_detail = Some(RuntimeDetailState {
            name: name.to_string(),
            ..Default::default()
        });
    }

    /// Exit the detail view and return to the runtimes list.
    pub(super) fn exit_runtime_detail(&mut self) {
        self.state.settings_runtime_detail = None;
    }

    /// Toggle Models ↔ Environment focus inside the detail view.
    pub(super) fn handle_runtime_detail_toggle_section(&mut self) {
        if let Some(detail) = self.state.settings_runtime_detail.as_mut() {
            detail.focus = detail.focus.next();
        }
    }

    /// Open Input modal to add a single model to the current runtime.
    pub(super) fn handle_runtime_detail_model_add(&mut self) {
        let Some(detail) = self.state.settings_runtime_detail.as_ref() else {
            return;
        };
        let runtime = detail.name.clone();
        self.state.modal = Modal::Input {
            title: format!("Add model — {runtime}"),
            prompt: "Model name:".into(),
            value: String::new(),
            on_submit: InputAction::SettingsAddModel { runtime },
        };
    }

    /// Open Input modal to edit the focused model.
    pub(super) fn handle_runtime_detail_model_edit(&mut self) {
        let Some(detail) = self.state.settings_runtime_detail.as_ref() else {
            return;
        };
        let runtime = detail.name.clone();
        let index = detail.model_index;
        let Some(rt) = self.config.runtimes.get(&runtime) else {
            return;
        };
        let Some(current) = rt.supported_models.get(index).cloned() else {
            return;
        };
        self.state.modal = Modal::Input {
            title: format!("Edit model — {runtime}"),
            prompt: "Model name:".into(),
            value: current,
            on_submit: InputAction::SettingsEditModel { runtime, index },
        };
    }

    /// Open Confirm modal to delete the focused model.
    pub(super) fn handle_runtime_detail_model_delete(&mut self) {
        let Some(detail) = self.state.settings_runtime_detail.as_ref() else {
            return;
        };
        let runtime = detail.name.clone();
        let index = detail.model_index;
        let Some(rt) = self.config.runtimes.get(&runtime) else {
            return;
        };
        let Some(model) = rt.supported_models.get(index).cloned() else {
            return;
        };
        self.state.modal = Modal::Confirm {
            title: "Delete model".into(),
            message: format!("Remove \"{model}\" from {runtime}?"),
            on_confirm: ConfirmAction::DeleteRuntimeModel { runtime, index },
        };
    }

    /// Move the focused model up one position. No-op if already at the top.
    pub(super) fn handle_runtime_detail_model_move_up(&mut self) {
        self.move_focused_model(-1);
    }

    /// Move the focused model down one position. No-op if already at the bottom.
    pub(super) fn handle_runtime_detail_model_move_down(&mut self) {
        self.move_focused_model(1);
    }

    /// Swap the focused model with its neighbor in `delta` direction (-1 = up,
    /// +1 = down). No-op when there is no detail view, no such runtime, or the
    /// resulting index would be out of bounds.
    fn move_focused_model(&mut self, delta: isize) {
        let Some(detail) = self.state.settings_runtime_detail.as_ref() else {
            return;
        };
        let index = detail.model_index;
        let Some(rt) = self.config.runtimes.get_mut(&detail.name) else {
            return;
        };
        let len = rt.supported_models.len();
        if index >= len {
            return;
        }
        let Some(target) = index.checked_add_signed(delta).filter(|&t| t < len) else {
            return;
        };
        rt.supported_models.swap(index, target);
        if let Some(detail) = self.state.settings_runtime_detail.as_mut() {
            detail.model_index = target;
        }
        self.save_config_background();
        self.refresh_settings_display();
    }

    /// Open Form modal to add a new env var (key + value).
    pub(super) fn handle_runtime_detail_env_add(&mut self) {
        let Some(detail) = self.state.settings_runtime_detail.as_ref() else {
            return;
        };
        let runtime = detail.name.clone();
        self.state.modal = Modal::Form {
            title: format!("Add env var — {runtime}"),
            fields: vec![
                FormField {
                    label: "Key".into(),
                    value: String::new(),
                    placeholder: "ANTHROPIC_BASE_URL".into(),
                    manually_edited: true,
                    required: true,
                    readonly: false,
                    field_type: FormFieldType::Text,
                },
                FormField {
                    label: "Value".into(),
                    value: String::new(),
                    placeholder: "https://example.com".into(),
                    manually_edited: true,
                    required: false,
                    readonly: false,
                    field_type: FormFieldType::Text,
                },
            ],
            active_field: 0,
            on_submit: FormAction::AddRuntimeEnvVar { runtime },
        };
    }

    /// Open Input modal to edit the focused env var's value.
    pub(super) fn handle_runtime_detail_env_edit(&mut self) {
        let Some((runtime, key)) = self.focused_env_key_pair() else {
            return;
        };
        let current = self
            .config
            .runtimes
            .get(&runtime)
            .and_then(|rt| rt.env.get(&key))
            .cloned()
            .unwrap_or_default();
        self.state.modal = Modal::Input {
            title: format!("Edit value — {key}"),
            prompt: format!("{key}:"),
            value: current,
            on_submit: InputAction::SettingsEditEnvValue { runtime, key },
        };
    }

    /// Open Confirm modal to delete the focused env var.
    pub(super) fn handle_runtime_detail_env_delete(&mut self) {
        let Some((runtime, key)) = self.focused_env_key_pair() else {
            return;
        };
        self.state.modal = Modal::Confirm {
            title: "Delete env var".into(),
            message: format!("Remove {key} from {runtime}?"),
            on_confirm: ConfirmAction::DeleteRuntimeEnvVar { runtime, key },
        };
    }

    /// Toggle reveal/mask for the focused env var.
    pub(super) fn handle_runtime_detail_env_toggle_reveal(&mut self) {
        let Some((_, key)) = self.focused_env_key_pair() else {
            return;
        };
        let Some(detail) = self.state.settings_runtime_detail.as_mut() else {
            return;
        };
        if !detail.revealed_env_keys.remove(&key) {
            detail.revealed_env_keys.insert(key);
        }
    }

    /// Apply a Form submission for env-var add. Validates the key, rejects
    /// duplicates, and inserts on success.
    pub(super) fn submit_add_runtime_env_var(&mut self, fields: Vec<FormField>, runtime: &str) {
        let key = fields
            .first()
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let value = fields.get(1).map(|f| f.value.clone()).unwrap_or_default();
        if key.is_empty() {
            self.state.modal = Modal::Error {
                message: "Env var key is required.".into(),
            };
            return;
        }
        if key.contains('=') || key.contains(' ') {
            self.state.modal = Modal::Error {
                message: "Env var key cannot contain '=' or whitespace.".into(),
            };
            return;
        }
        let Some(rt) = self.config.runtimes.get_mut(runtime) else {
            self.state.modal = Modal::Error {
                message: format!("Runtime \"{runtime}\" no longer exists."),
            };
            return;
        };
        if rt.env.contains_key(&key) {
            self.state.modal = Modal::Error {
                message: format!("Env var \"{key}\" already set — edit it instead."),
            };
            return;
        }
        rt.env.insert(key, value);
        self.save_config_background();
        self.refresh_settings_display();
        self.state.modal = Modal::None;
    }

    /// Returns `(runtime_name, env_key)` for the focused env row, or `None`
    /// when there are no env vars or no detail view is active.
    ///
    /// Sourced from `settings_display.runtimes` so the env ordering matches
    /// what the UI currently renders — `env_index` would otherwise depend on
    /// two independent sorts that could drift apart.
    fn focused_env_key_pair(&self) -> Option<(String, String)> {
        let detail = self.state.settings_runtime_detail.as_ref()?;
        let row = self
            .state
            .settings_display
            .runtimes
            .iter()
            .find(|r| r.name == detail.name)?;
        let key = row.env.get(detail.env_index)?.0.clone();
        Some((detail.name.clone(), key))
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
        let row = &runtimes[idx];
        if row.is_built_in {
            self.state.status_message = Some("Cannot delete built-in claude runtime".into());
            return;
        }
        let name = row.name.clone();
        self.state.modal = Modal::Confirm {
            title: "Delete runtime".into(),
            message: format!("Remove runtime \"{name}\" from config?"),
            on_confirm: ConfirmAction::DeleteRuntime { name },
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
