use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use rusqlite::Connection;

use conductor_core::config::Config;

use crate::action::Action;
use crate::background;
use crate::event::{BackgroundSender, EventLoop};
use crate::input;
use crate::state::AppState;
use crate::theme::Theme;
use crate::ui;

mod action_dispatch;
mod agent_events;
mod agent_execution;
mod crud_operations;
mod data_refresh;
mod git_operations;
mod github_discovery;
mod helpers;
mod info_pane;
mod input_handling;
mod modal_dialog;
mod navigation;
mod settings_management;
mod theme_management;
mod url_operations;
mod workflow_management;

#[cfg(test)]
mod tests;

pub struct App {
    state: AppState,
    conn: Connection,
    config: Config,
    bg_tx: Option<BackgroundSender>,
    /// Guard to prevent multiple concurrent workflow poll threads.
    workflow_poll_in_flight: Arc<AtomicBool>,
    /// Background workflow execution thread handles.
    workflow_threads: Vec<std::thread::JoinHandle<()>>,
    /// Shutdown signal sent to workflow executor threads on TUI exit.
    workflow_shutdown: Arc<AtomicBool>,
}

impl App {
    pub fn new(conn: Connection, config: Config, theme: Theme) -> Self {
        let mut state = AppState::new();
        state.theme = theme;
        Self {
            state,
            conn,
            config,
            bg_tx: None,
            workflow_poll_in_flight: Arc::new(AtomicBool::new(false)),
            workflow_threads: Vec::new(),
            workflow_shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Main run loop.
    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Initial data load
        self.refresh_data();

        let events = EventLoop::new(Duration::from_millis(200));

        // Spawn background workers
        let bg_tx = events.bg_sender();
        self.bg_tx = Some(bg_tx.clone());
        background::spawn_db_poller(bg_tx.clone(), Duration::from_secs(5));
        let sync_mins = self.config.general.sync_interval_minutes as u64;
        background::spawn_ticket_sync(bg_tx, Duration::from_secs(sync_mins * 60));

        let mut dirty = true; // tracks whether state changed since last draw

        loop {
            // Only redraw when state has actually changed.
            if dirty {
                terminal.draw(|frame| ui::render(frame, &self.state))?;
                dirty = false;
            }

            // Block until at least one event is available
            events.wait();

            // PRIORITY 1: Drain all key events first — input is never starved
            for key in events.drain_input() {
                let action = input::map_key(key, &self.state);
                dirty |= self.update(action);
            }

            // PRIORITY 2: Drain all background events
            let bg_actions = events.drain_background();
            for action in bg_actions {
                dirty |= self.update(action);
            }

            if self.state.should_quit {
                break;
            }
        }

        // Signal all workflow executor threads to stop, then join them with a
        // 10-second bounded timeout. Threads that don't finish in time are
        // abandoned; the startup recovery path will reconcile their steps.
        self.workflow_shutdown.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        for handle in self.workflow_threads.drain(..) {
            loop {
                if handle.is_finished() {
                    let _ = handle.join();
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        Ok(())
    }

    /// Handle an action by mutating state. Returns true if the UI needs a redraw.
    ///
    /// This thin wrapper delegates to `handle_action` and updates
    /// `status_message_at` whenever the status message presence changes.
    pub(crate) fn update(&mut self, action: Action) -> bool {
        let had_message = self.state.status_message.is_some();
        let dirty = self.handle_action(action);
        self.state.track_status_message_change(had_message);
        dirty
    }
}
