use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use std::time::Duration;

use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::{Peer, RoleServer};

/// How often subscribed runs are checked for a terminal status.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

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

    /// The run IDs that currently have subscribers.
    fn run_ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    #[cfg(test)]
    fn len_for(&self, run_id: &str) -> usize {
        self.lock().get(run_id).map(|v| v.len()).unwrap_or(0)
    }
}

/// Process-wide subscription hub: owns the subscriber registry and the task
/// that watches subscribed runs.
///
/// Runs execute in their own processes (#3124), so their engine events never
/// reach this server; the hub polls the database for their status instead. That
/// also covers runs started from the CLI or the TUI.
#[derive(Clone)]
pub struct SubscriptionHub {
    registry: SubscriptionRegistry,
}

impl SubscriptionHub {
    /// Construct a hub and spawn its poller task. The returned `JoinHandle`
    /// belongs to the holder, which aborts it when it no longer needs the hub.
    pub fn new() -> (Self, tokio::task::JoinHandle<()>) {
        let registry = SubscriptionRegistry::new();
        let handle = spawn_poller(registry.clone(), POLL_INTERVAL, terminal_lookup());
        (Self { registry }, handle)
    }

    /// Register a subscriber for `run_id`.
    pub fn subscribe(&self, run_id: String, peer: Peer<RoleServer>, uri: String) {
        self.registry.insert(run_id, peer, uri);
    }

    /// Remove a subscriber identified by `(run_id, uri)`.
    pub fn unsubscribe(&self, run_id: &str, uri: &str) {
        self.registry.remove(run_id, uri);
    }

    /// Drains all subscribers for `run_id` and fires a resource-updated notification on each.
    pub async fn notify_and_drain(&self, run_id: &str) {
        self.registry.notify_and_drain(run_id).await;
    }
}

/// Which of `run_ids` are finished, as far as the database says. A run that no
/// longer exists counts as finished so its subscribers aren't orphaned. Blocking.
type TerminalLookup = Arc<dyn Fn(Vec<String>) -> Vec<String> + Send + Sync>;

/// A lookup that opens the database on first use and reuses the connection on
/// later polls, instead of reloading config and reopening SQLite every tick. A
/// failed open is retried on the next poll.
fn terminal_lookup() -> TerminalLookup {
    let conductor: Arc<std::sync::Mutex<Option<conductor_core::Conductor>>> =
        Arc::new(std::sync::Mutex::new(None));
    Arc::new(move |run_ids| {
        let mut guard = conductor.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            match conductor_core::Conductor::open() {
                Ok(c) => *guard = Some(c),
                Err(e) => {
                    tracing::warn!("subscription poll could not open the database: {e}");
                    return Vec::new();
                }
            }
        }
        match guard.as_ref() {
            Some(c) => terminal_run_ids(&c.conn, run_ids),
            None => Vec::new(),
        }
    })
}

fn terminal_run_ids(conn: &rusqlite::Connection, run_ids: Vec<String>) -> Vec<String> {
    run_ids
        .into_iter()
        .filter(
            |id| match conductor_core::workflow::get_workflow_run_status(conn, id) {
                Ok(None) => true,
                Ok(status) => super::server::is_terminal(status.as_deref()),
                Err(e) => {
                    tracing::warn!("subscription poll failed for run {id}: {e}");
                    false
                }
            },
        )
        .collect()
}

/// One poll: look up the subscribed runs and notify and drain the finished ones.
/// Does no database work when nothing is subscribed.
async fn poll_once(registry: &SubscriptionRegistry, lookup: &TerminalLookup) {
    let run_ids = registry.run_ids();
    if run_ids.is_empty() {
        return;
    }
    let lookup = Arc::clone(lookup);
    let finished = match tokio::task::spawn_blocking(move || lookup(run_ids)).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("subscription poll task failed: {e}");
            return;
        }
    };
    for run_id in finished {
        registry.notify_and_drain(&run_id).await;
    }
}

/// Polls every `interval` until aborted. Dead peers are silently swept (send
/// errors are ignored).
fn spawn_poller(
    registry: SubscriptionRegistry,
    interval: Duration,
    lookup: TerminalLookup,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            poll_once(&registry, &lookup).await;
        }
    })
}

#[cfg(test)]
mod tests {
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

    fn occupy(reg: &SubscriptionRegistry, run_id: &str) {
        // Peer can't be built in a unit test, so an empty slot stands in for a subscriber.
        reg.lock().entry(run_id.to_string()).or_default();
    }

    /// A poll drains the runs the lookup reports finished and keeps the others.
    #[tokio::test]
    async fn test_poll_once_drains_only_finished_runs() {
        let reg = make_registry();
        occupy(&reg, "run-done");
        occupy(&reg, "run-live");
        let lookup: TerminalLookup =
            Arc::new(|ids| ids.into_iter().filter(|id| id == "run-done").collect());
        poll_once(&reg, &lookup).await;
        assert!(reg.lock().get("run-done").is_none());
        assert!(reg.lock().contains_key("run-live"));
    }

    /// With no subscribers a poll must not touch the database.
    #[tokio::test]
    async fn test_poll_once_skips_lookup_when_idle() {
        let reg = make_registry();
        let lookup: TerminalLookup =
            Arc::new(|_| panic!("lookup must not run with no subscribers"));
        poll_once(&reg, &lookup).await;
    }

    /// The poller task notices a run finishing without any engine event.
    #[tokio::test]
    async fn test_poller_drains_a_run_that_finishes() {
        let reg = make_registry();
        occupy(&reg, "run-later");
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let lookup: TerminalLookup = Arc::new(move |ids| {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                ids
            } else {
                Vec::new()
            }
        });
        let handle = spawn_poller(reg.clone(), Duration::from_millis(10), lookup);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(reg.lock().contains_key("run-later"));
        finished.store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(reg.lock().get("run-later").is_none());
        handle.abort();
    }
}
