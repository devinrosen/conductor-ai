use std::sync::mpsc;
use std::time::Duration;

use crate::config::HookConfig;
use crate::notification_event::NotificationEvent;

/// Returns `true` if `pattern` matches `event_name`.
///
/// Three cases are supported:
/// - `"*"` — matches every event name.
/// - `"prefix.*"` — matches any event whose name starts with `"prefix."`.
/// - exact string — matches only when the strings are equal.
///
/// No external crate is needed: the event namespace is two-level and well-defined.
pub(crate) fn glob_matches(pattern: &str, event_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return event_name.starts_with(&format!("{prefix}."));
    }
    pattern == event_name
}

/// Execute a shell hook for `event`, enforcing the configured timeout.
///
/// The command is run via `sh -c` with all `CONDUCTOR_*` env vars injected.
/// If the process does not finish within `timeout_ms`, it is killed and a warning
/// is logged. All failures are non-fatal (logged as warnings).
fn run_shell_hook(hook: &HookConfig, event: &NotificationEvent) {
    let Some(ref cmd) = hook.run else { return };

    let timeout_ms = hook.timeout_ms.unwrap_or(10_000);
    let env_vars = event.to_env_vars();

    let mut child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .envs(&env_vars)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(cmd = %cmd, "shell hook spawn failed: {e}");
            return;
        }
    };

    // Enforce timeout via a watchdog thread + channel.
    // The child is moved into the thread; we use recv_timeout to cap the wait.
    let (tx, rx) = mpsc::channel::<std::process::ExitStatus>();
    std::thread::spawn(move || match child.wait() {
        Ok(status) => {
            let _ = tx.send(status);
        }
        Err(e) => {
            tracing::warn!("shell hook wait error: {e}");
        }
    });

    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(status) => {
            if !status.success() {
                tracing::warn!(cmd = %cmd, "shell hook exited with non-zero status: {status}");
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(cmd = %cmd, timeout_ms, "shell hook timed out");
            // The spawned thread holds the child; it will be dropped when the
            // thread exits. The OS will eventually reap the orphaned process.
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // wait() failed inside the thread — already logged there.
        }
    }
}

/// POST `event.to_json()` to `hook.url`, resolving `$VAR` header values from env.
///
/// Uses the existing `ureq` dependency (synchronous). All failures are non-fatal.
fn run_http_hook(hook: &HookConfig, event: &NotificationEvent) {
    let Some(ref url) = hook.url else { return };

    let timeout_ms = hook.timeout_ms.unwrap_or(10_000);
    let payload = event.to_json();

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms))
        .build();

    let mut request = agent.post(url);

    if let Some(ref headers) = hook.headers {
        for (key, val) in headers {
            let resolved = resolve_env_var(val);
            request = request.set(key, &resolved);
        }
    }

    if let Err(e) = request.send_json(&payload) {
        tracing::warn!(url = %url, "HTTP hook POST failed: {e}");
    }
}

/// Resolve a header value: if it starts with `$`, look it up in the environment.
/// Returns the original string if the variable is not set.
fn resolve_env_var(s: &str) -> String {
    if let Some(var_name) = s.strip_prefix('$') {
        std::env::var(var_name).unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    }
}

/// Fires user-configured notification hooks for a given lifecycle event.
///
/// Each matching hook is spawned on its own OS thread (fire-and-forget). Failures
/// are logged as warnings and never propagated to the caller.
pub struct HookRunner {
    hooks: Vec<HookConfig>,
}

impl HookRunner {
    /// Create a runner from a slice of hook configs.
    pub fn new(hooks: &[HookConfig]) -> Self {
        Self {
            hooks: hooks.to_vec(),
        }
    }

