use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent, KeyEventKind};

use crate::action::Action;

/// Notification sent to wake the main loop.
enum Wake {
    Input,
    Background,
}

/// A sender that background workers use to push actions. Automatically
/// sends a wake notification so the main loop unblocks.
#[derive(Clone)]
pub struct BackgroundSender {
    action_tx: mpsc::Sender<Action>,
    wake_tx: mpsc::Sender<Wake>,
}

impl BackgroundSender {
    /// Send a background action and wake the main loop.
    /// Returns true if sent successfully, false if the channel is closed.
    pub fn send(&self, action: Action) -> bool {
        if self.action_tx.send(action).is_err() {
            return false;
        }
        let _ = self.wake_tx.send(Wake::Background);
        true
    }
}

/// The event loop with priority channels: input events are always processed
/// before background events, ensuring key presses are never blocked by
/// agent output, DB polls, or other background work.
///
/// Architecture:
///   - `input_rx`: key events from crossterm (high priority)
///   - `bg_rx`: background actions from agent threads, DB poller, ticks (low priority)
///   - `wake_rx`: notification channel to unblock the main loop
///
/// The main loop blocks on `wake_rx`, then drains `input_rx` first (always),
/// then `bg_rx`. This guarantees input is never starved by background work.
pub struct EventLoop {
    input_rx: mpsc::Receiver<KeyEvent>,
    bg_rx: mpsc::Receiver<Action>,
    bg_tx: BackgroundSender,
    wake_rx: mpsc::Receiver<Wake>,
}

impl EventLoop {
    /// Create a new event loop. `tick_rate` controls the tick interval.
    pub fn new(tick_rate: Duration) -> Self {
        let (input_tx, input_rx) = mpsc::channel();
        let (action_tx, bg_rx) = mpsc::channel();
        let (wake_tx, wake_rx) = mpsc::channel();

        let bg_tx = BackgroundSender {
            action_tx,
            wake_tx: wake_tx.clone(),
        };

        // Crossterm input reader thread — polls at 10ms for snappy key response
        let input_wake_tx = wake_tx.clone();
        let input_poll = Duration::from_millis(10);
        thread::spawn(move || loop {
            if event::poll(input_poll).unwrap_or(false) {
                loop {
                    match event::read() {
                        Ok(CrosstermEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                            if input_tx.send(key).is_err() {
                                return;
                            }
                            let _ = input_wake_tx.send(Wake::Input);
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    if !event::poll(Duration::ZERO).unwrap_or(false) {
                        break;
                    }
                }
            }
        });

        // Tick timer thread
        let tick_tx = bg_tx.clone();
        thread::spawn(move || loop {
            thread::sleep(tick_rate);
            if !tick_tx.send(Action::Tick) {
                break;
            }
        });

        Self {
            input_rx,
            bg_rx,
            bg_tx,
            wake_rx,
        }
    }

    /// Block until at least one event is available on any channel.
    pub fn wait(&self) {
        let _ = self.wake_rx.recv();
        // Drain additional wake notifications to avoid stale wakes.
        while self.wake_rx.try_recv().is_ok() {}
    }

    /// Drain all pending key events (high priority).
    pub fn drain_input(&self) -> Vec<KeyEvent> {
        let mut keys = Vec::new();
        while let Ok(key) = self.input_rx.try_recv() {
            keys.push(key);
        }
        keys
    }

    /// Drain all pending background actions (low priority).
    pub fn drain_background(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Ok(action) = self.bg_rx.try_recv() {
            actions.push(action);
        }
        actions
    }

    /// Get a background sender for worker threads.
    pub fn bg_sender(&self) -> BackgroundSender {
        self.bg_tx.clone()
    }
}
