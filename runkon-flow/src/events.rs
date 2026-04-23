use std::time::{SystemTime, UNIX_EPOCH};

/// A single workflow engine event with timestamp and run identity.
#[derive(Debug, Clone)]
pub struct EngineEventData {
    /// Unix timestamp (seconds) when the event was emitted.
    pub timestamp: u64,
    /// The workflow run ID this event belongs to.
    pub run_id: String,
    /// The event payload.
    pub event: EngineEvent,
}

impl EngineEventData {
    pub fn new(run_id: String, event: EngineEvent) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            timestamp,
            run_id,
            event,
        }
    }
}

/// Workflow engine event variants emitted after each DB-write state transition.
///
/// Marked `#[non_exhaustive]` so downstream crates must handle an `_` arm —
/// future variants can be added without breaking existing sinks.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum EngineEvent {
    // Run lifecycle
    RunStarted {
        workflow_name: String,
    },
    RunCompleted {
        succeeded: bool,
    },
    RunResumed {
        workflow_name: String,
    },
    RunCancelled,
    // Step lifecycle
    StepStarted {
        step_name: String,
    },
    StepCompleted {
        step_name: String,
        succeeded: bool,
    },
    StepRetrying {
        step_name: String,
        attempt: u32,
    },
    // Gate
    GateWaiting {
        gate_name: String,
    },
    GateResolved {
        gate_name: String,
        approved: bool,
    },
    // Fan-out
    FanOutItemsCollected {
        count: usize,
    },
    FanOutItemStarted {
        item_id: String,
    },
    FanOutItemCompleted {
        item_id: String,
        succeeded: bool,
    },
    // Metrics
    MetricsUpdated {
        total_cost: f64,
        total_turns: i64,
        total_duration_ms: i64,
    },
}

/// Observability sink that receives engine events after each DB-write state transition.
///
/// # Contract
///
/// - **DB writes happen before emit**: subscribers never observe pre-persistence state.
/// - **Slow sinks block the engine**: sinks that need async offload must implement it
///   internally (e.g. send over a channel, not await a future).
/// - **Panics are caught**: the engine wraps each `emit` call in `catch_unwind` and
///   logs panics; they do not abort the run.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &EngineEventData);
}
