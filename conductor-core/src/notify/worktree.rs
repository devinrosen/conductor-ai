//! `worktree.created` and `worktree.removed`: the worktree's life, for `[[notify.hooks]]`.
//!
//! Unlike the other events these fire from short-lived CLI processes as well as the TUI and
//! web, so hooks run on threads that the caller waits for (bounded by the hooks' own
//! timeouts) instead of the detached threads [`HookRunner::fire`] spawns, which a CLI exit
//! would kill. A failing hook is logged and never propagated: removing or creating a
//! worktree never fails because of a hook.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use runkon_notify::{Event, HookRunner, Severity};

use crate::config::HookConfig;
use crate::worktree::WorktreeStatus;

use super::catalog::{WORKTREE_CREATED, WORKTREE_REMOVED};
use super::try_claim_notification;

/// `notification_log.event_type` for the once-per-worktree `worktree.removed` claim.
pub const WORKTREE_REMOVED_EVENT_TYPE: &str = "worktree_removed";

const DEFAULT_HOOK_TIMEOUT_MS: u64 = 10_000;
const WAIT_GRACE: Duration = Duration::from_secs(2);

/// What a worktree event describes.
#[derive(Debug, Clone, Copy)]
pub struct WorktreeEventArgs<'a> {
    pub repo_slug: &'a str,
    pub worktree_slug: &'a str,
    pub worktree_id: &'a str,
    pub branch: &'a str,
    pub path: &'a str,
    pub base_branch: Option<&'a str>,
    pub pr_number: Option<u64>,
}

/// Why a worktree was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeRemovedReason {
    Merged,
    Abandoned,
    /// A completed record purged without an earlier `worktree.removed`.
    Deleted,
}

impl WorktreeRemovedReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::Abandoned => "abandoned",
            Self::Deleted => "deleted",
        }
    }
}

impl From<&WorktreeStatus> for WorktreeRemovedReason {
    fn from(status: &WorktreeStatus) -> Self {
        match status {
            WorktreeStatus::Merged => Self::Merged,
            _ => Self::Abandoned,
        }
    }
}

fn base_fields(args: &WorktreeEventArgs<'_>) -> std::collections::HashMap<String, String> {
    [
        ("repo_slug".to_string(), args.repo_slug.to_string()),
        ("worktree_slug".into(), args.worktree_slug.to_string()),
        ("worktree_id".into(), args.worktree_id.to_string()),
        ("branch".into(), args.branch.to_string()),
        ("path".into(), args.path.to_string()),
        (
            "base_branch".into(),
            args.base_branch.unwrap_or("").to_string(),
        ),
        (
            "pr_number".into(),
            args.pr_number.map(|n| n.to_string()).unwrap_or_default(),
        ),
        ("timestamp".into(), chrono::Utc::now().to_rfc3339()),
    ]
    .into_iter()
    .collect()
}

pub(crate) fn worktree_created_event(args: &WorktreeEventArgs<'_>) -> Event {
    Event {
        kind: WORKTREE_CREATED.into(),
        title: "Conductor \u{2014} Worktree Created".into(),
        body: format!("{}/{}", args.repo_slug, args.worktree_slug),
        severity: Severity::Info,
        fields: base_fields(args),
    }
}

pub(crate) fn worktree_removed_event(
    args: &WorktreeEventArgs<'_>,
    reason: WorktreeRemovedReason,
) -> Event {
    let mut fields = base_fields(args);
    fields.insert("reason".into(), reason.as_str().into());
    Event {
        kind: WORKTREE_REMOVED.into(),
        title: "Conductor \u{2014} Worktree Removed".into(),
        body: format!(
            "{}/{} ({})",
            args.repo_slug,
            args.worktree_slug,
            reason.as_str()
        ),
        severity: Severity::Info,
        fields,
    }
}

/// Fire `worktree.created`. No-op when no hooks are configured.
pub fn fire_worktree_created(hooks: &[HookConfig], args: &WorktreeEventArgs<'_>) {
    if hooks.is_empty() {
        return;
    }
    dispatch_and_wait(hooks, &worktree_created_event(args));
}

/// Fire `worktree.removed` once per worktree ID.
///
/// No-op when no hooks are configured, and then nothing is claimed in `notification_log`,
/// so a hook added later can still be told about old records when they are purged.
pub fn fire_worktree_removed(
    conn: &rusqlite::Connection,
    hooks: &[HookConfig],
    args: &WorktreeEventArgs<'_>,
    reason: WorktreeRemovedReason,
) {
    if hooks.is_empty() {
        return;
    }
    match try_claim_notification(conn, args.worktree_id, WORKTREE_REMOVED_EVENT_TYPE) {
        Ok(false) => return,
        Ok(true) => {}
        Err(e) => {
            tracing::warn!(
                worktree_id = args.worktree_id,
                "worktree.removed dedup claim failed, firing anyway: {e}"
            );
        }
    }
    dispatch_and_wait(hooks, &worktree_removed_event(args, reason));
}

/// Run every hook matching `event` on its own thread and wait for them, up to the longest
/// `timeout_ms` plus a grace period. Failures and stragglers are logged.
fn dispatch_and_wait(hooks: &[HookConfig], event: &Event) {
    let (tx, rx) = mpsc::channel::<(usize, Result<(), String>)>();
    let mut max_timeout = DEFAULT_HOOK_TIMEOUT_MS;
    for (i, hook) in hooks.iter().enumerate() {
        max_timeout = max_timeout.max(hook.timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS));
        // One runner per hook: `run_test` stops after the first matching hook.
        let runner = HookRunner::new(&[hook.to_runkon_hook_config()]);
        let event = event.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((i, runner.run_test(&event)));
        });
    }
    drop(tx);

    let deadline = Instant::now() + Duration::from_millis(max_timeout) + WAIT_GRACE;
    let mut pending = hooks.len();
    while pending > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((i, result)) => {
                pending -= 1;
                if let Err(msg) = result {
                    tracing::warn!(
                        event = %event.kind,
                        on = %hooks[i].on,
                        error = %msg,
                        "worktree hook failed"
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    event = %event.kind,
                    still_running = pending,
                    "worktree hooks still running at the deadline; not waiting"
                );
                return;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}
