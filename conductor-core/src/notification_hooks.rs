use std::process::Stdio;
use std::time::Duration;

use crate::config::HookConfig;
use crate::notification_event::NotificationEvent;

/// Result of matching an `on` pattern against an event name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnMatch {
    /// No sub-pattern matched.
    None,
    /// Matched; fires for any workflow (root or child).
    Any,
    /// Matched via a `:root` suffix; fires only for root workflows.
    RootOnly,
}

/// Match a comma-separated `on` pattern against `event_name`.
///
/// Each sub-pattern may have a `:root` suffix (e.g. `"workflow_run.completed:root"`).
/// Returns [`OnMatch::RootOnly`] if the matching sub-pattern had `:root`,
/// [`OnMatch::Any`] if it matched without `:root`, or [`OnMatch::None`] if no match.
pub(crate) fn on_pattern_match(on: &str, event_name: &str) -> OnMatch {
    for part in on.split(',') {
        let part = part.trim();
        if let Some(pat) = part.strip_suffix(":root") {
            if glob_matches(pat, event_name) {
                return OnMatch::RootOnly;
            }
        } else if glob_matches(part, event_name) {
            return OnMatch::Any;
        }
    }
    OnMatch::None
}

/// Convenience wrapper: returns `true` if `on` matches `event_name` (ignoring `:root`).
pub fn on_pattern_matches(on: &str, event_name: &str) -> bool {
    on_pattern_match(on, event_name) != OnMatch::None
}

/// Returns `true` if `pattern` matches `event_name` or `value`.
///
/// Supported cases:
/// - `"*"` — matches everything.
/// - `"prefix.*"` — matches any string that starts with `"prefix."`.
/// - `"prefix/*"` — matches any string that starts with `"prefix/"` (for branch globs like `"feature/*"`).
/// - exact string — matches only when the strings are equal.
pub(crate) fn glob_matches(pattern: &str, event_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return event_name.starts_with(&format!("{prefix}."));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return event_name.starts_with(&format!("{prefix}/"));
    }
    pattern == event_name
}

/// Returns `true` if `event` passes all optional filter fields on `hook`.
///
/// - `threshold_multiple`: for `cost_spike` / `duration_spike` events, the event's
///   `multiple` must be >= the configured minimum; other events pass through.
/// - `gate_pending_ms`: for `gate.pending_too_long` events, the event's `pending_ms`
///   must be >= the configured minimum; other events pass through.
/// - `workflow`: the event's `workflow_name` field must equal the configured workflow name
///   (for workflow events only; non-workflow events pass through).
/// - `repo`: the event's `repo_slug` must equal the configured repo (exact match).
/// - `branch`: the event's `branch` must match the configured glob pattern.
/// - `step`: for `GateWaiting`/`GatePendingTooLong`, the event's `step_name` must equal
///   the configured step (exact match); non-gate events pass through.
fn hook_event_passes_filters(hook: &HookConfig, event: &NotificationEvent) -> bool {
    if let Some(min_multiple) = hook.threshold_multiple {
        match event {
            NotificationEvent::WorkflowRunCostSpike { multiple, .. }
            | NotificationEvent::WorkflowRunDurationSpike { multiple, .. }
                if *multiple < min_multiple =>
            {
                return false;
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
        // Match workflow_name directly for workflow events; non-workflow events pass through.
        match event {
            NotificationEvent::WorkflowRunCompleted { workflow_name, .. }
            | NotificationEvent::WorkflowRunFailed { workflow_name, .. }
            | NotificationEvent::WorkflowRunCostSpike { workflow_name, .. }
            | NotificationEvent::WorkflowRunDurationSpike { workflow_name, .. }
                if workflow_name != wf_filter =>
            {
                return false;
            }
            _ => {} // non-workflow events pass through
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
            } if parent_workflow_run_id.is_some() => {
                return false;
            }
            _ => {} // non-workflow events pass through
        }
    }

    if let Some(ref repo_filter) = hook.repo {
        if event.repo_slug() != repo_filter.as_str() {
            return false;
        }
    }

    if let Some(ref branch_filter) = hook.branch {
        if !glob_matches(branch_filter.as_str(), event.branch()) {
            return false;
        }
    }

    if let Some(ref step_filter) = hook.step {
        match event {
            NotificationEvent::GateWaiting { step_name, .. }
            | NotificationEvent::GatePendingTooLong { step_name, .. }
                if step_name != step_filter =>
            {
                return false;
            }
            _ => {} // non-gate events pass through
        }
    }

    true
}

/// Returns `true` if the event is a root workflow event (no parent), or is
/// not a workflow event at all (non-workflow events always pass through).
fn event_is_root_workflow(event: &NotificationEvent) -> bool {
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
        } => parent_workflow_run_id.is_none(),
        // Non-workflow events are always considered "root" (no parent concept).
        _ => true,
    }
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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

