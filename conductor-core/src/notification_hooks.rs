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
pub fn glob_matches(pattern: &str, event_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return event_name.starts_with(&format!("{prefix}."));
    }
    pattern == event_name
}

/// Returns `true` if `event` passes all optional filter fields on `hook`.
///
/// - `threshold_multiple`: for `cost_spike` / `duration_spike` events, the event's
///   `multiple` must be >= the configured minimum; other events pass through.
/// - `gate_pending_ms`: for `gate.pending_too_long` events, the event's `pending_ms`
///   must be >= the configured minimum; other events pass through.
/// - `workflow`: the event's label must start with the configured workflow name.
fn hook_event_passes_filters(hook: &HookConfig, event: &NotificationEvent) -> bool {
    if let Some(min_multiple) = hook.threshold_multiple {
        match event {
            NotificationEvent::WorkflowRunCostSpike { multiple, .. }
            | NotificationEvent::WorkflowRunDurationSpike { multiple, .. } => {
                if *multiple < min_multiple {
                    return false;
                }
            }
            _ => {}
        }
    }

    if let Some(min_pending_ms) = hook.gate_pending_ms {
        if let NotificationEvent::GatePendingTooLong { pending_ms, .. } = event {
            if *pending_ms < min_pending_ms {
                return false;
            }
        }
    }

    if let Some(ref wf_filter) = hook.workflow {
        if !event.label().starts_with(wf_filter.as_str()) {
            return false;
        }
    }

    if hook.root_workflows_only == Some(true) {
        match event {
            NotificationEvent::WorkflowRunCompleted {
                parent_workflow_run_id,
                ..
            }
            | NotificationEvent::WorkflowRunFailed {
                parent_workflow_run_id,
                ..
            }
            | NotificationEvent::WorkflowRunCostSpike {
                parent_workflow_run_id,
                ..
            }
            | NotificationEvent::WorkflowRunDurationSpike {
                parent_workflow_run_id,
                ..
            } => {
                if parent_workflow_run_id.is_some() {
                    return false;
                }
            }
            _ => {} // non-workflow events pass through
        }
    }

    true
}

/// Execute a shell hook for `event`, enforcing the configured timeout.
///
/// The command is run via `sh -c` with all `CONDUCTOR_*` env vars injected.
/// If the process does not finish within `timeout_ms`, it is killed via
/// `Child::kill()` and a warning is logged. All failures are non-fatal.
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

    // Poll with try_wait so we retain ownership of `child` and can kill it on timeout.
    let timeout = Duration::from_millis(timeout_ms);
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    tracing::warn!(cmd = %cmd, "shell hook exited with non-zero status: {status}");
                }
                return;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    tracing::warn!(cmd = %cmd, timeout_ms, "shell hook timed out");
                    let _ = child.kill();
                    let _ = child.wait(); // reap zombie
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!(cmd = %cmd, "shell hook wait error: {e}");
                let _ = child.kill();
                let _ = child.wait(); // reap zombie
                return;
            }
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
///
/// Returns the resolved value, or the original `$VAR_NAME` string if the variable
/// is not set (and logs a warning so silent authentication failures are visible).
fn resolve_env_var(s: &str) -> String {
    if let Some(var_name) = s.strip_prefix('$') {
        match std::env::var(var_name) {
            Ok(val) => val,
            Err(_) => {
                tracing::warn!(
                    var = %var_name,
                    "HTTP hook header references unset env var ${var_name}; \
                     passing literal string — authentication may fail"
                );
                s.to_string()
            }
        }
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

    /// Fire all hooks whose `on` pattern matches `event.event_name()` and whose
    /// optional filter fields (`threshold_multiple`, `gate_pending_ms`, `workflow`)
    /// are satisfied by the event.
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
            if !hook_event_passes_filters(hook, event) {
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

    // ── hook_event_passes_filters ────────────────────────────────────────

    #[test]
    fn filter_threshold_multiple_blocks_below_minimum() {
        let hook = HookConfig {
            on: "workflow_run.*".into(),
            threshold_multiple: Some(3.0),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCostSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 2.0, // below 3.0 threshold
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_threshold_multiple_passes_at_or_above_minimum() {
        let hook = HookConfig {
            on: "workflow_run.*".into(),
            threshold_multiple: Some(3.0),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCostSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 3.5, // above 3.0 threshold
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_threshold_multiple_ignored_for_non_spike_events() {
        let hook = HookConfig {
            on: "*".into(),
            threshold_multiple: Some(5.0),
            ..Default::default()
        };
        // Non-spike events are not filtered by threshold_multiple
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_gate_pending_ms_blocks_below_minimum() {
        let hook = HookConfig {
            on: "gate.*".into(),
            gate_pending_ms: Some(60_000),
            ..Default::default()
        };
        let event = NotificationEvent::GatePendingTooLong {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "s".into(),
            pending_ms: 30_000, // below 60_000 threshold
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_gate_pending_ms_passes_at_or_above_minimum() {
        let hook = HookConfig {
            on: "gate.*".into(),
            gate_pending_ms: Some(60_000),
            ..Default::default()
        };
        let event = NotificationEvent::GatePendingTooLong {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "s".into(),
            pending_ms: 120_000, // above threshold
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_workflow_name_blocks_non_matching_label() {
        let hook = HookConfig {
            on: "*".into(),
            workflow: Some("deploy".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "ticket-to-pr on main".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "ticket-to-pr".into(),
            parent_workflow_run_id: None,
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_workflow_name_passes_matching_label() {
        let hook = HookConfig {
            on: "*".into(),
            workflow: Some("deploy".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "deploy on main".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "deploy".into(),
            parent_workflow_run_id: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
        };
        // Non-matching: should return immediately without spawning.
        runner.fire(&event);
    }

    #[test]
    fn hook_runner_filtered_hook_not_fired() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("filtered.txt");
        let out_path = out_file.to_str().unwrap().to_string();

        // Hook matches the event name but threshold_multiple filter blocks it
        let hook = HookConfig {
            on: "workflow_run.*".into(),
            run: Some(format!("echo fired > '{out_path}'")),
            threshold_multiple: Some(5.0),
            timeout_ms: Some(3_000),
            ..Default::default()
        };
        let runner = HookRunner::new(&[hook]);
        let event = NotificationEvent::WorkflowRunCostSpike {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            multiple: 2.0, // below threshold
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
        };
        runner.fire(&event);
        std::thread::sleep(std::time::Duration::from_millis(300));
        // File should NOT have been created because the filter blocked it
        assert!(!out_file.exists());
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
            workflow_name: "my-wf".into(),
            parent_workflow_run_id: None,
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
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
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

    // ── root_workflows_only filter ───────────────────────────────────────

    #[test]
    fn filter_root_workflows_only_blocks_sub_workflow() {
        let hook = HookConfig {
            on: "workflow_run.*".into(),
            root_workflows_only: Some(true),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "child-wf".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "child-wf".into(),
            parent_workflow_run_id: Some("parent-run-id".into()),
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_root_workflows_only_passes_root_workflow() {
        let hook = HookConfig {
            on: "workflow_run.*".into(),
            root_workflows_only: Some(true),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "root-wf".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "root-wf".into(),
            parent_workflow_run_id: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_root_workflows_only_none_passes_all() {
        let hook = HookConfig {
            on: "workflow_run.*".into(),
            root_workflows_only: None,
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunFailed {
            run_id: "r".into(),
            label: "child-wf".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "child-wf".into(),
            parent_workflow_run_id: Some("parent-run-id".into()),
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }
}
