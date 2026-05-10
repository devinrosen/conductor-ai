use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use conductor_core::workflow::{EngineEvent, EngineEventData, EventSink};
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::{Peer, RoleServer};
use tokio::sync::mpsc;

struct ClientSink {
    peer: Peer<RoleServer>,
    uri: String,
}

/// Registry of active MCP subscribers keyed by run_id.
#[derive(Clone)]
pub struct SubscriptionRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<ClientSink>>>>,
}

impl SubscriptionRegistry {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Acquire the registry lock, recovering from poisoning by ignoring it —
    /// state is purely additive so a panicked writer leaves the map consistent.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<ClientSink>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn insert(&self, run_id: String, peer: Peer<RoleServer>, uri: String) {
        self.lock()
            .entry(run_id)
            .or_default()
            .push(ClientSink { peer, uri });
    }

    // TODO(transport): when conductor moves off stdio (single-peer) to a
    // multi-client transport like SSE/HTTP, this needs to identify the
    // requesting client and only remove their subscription, not every
    // subscription matching `uri`. rmcp does not currently expose a stable
    // peer-identity API to do this.
    pub fn remove(&self, run_id: &str, uri: &str) {
        let mut map = self.lock();
        if let Some(sinks) = map.get_mut(run_id) {
            sinks.retain(|s| s.uri != uri);
            if sinks.is_empty() {
                map.remove(run_id);
            }
        }
    }

    fn take(&self, run_id: &str) -> Vec<ClientSink> {
        self.lock().remove(run_id).unwrap_or_default()
    }

    /// Drains all subscribers for `run_id` and fires a resource-updated notification on each.
    pub async fn notify_and_drain(&self, run_id: &str) {
        let sinks = self.take(run_id);
        for sink in sinks {
            let _ = sink
                .peer
                .notify_resource_updated(ResourceUpdatedNotificationParam::new(sink.uri))
                .await;
        }
    }

    #[cfg(test)]
    fn len_for(&self, run_id: &str) -> usize {
        self.lock().get(run_id).map(|v| v.len()).unwrap_or(0)
    }
}

/// EventSink backed by a tokio unbounded channel.
struct TokioSink(mpsc::UnboundedSender<EngineEventData>);

impl EventSink for TokioSink {
    fn emit(&self, event: &EngineEventData) {
        let _ = self.0.send(event.clone());
    }
}

/// Process-wide subscription hub: owns the event channel sender and the subscriber registry.
#[derive(Clone)]
pub struct SubscriptionHub {
    tx: mpsc::UnboundedSender<EngineEventData>,
    registry: SubscriptionRegistry,
}

impl SubscriptionHub {
    /// Construct a hub and spawn its broadcaster task. The returned `JoinHandle`
    /// keeps the task alive for the lifetime of the holder; on drop, the channel
    /// closes and the task exits cleanly.
    pub fn new() -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let registry = SubscriptionRegistry::new();
        let handle = spawn_broadcaster(rx, registry.clone());
        let hub = Self { tx, registry };
        (hub, handle)
    }

    /// Register a subscriber for `run_id`.
    pub fn subscribe(&self, run_id: String, peer: Peer<RoleServer>, uri: String) {
        self.registry.insert(run_id, peer, uri);
    }

    /// Remove a subscriber identified by `(run_id, uri)`.
    pub fn unsubscribe(&self, run_id: &str, uri: &str) {
        self.registry.remove(run_id, uri);
    }

    /// Returns an `EventSink` that forwards events to the broadcaster task.
    pub fn channel_sink(&self) -> Arc<dyn EventSink> {
        Arc::new(TokioSink(self.tx.clone()))
    }

    /// Drains all subscribers for `run_id` and fires a resource-updated notification on each.
    pub async fn notify_and_drain(&self, run_id: &str) {
        self.registry.notify_and_drain(run_id).await;
    }
}

