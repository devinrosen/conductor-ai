use serde::Serialize;
use tokio::sync::broadcast;

/// Event types that flow through SSE to connected browsers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum ConductorEvent {
    #[serde(rename = "repo_created")]
    RepoCreated { id: String },
    #[serde(rename = "repo_deleted")]
    RepoDeleted { id: String },
    #[serde(rename = "worktree_created")]
    WorktreeCreated { id: String, repo_id: String },
    #[serde(rename = "worktree_deleted")]
    WorktreeDeleted { id: String, repo_id: String },
    #[serde(rename = "tickets_synced")]
    TicketsSynced { repo_id: String },
    #[serde(rename = "session_started")]
    SessionStarted { id: String },
    #[serde(rename = "session_ended")]
    SessionEnded { id: String },
}

impl ConductorEvent {
    /// The SSE event name used as the `event:` field.
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::RepoCreated { .. } => "repo_created",
            Self::RepoDeleted { .. } => "repo_deleted",
            Self::WorktreeCreated { .. } => "worktree_created",
            Self::WorktreeDeleted { .. } => "worktree_deleted",
            Self::TicketsSynced { .. } => "tickets_synced",
            Self::SessionStarted { .. } => "session_started",
            Self::SessionEnded { .. } => "session_ended",
        }
    }
}

/// Fan-out event bus built on `tokio::sync::broadcast`.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ConductorEvent>,
}

impl EventBus {
    /// Create a new EventBus with the given channel buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Emit an event to all subscribers. Silently ignores "no receivers" errors
    /// (which occur when no SSE clients are connected).
    pub fn emit(&self, event: ConductorEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<ConductorEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_with_no_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        bus.emit(ConductorEvent::RepoCreated { id: "test".into() });
    }

    #[tokio::test]
    async fn subscriber_receives_events() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.emit(ConductorEvent::RepoCreated { id: "abc".into() });
        let event = rx.recv().await.unwrap();
        assert_eq!(event.event_name(), "repo_created");
    }

    #[test]
    fn event_serializes_to_expected_json() {
        let event = ConductorEvent::WorktreeCreated {
            id: "wt1".into(),
            repo_id: "r1".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"worktree_created\""));
        assert!(json.contains("\"id\":\"wt1\""));
        assert!(json.contains("\"repo_id\":\"r1\""));
    }

    #[test]
    fn event_name_matches_all_variants() {
        let cases: Vec<(ConductorEvent, &str)> = vec![
            (
                ConductorEvent::RepoCreated { id: "".into() },
                "repo_created",
            ),
            (
                ConductorEvent::RepoDeleted { id: "".into() },
                "repo_deleted",
            ),
            (
                ConductorEvent::WorktreeCreated {
                    id: "".into(),
                    repo_id: "".into(),
                },
                "worktree_created",
            ),
            (
                ConductorEvent::WorktreeDeleted {
                    id: "".into(),
                    repo_id: "".into(),
                },
                "worktree_deleted",
            ),
            (
                ConductorEvent::TicketsSynced { repo_id: "".into() },
                "tickets_synced",
            ),
            (
                ConductorEvent::SessionStarted { id: "".into() },
                "session_started",
            ),
            (
                ConductorEvent::SessionEnded { id: "".into() },
                "session_ended",
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.event_name(), expected);
        }
    }
}