/// Execute a shell hook synchronously and capture its output, for test/diagnostic use.
///
/// Unlike `run_shell_hook`, this function:
/// - Pipes stdout and stderr so they do not leak into the caller's terminal.
/// - Blocks until the child exits (no timeout polling; uses `wait_with_output()`).
/// - Returns `Err(stderr_text)` on non-zero exit, falling back to `"exited with status N"`
///   if stderr is empty.
fn run_shell_hook_capture(hook: &HookConfig, event: &NotificationEvent) -> Result<(), String> {
    let Some(ref cmd) = hook.run else {
        return Ok(());
    };

    let env_vars = event.to_env_vars();

    let child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .envs(&env_vars)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("spawn failed: {e}")),
    };

    match child.wait_with_output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                Err(format!("exited with status {}", output.status))
            } else {
                Err(stderr)
            }
        }
        Err(e) => Err(format!("wait error: {e}")),
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

    /// Run the first matching hook synchronously and return the real exit result.
    ///
    /// Intended for the TUI "Test Hook" feature. Unlike `fire()`, this method:
    /// - Blocks until the hook finishes (no background thread).
    /// - Captures stdout/stderr so nothing leaks into the terminal.
    /// - Returns `Ok(())` on success or `Err(message)` on failure.
    ///
    /// Only shell (`run`) hooks return a meaningful result. HTTP hooks return `Ok(())`
    /// for now — they already swallow errors internally.
    /// TODO: surface HTTP hook errors in run_test when needed.
    pub fn run_test(&self, event: &NotificationEvent) -> Result<(), String> {
        let event_name = event.event_name();
        for hook in &self.hooks {
            let m = on_pattern_match(&hook.on, event_name);
            if m == OnMatch::None {
                continue;
            }
            if m == OnMatch::RootOnly && !event_is_root_workflow(event) {
                continue;
            }
            if !hook_event_passes_filters(hook, event) {
                continue;
            }
            if hook.run.is_some() {
                return run_shell_hook_capture(hook, event);
            }
            if hook.url.is_some() {
                run_http_hook(hook, event);
                return Ok(());
            }
        }
        Ok(())
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
            let m = on_pattern_match(&hook.on, event_name);
            if m == OnMatch::None {
                continue;
            }
            if m == OnMatch::RootOnly && !event_is_root_workflow(event) {
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

    #[test]
    fn glob_slash_star_matches_branch_with_prefix() {
        // Positive: "feature/*" matches "feature/my-branch"
        assert!(glob_matches("feature/*", "feature/my-branch"));
        assert!(glob_matches("feature/*", "feature/foo"));
    }

    #[test]
    fn glob_slash_star_does_not_match_other_prefix() {
        // Negative: "feature/*" must NOT match branches without the "feature/" prefix
        assert!(!glob_matches("feature/*", "main"));
        assert!(!glob_matches("feature/*", "fix/my-fix"));
        assert!(!glob_matches("feature/*", "feature"));
    }

    // ── on_pattern_matches (comma-separated) ──────────────────────────

    #[test]
    fn on_pattern_single_event_matches() {
        assert!(on_pattern_matches("gate.waiting", "gate.waiting"));
        assert!(!on_pattern_matches("gate.waiting", "gate.pending_too_long"));
    }

    #[test]
    fn on_pattern_comma_separated_matches_any() {
        assert!(on_pattern_matches(
            "workflow_run.completed,gate.waiting",
            "gate.waiting"
        ));
        assert!(on_pattern_matches(
            "workflow_run.completed,gate.waiting",
            "workflow_run.completed"
        ));
        assert!(!on_pattern_matches(
            "workflow_run.completed,gate.waiting",
            "agent_run.failed"
        ));
    }

    #[test]
    fn on_pattern_comma_with_spaces_trimmed() {
        assert!(on_pattern_matches(
            "workflow_run.completed , gate.waiting",
            "gate.waiting"
        ));
    }

    #[test]
    fn on_pattern_comma_with_wildcard() {
        assert!(on_pattern_matches(
            "workflow_run.*,gate.waiting",
            "workflow_run.failed"
        ));
        assert!(on_pattern_matches(
            "workflow_run.*,gate.waiting",
            "gate.waiting"
        ));
        assert!(!on_pattern_matches(
            "workflow_run.*,gate.waiting",
            "agent_run.completed"
        ));
    }

    #[test]
    fn on_pattern_empty_string_matches_nothing() {
        assert!(!on_pattern_matches("", "gate.waiting"));
    }

    // ── on_pattern_match with :root suffix ──────────────────────────────

    #[test]
    fn on_pattern_root_suffix_returns_root_only() {
        assert_eq!(
            on_pattern_match("workflow_run.completed:root", "workflow_run.completed"),
            OnMatch::RootOnly
        );
    }

    #[test]
    fn on_pattern_no_root_suffix_returns_any() {
        assert_eq!(
            on_pattern_match("workflow_run.completed", "workflow_run.completed"),
            OnMatch::Any
        );
    }

    #[test]
    fn on_pattern_root_suffix_no_match_returns_none() {
        assert_eq!(
            on_pattern_match("workflow_run.completed:root", "gate.waiting"),
            OnMatch::None
        );
    }

    #[test]
    fn on_pattern_comma_mixed_root_and_any() {
        // "completed" is root-only, "gate.waiting" is any
        let pat = "workflow_run.completed:root,gate.waiting";
        assert_eq!(
            on_pattern_match(pat, "workflow_run.completed"),
            OnMatch::RootOnly
        );
        assert_eq!(on_pattern_match(pat, "gate.waiting"), OnMatch::Any);
        assert_eq!(on_pattern_match(pat, "agent_run.failed"), OnMatch::None);
    }

    #[test]
    fn on_pattern_wildcard_with_root() {
        assert_eq!(
            on_pattern_match("workflow_run.*:root", "workflow_run.completed"),
            OnMatch::RootOnly
        );
        assert_eq!(
            on_pattern_match("workflow_run.*:root", "gate.waiting"),
            OnMatch::None
        );
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            cost_usd: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            cost_usd: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    // ── run_shell_hook_capture ───────────────────────────────────────────

    #[test]
    fn capture_success_returns_ok() {
        let hook = HookConfig {
            on: "*".into(),
            run: Some("exit 0".into()),
            timeout_ms: Some(5_000),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(run_shell_hook_capture(&hook, &event).is_ok());
    }

    #[test]
    fn capture_nonzero_with_stderr_returns_err_with_stderr_text() {
        let hook = HookConfig {
            on: "*".into(),
            run: Some("echo 'something went wrong' >&2; exit 1".into()),
            timeout_ms: Some(5_000),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let result = run_shell_hook_capture(&hook, &event);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("something went wrong"));
    }

    #[test]
    fn capture_nonzero_no_stderr_returns_err_with_status() {
        let hook = HookConfig {
            on: "*".into(),
            run: Some("exit 2".into()),
            timeout_ms: Some(5_000),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        let result = run_shell_hook_capture(&hook, &event);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("exit") || msg.contains("status") || msg.contains('2'),
            "got: {msg}"
        );
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            cost_usd: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
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
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
            error: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    // ── repo filter ──────────────────────────────────────────────────────

    #[test]
    fn filter_repo_blocks_non_matching_repo() {
        let hook = HookConfig {
            on: "*".into(),
            repo: Some("conductor-ai".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "other-repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_repo_passes_matching_repo() {
        let hook = HookConfig {
            on: "*".into(),
            repo: Some("conductor-ai".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "conductor-ai".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    // ── branch filter ────────────────────────────────────────────────────

    #[test]
    fn filter_branch_blocks_non_matching_branch() {
        let hook = HookConfig {
            on: "*".into(),
            branch: Some("main".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "feature/foo".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_branch_glob_passes_matching_branch() {
        let hook = HookConfig {
            on: "*".into(),
            branch: Some("feature/*".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "feature/foo".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    // ── step filter ──────────────────────────────────────────────────────

    #[test]
    fn filter_step_blocks_non_matching_step() {
        let hook = HookConfig {
            on: "*".into(),
            step: Some("review".into()),
            ..Default::default()
        };
        let event = NotificationEvent::GateWaiting {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "approve".into(),
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(!hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_step_passes_matching_step() {
        let hook = HookConfig {
            on: "*".into(),
            step: Some("review".into()),
            ..Default::default()
        };
        let event = NotificationEvent::GateWaiting {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            step_name: "review".into(),
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        assert!(hook_event_passes_filters(&hook, &event));
    }

    #[test]
    fn filter_step_ignored_for_non_gate_events() {
        let hook = HookConfig {
            on: "*".into(),
            step: Some("anything".into()),
            ..Default::default()
        };
        let event = NotificationEvent::WorkflowRunCompleted {
            run_id: "r".into(),
            label: "l".into(),
            timestamp: "t".into(),
            url: None,
            workflow_name: "wf".into(),
            parent_workflow_run_id: None,
            repo_slug: "repo".into(),
            branch: "main".into(),
            duration_ms: None,
            ticket_url: None,
        };
        // Non-gate event: step filter is ignored, event passes through
        assert!(hook_event_passes_filters(&hook, &event));
    }
}