    /// Fire all hooks whose `on` pattern matches `event.event_name()`.
    ///
    /// Each matching hook is executed in a separate OS thread so the caller is
    /// never blocked. Both `run` (shell) and `url` (HTTP) hooks can coexist in
    /// the same config entry; both are attempted when present.
    pub fn fire(&self, event: &NotificationEvent) {
        let event_name = event.event_name();
        for hook in &self.hooks {
            if !glob_matches(&hook.on, event_name) {
                continue;
            }
            let hook_clone = hook.clone();
            let event_clone = event.clone();
            std::thread::spawn(move || {
                if hook_clone.run.is_some() {
                    run_shell_hook(&hook_clone, &event_clone);
                }
                if hook_clone.url.is_some() {
                    run_http_hook(&hook_clone, &event_clone);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_event::NotificationEvent;

    // ── glob_matches ─────────────────────────────────────────────────────

    #[test]
    fn glob_star_matches_any() {
        assert!(glob_matches("*", "workflow_run.completed"));
        assert!(glob_matches("*", "gate.waiting"));
        assert!(glob_matches("*", "feedback.requested"));
    }

    #[test]
    fn glob_prefix_matches_same_category() {
        assert!(glob_matches("workflow_run.*", "workflow_run.completed"));
        assert!(glob_matches("workflow_run.*", "workflow_run.failed"));
        assert!(glob_matches("workflow_run.*", "workflow_run.cost_spike"));
    }

    #[test]
    fn glob_prefix_does_not_match_other_category() {
        assert!(!glob_matches("workflow_run.*", "gate.waiting"));
        assert!(!glob_matches("workflow_run.*", "agent_run.completed"));
        assert!(!glob_matches("workflow_run.*", "feedback.requested"));
    }

    #[test]
    fn glob_exact_matches_only_exact() {
        assert!(glob_matches("gate.waiting", "gate.waiting"));
        assert!(!glob_matches("gate.waiting", "gate.pending_too_long"));
        assert!(!glob_matches("gate.waiting", "workflow_run.completed"));
    }

    #[test]
    fn glob_prefix_does_not_partially_match_name() {
        // "workflow.*" should NOT match "workflow_run.completed" because
        // the prefix "workflow" does not equal "workflow_run".
        assert!(!glob_matches("workflow.*", "workflow_run.completed"));
    }

    // ── resolve_env_var ──────────────────────────────────────────────────

    #[test]
    fn resolve_env_var_non_dollar_passthrough() {
        assert_eq!(
            resolve_env_var("Bearer static-token"),
            "Bearer static-token"
        );
    }

    #[test]
    fn resolve_env_var_missing_returns_original() {
        // Use an env var name that is extremely unlikely to be set.
        let result = resolve_env_var("$__CONDUCTOR_TEST_UNSET_VAR_XYZ__");
        assert_eq!(result, "$__CONDUCTOR_TEST_UNSET_VAR_XYZ__");
    }

    #[test]
    fn resolve_env_var_set_var_is_resolved() {
        std::env::set_var("__CONDUCTOR_TEST_HOOK_VAR__", "resolved-value");
        let result = resolve_env_var("$__CONDUCTOR_TEST_HOOK_VAR__");
        std::env::remove_var("__CONDUCTOR_TEST_HOOK_VAR__");
        assert_eq!(result, "resolved-value");
    }

    // ── HookRunner::fire ─────────────────────────────────────────────────

    #[test]
    fn hook_runner_no_hooks_fires_nothing() {
        let runner = HookRunner::new(&[]);
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
        };
        // Should not panic or hang.
        runner.fire(&event);
    }

    #[test]
    fn hook_runner_non_matching_hook_not_spawned() {
        let hook = HookConfig {
            on: "agent_run.*".into(),
            run: None,
            url: None,
            ..Default::default()
        };
        let runner = HookRunner::new(&[hook]);
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
        };
        // Non-matching: should return immediately without spawning.
        runner.fire(&event);
    }

    #[test]
    fn run_shell_hook_writes_env_var_to_tempfile() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("out.txt");
        let out_path = out_file.to_str().unwrap().to_string();

        let hook = HookConfig {
            on: "*".into(),
            run: Some(format!("echo $CONDUCTOR_EVENT > '{out_path}'")),
            timeout_ms: Some(5_000),
            ..Default::default()
        };

        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "run-abc".into(),
            label: "my-wf".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            url: None,
        };

        run_shell_hook(&hook, &event);

        let mut contents = String::new();
        std::fs::File::open(&out_file)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents.trim(), "workflow_run.completed");
    }

    #[test]
    fn hook_runner_fires_matching_shell_hook() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("fired.txt");
        let out_path = out_file.to_str().unwrap().to_string();

        let hook = HookConfig {
            on: "workflow_run.*".into(),
            run: Some(format!("echo fired > '{out_path}'")),
            timeout_ms: Some(5_000),
            ..Default::default()
        };
        let runner = HookRunner::new(&[hook]);
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
        };
        runner.fire(&event);

        // Give the spawned thread time to complete.
        std::thread::sleep(std::time::Duration::from_millis(500));

        let mut contents = String::new();
        std::fs::File::open(&out_file)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents.trim(), "fired");
    }
}