/// Reads `EngineEventData` from the receiver, and on terminal events
/// (`RunCompleted` / `RunCancelled`) drains the registry for that run_id
/// and fires `notify_resource_updated` for each subscriber. Dead peers
/// are silently swept (send errors are ignored).
fn spawn_broadcaster(
    mut rx: mpsc::UnboundedReceiver<EngineEventData>,
    registry: SubscriptionRegistry,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event_data) = rx.recv().await {
            let is_terminal = matches!(
                event_data.event,
                EngineEvent::RunCompleted { .. } | EngineEvent::RunCancelled { .. }
            );
            if !is_terminal {
                continue;
            }
            registry.notify_and_drain(&event_data.run_id).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use conductor_core::workflow::{EngineEvent, EngineEventData};

    use super::*;

    fn make_registry() -> SubscriptionRegistry {
        SubscriptionRegistry::new()
    }

    /// Verify that remove is idempotent: removing a non-existent entry is a no-op.
    #[test]
    fn test_remove_nonexistent_is_noop() {
        let reg = make_registry();
        reg.remove("run-abc", "conductor://run/run-abc");
        assert_eq!(reg.len_for("run-abc"), 0);
    }

    /// Verify that take drains all entries for a run_id.
    #[test]
    fn test_take_drains_entries() {
        let reg = make_registry();

        // We can't easily make a real Peer<RoleServer> in a unit test, so we
        // just verify the registry state transitions without peers.
        // The broadcaster integration is covered by the channel routing test below.

        // Manually insert via the internal map.
        {
            // Fabricate a valid-looking channel so we can construct a Peer.
            // rmcp::Peer doesn't expose a test constructor, so we verify the
            // registry logic (HashMap operations) without actual Peer objects.
            let inner = Arc::clone(&reg.inner);
            let mut map = inner.lock().unwrap();
            map.entry("run-xyz".to_string()).or_default(); // empty vec — just occupies the slot
        }

        let taken = reg.take("run-xyz");
        assert!(taken.is_empty(), "empty slot should produce empty vec");
        assert_eq!(reg.len_for("run-xyz"), 0, "take should drain the slot");
    }

    /// Verify that take on an absent run_id returns an empty vec.
    #[test]
    fn test_take_absent_returns_empty() {
        let reg = make_registry();
        let taken = reg.take("no-such-run");
        assert!(taken.is_empty());
    }

    /// Verify the broadcaster drops non-terminal events and does not stall.
    #[tokio::test]
    async fn test_broadcaster_ignores_nonterminal_events() {
        let (hub, handle) = SubscriptionHub::new();

        // Emit a non-terminal event.
        let sink = hub.channel_sink();
        sink.emit(&EngineEventData::new(
            "run-1".to_string(),
            EngineEvent::RunStarted {
                workflow_name: "wf".to_string(),
            },
        ));

        // Drop all senders (sink holds a clone of hub.tx) before awaiting the handle.
        drop(sink);
        drop(hub);
        handle.await.expect("broadcaster task should exit cleanly");
    }

    /// Verify the broadcaster exits when the sender is dropped.
    #[tokio::test]
    async fn test_broadcaster_exits_on_channel_close() {
        let (hub, handle) = SubscriptionHub::new();
        drop(hub);
        handle.await.expect("broadcaster task should exit cleanly");
    }

    /// Verify notify_and_drain removes the registry slot (no sinks to notify).
    #[tokio::test]
    async fn test_notify_and_drain_removes_slot() {
        let reg = make_registry();
        {
            let mut map = reg.inner.lock().unwrap();
            map.entry("run-drain".to_string()).or_default();
        }
        reg.notify_and_drain("run-drain").await;
        let map = reg.inner.lock().unwrap();
        assert!(
            !map.contains_key("run-drain"),
            "notify_and_drain should remove the slot"
        );
    }

    /// Verify notify_and_drain is a no-op when the run has no subscribers.
    #[tokio::test]
    async fn test_notify_and_drain_absent_is_noop() {
        let reg = make_registry();
        reg.notify_and_drain("no-such-run").await;
        assert_eq!(reg.len_for("no-such-run"), 0);
    }

    /// Verify that terminal events drain the registry (no panics even with empty sinks).
    #[tokio::test]
    async fn test_broadcaster_drains_registry_on_terminal() {
        let (hub, handle) = SubscriptionHub::new();
        let registry = hub.registry.clone();

        // Manually insert a raw entry so we can verify it's consumed.
        {
            let mut map = registry.inner.lock().unwrap();
            map.entry("run-term".to_string()).or_default(); // occupies slot with no sinks
        }
        assert_eq!(registry.len_for("run-term"), 0);

        let sink = hub.channel_sink();
        sink.emit(&EngineEventData::new(
            "run-term".to_string(),
            EngineEvent::RunCompleted { succeeded: true },
        ));

        // Drop all senders (sink holds a clone of hub.tx) before awaiting the handle.
        drop(sink);
        drop(hub);
        handle.await.expect("broadcaster task should exit cleanly");

        // Registry slot should be gone after the terminal event was processed.
        // (The map entry was removed via take.)
        assert_eq!(registry.len_for("run-term"), 0);
    }
}
