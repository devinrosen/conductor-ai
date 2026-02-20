use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent};

/// Unified event type for the TUI main loop.
#[derive(Debug)]
pub enum Event {
    /// A keyboard event from crossterm.
    Key(KeyEvent),
    /// A periodic tick for UI refresh.
    Tick,
    /// A message from a background worker (carries an Action).
    Background(crate::action::Action),
}

/// The event loop: spawns a crossterm input reader thread, a tick timer,
/// and exposes a sender for background workers.
pub struct EventLoop {
    rx: mpsc::Receiver<Event>,
    bg_tx: mpsc::Sender<Event>,
}

impl EventLoop {
    /// Create a new event loop. `tick_rate` controls the tick interval.
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let bg_tx = tx.clone();

        // Crossterm input reader thread
        let input_tx = tx.clone();
        thread::spawn(move || loop {
            if event::poll(tick_rate).unwrap_or(false) {
                if let Ok(CrosstermEvent::Key(key)) = event::read() {
                    if input_tx.send(Event::Key(key)).is_err() {
                        break;
                    }
                }
            }
        });

        // Tick timer thread
        thread::spawn(move || loop {
            thread::sleep(tick_rate);
            if tx.send(Event::Tick).is_err() {
                break;
            }
        });

        Self { rx, bg_tx }
    }

    /// Get the next event, blocking until one is available.
    pub fn next(&self) -> Result<Event, mpsc::RecvError> {
        self.rx.recv()
    }

    /// Get a sender for background workers to push events.
    pub fn bg_sender(&self) -> mpsc::Sender<Event> {
        self.bg_tx.clone()
    }
}
