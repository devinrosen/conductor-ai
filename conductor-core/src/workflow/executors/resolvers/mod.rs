mod human_approval;
mod pr_approval;
mod pr_checks;

pub(in crate::workflow) use human_approval::{HumanApprovalGateResolver, HumanGateKind};
pub(in crate::workflow) use pr_approval::PrApprovalGateResolver;
pub(in crate::workflow) use pr_checks::PrChecksGateResolver;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::Config;

// ---------------------------------------------------------------------------
// GitHubTokenCache (moved from gate_resolver.rs)
// ---------------------------------------------------------------------------

/// Thread-safe cache for GitHub App installation tokens.
///
/// One `gh auth token` shell-out per TTL window (55 min on success, 30 s on
/// failure).  `token_override` short-circuits the shell-out for tests.
pub(in crate::workflow) struct GitHubTokenCache {
    cache: Mutex<Option<(Option<String>, Instant)>>,
    override_token: Option<String>,
    owner: String,
}

impl GitHubTokenCache {
    pub(in crate::workflow) fn new(token_override: Option<String>, owner: String) -> Self {
        Self {
            cache: Mutex::new(None),
            override_token: token_override,
            owner,
        }
    }

    #[cfg(test)]
    pub(in crate::workflow) fn set_cache_for_test(
        &self,
        token: Option<String>,
        fetched_at: Instant,
    ) {
        *self.cache.lock().expect("token cache mutex poisoned") = Some((token, fetched_at));
    }

    /// Return the current token, refreshing if stale.
    ///
    /// Returns `None` when no GitHub App is configured and no override is set.
    /// Never sets `GH_TOKEN=""` — callers must only set the env var when `Some`.
    pub(in crate::workflow) fn get(
        &self,
        config: &Config,
        bot_name: Option<&str>,
    ) -> Option<String> {
        if let Some(ref t) = self.override_token {
            return Some(t.clone());
        }
        if bot_name.is_none() && config.github.app.is_none() {
            return None;
        }
        let mut cache = match self.cache.lock() {
            Ok(g) => g,
            Err(_) => {
                tracing::warn!("token cache mutex poisoned; skipping token cache");
                return None;
            }
        };
        let needs_refresh = cache
            .as_ref()
            .map(|(cached_token, fetched_at)| {
                let ttl = if cached_token.is_some() {
                    Duration::from_secs(55 * 60)
                } else {
                    Duration::from_secs(30)
                };
                fetched_at.elapsed() > ttl
            })
            .unwrap_or(true);
        if needs_refresh {
            let token =
                crate::github_app::resolve_named_app_token(config, bot_name, &self.owner, "gate")
                    .token()
                    .map(String::from);
            *cache = Some((token.clone(), Instant::now()));
            token
        } else {
            cache.as_ref().and_then(|(t, _)| t.clone())
        }
    }
}

/// How often the engine re-runs the `gh`-based gate resolvers.
pub(super) const GH_GATE_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Upper bound on a single `gh` invocation. runkon polls resolvers on the
/// engine thread and doesn't bound the call, so a hung `gh` would hang the gate.
pub(super) const GH_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// GhGateCommon
// ---------------------------------------------------------------------------

/// Shared state for gate resolvers that call `gh` on behalf of a bot account.
///
/// Centralises the constructor and token resolution so concrete
/// resolver types stay thin and a single-point change (e.g. the timeout)
/// applies to all of them.
pub(super) struct GhGateCommon {
    pub(super) working_dir: String,
    default_bot_name: Option<String>,
    token_cache: Arc<GitHubTokenCache>,
    pub(super) config: Config,
    #[allow(dead_code)]
    pub(super) db_path: PathBuf,
}

impl GhGateCommon {
    pub(super) fn new(
        working_dir: String,
        default_bot_name: Option<String>,
        token_cache: Arc<GitHubTokenCache>,
        config: Config,
        db_path: PathBuf,
        _owner: String,
    ) -> Self {
        Self {
            working_dir,
            default_bot_name,
            token_cache,
            config,
            db_path,
        }
    }

    /// Resolve the effective bot token and run a `gh` JSON command.
    pub(super) fn run_gh(
        &self,
        args: &[&str],
        params_bot: Option<&str>,
    ) -> Option<serde_json::Value> {
        let effective_bot = params_bot.or(self.default_bot_name.as_deref());
        let token = self.token_cache.get(&self.config, effective_bot);
        run_gh_json(args, &self.working_dir, token.as_deref())
    }
}

/// Run a `gh` command and parse stdout as JSON.
///
/// Logs a warning and returns `None` on subprocess failure or JSON parse error.
pub(super) fn run_gh_json(
    args: &[&str],
    working_dir: &str,
    token: Option<&str>,
) -> Option<serde_json::Value> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(working_dir);
    if let Some(t) = token {
        cmd.env("GH_TOKEN", t);
    }
    run_json_command(cmd, GH_TIMEOUT)
}

/// Kill the whole process group led by `pid` (best effort).
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: plain syscall; a negative pid targets the process group. ESRCH
    // (group already gone) is harmless and ignored.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

/// Kill the child's process group, then the child itself, and reap it.
fn kill_and_reap(child: &mut std::process::Child) {
    kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `cmd`, bounded by `timeout`, and parse stdout as JSON.
///
/// stdout and stderr are drained on helper threads so a large response can't
/// fill the pipe and look like a hang. On timeout the child is killed and
/// `None` is returned (callers treat that as `Pending`).
fn run_json_command(mut cmd: Command, timeout: Duration) -> Option<serde_json::Value> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Put the child in its own process group so a timeout can kill any
    // grandchildren that inherited the pipes; otherwise they would keep the
    // drain threads and pipe fds alive on every timed-out poll.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("gh command failed: {e}");
            return None;
        }
    };
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    }
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());

    let pid = child.id();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_and_reap(&mut child);
                    tracing::warn!(
                        "gh command timed out after {timeout:?}; treating gate as pending"
                    );
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!("gh command wait failed: {e}");
                kill_and_reap(&mut child);
                return None;
            }
        }
    };
    // The child has exited, but a grandchild may have inherited the pipes and
    // kept them open. Bound the drain by the same deadline; on expiry kill the
    // process group so the pipes close and the reader threads terminate.
    let remaining = |d: Instant| d.saturating_duration_since(Instant::now());
    let (Ok(stdout), Ok(stderr)) = (
        out_reader.recv_timeout(remaining(deadline)),
        err_reader.recv_timeout(remaining(deadline)),
    ) else {
        kill_process_group(pid);
        tracing::warn!(
            "gh output pipes still open {timeout:?} after start; treating gate as pending"
        );
        return None;
    };
    process_gh_output(status.success(), &stdout, &stderr)
}

/// Parse `gh` subprocess output into a JSON value.
///
/// Separated from `run_gh_json` so the success/failure logic can be unit-tested
/// without spawning a real subprocess.
fn process_gh_output(success: bool, stdout: &[u8], stderr: &[u8]) -> Option<serde_json::Value> {
    if !success {
        // Truncate stderr before logging: gh CLI can include auth tokens/scopes
        // in 401/403 error messages, so we emit only the first 200 characters.
        let stderr_str = String::from_utf8_lossy(stderr);
        let excerpt = stderr_str.trim();
        let truncated = if excerpt.len() > 200 {
            let end = excerpt.floor_char_boundary(200);
            &excerpt[..end]
        } else {
            excerpt
        };
        tracing::warn!(
            "gh command exited non-zero (stderr truncated): {}",
            truncated
        );
        return None;
    }
    let json_str = String::from_utf8_lossy(stdout);
    match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(val) => Some(val),
        Err(e) => {
            tracing::warn!("gh command JSON parse error: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn test_run_json_command_timeout_returns_none() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let start = Instant::now();
        let result = run_json_command(cmd, Duration::from_millis(200));
        assert!(result.is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_json_command_lingering_grandchild_is_bounded() {
        // The shell exits immediately but a backgrounded grandchild keeps the
        // inherited stdout pipe open.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5 & echo '{}'"]);
        let start = Instant::now();
        let result = run_json_command(cmd, Duration::from_millis(300));
        assert!(result.is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    /// Poll until `pid` no longer exists (or ~3 s pass).
    #[cfg(unix)]
    fn wait_until_gone(pid: i32) -> bool {
        for _ in 0..60 {
            // SAFETY: signal 0 only probes for existence.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[cfg(unix)]
    fn read_pid(path: &std::path::Path) -> i32 {
        for _ in 0..60 {
            if let Ok(s) = std::fs::read_to_string(path) {
                if let Ok(pid) = s.trim().parse() {
                    return pid;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("grandchild pid file never written");
    }

    #[cfg(unix)]
    #[test]
    fn test_run_json_command_timeout_kills_grandchild() {
        let dir = std::env::temp_dir().join(format!("rjc-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("sleep 30 & echo $! > {}; wait", pid_file.display()),
        ]);
        let result = run_json_command(cmd, Duration::from_millis(500));
        assert!(result.is_none());
        let pid = read_pid(&pid_file);
        assert!(wait_until_gone(pid), "grandchild {pid} survived timeout");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_json_command_drain_expiry_kills_grandchild() {
        let dir = std::env::temp_dir().join(format!("rjc-drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("sleep 30 & echo $! > {}; echo '{{}}'", pid_file.display()),
        ]);
        let result = run_json_command(cmd, Duration::from_millis(500));
        assert!(result.is_none());
        let pid = read_pid(&pid_file);
        assert!(
            wait_until_gone(pid),
            "grandchild {pid} survived drain expiry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_json_command_success_returns_json() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", r#"echo '{"ok":true}'"#]);
        let result = run_json_command(cmd, Duration::from_secs(5));
        assert_eq!(result, Some(json!({"ok": true})));
    }

    #[cfg(unix)]
    #[test]
    fn test_run_json_command_large_output_does_not_deadlock() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            r#"printf '"'; head -c 300000 /dev/zero | tr '\0' 'a'; printf '"'"#,
        ]);
        let result = run_json_command(cmd, Duration::from_secs(10));
        assert_eq!(result.and_then(|v| v.as_str().map(str::len)), Some(300000));
    }

    #[test]
    fn test_process_gh_output_success_valid_json() {
        let stdout = br#"{"state":"APPROVED"}"#;
        let result = process_gh_output(true, stdout, b"");
        assert_eq!(result, Some(json!({"state": "APPROVED"})));
    }

    #[test]
    fn test_process_gh_output_non_zero_exit_returns_none() {
        let result = process_gh_output(false, b"", b"some error");
        assert!(result.is_none());
    }

    #[test]
    fn test_process_gh_output_invalid_json_returns_none() {
        let result = process_gh_output(true, b"not valid json {{{", b"");
        assert!(result.is_none());
    }

    #[test]
    fn test_process_gh_output_empty_stdout_returns_none() {
        let result = process_gh_output(true, b"", b"");
        assert!(result.is_none());
    }

    #[test]
    fn test_run_gh_json_nonexistent_dir_returns_none() {
        // Subprocess launch fails when working_dir doesn't exist → None without panic.
        let result = run_gh_json(&["pr", "view"], "/nonexistent/conductor/test/dir", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_process_gh_output_multibyte_stderr_does_not_panic() {
        // Each "é" is 2 bytes. 101 of them = 202 bytes but only 101 chars.
        // Without floor_char_boundary, &s[..200] would split the last é and panic.
        let stderr: Vec<u8> = "é".repeat(101).into_bytes();
        // Must not panic; result is None because success=false.
        let result = process_gh_output(false, b"", &stderr);
        assert!(result.is_none());
    }

    #[test]
    fn test_token_cache_override_short_circuits_shell() {
        let cache = GitHubTokenCache::new(Some("test-token-override".into()), String::new());
        let config = Config::default();
        let token = cache.get(&config, None);
        assert_eq!(token.as_deref(), Some("test-token-override"));
    }

    #[test]
    fn test_token_cache_none_when_no_app_configured() {
        let cache = GitHubTokenCache::new(None, String::new());
        let config = Config::default(); // no github.app configured
        let token = cache.get(&config, None);
        assert!(
            token.is_none(),
            "expected None when no GitHub App configured"
        );
    }

    #[test]
    fn test_token_cache_ttl_success_path_override() {
        let cache = GitHubTokenCache::new(Some("my-override-token".into()), String::new());
        let config = Config::default();

        let first = cache.get(&config, None);
        let second = cache.get(&config, None);

        assert_eq!(first.as_deref(), Some("my-override-token"));
        assert_eq!(
            first, second,
            "override token must be returned consistently on repeated calls"
        );
    }

    #[test]
    fn test_token_cache_bot_name_none_no_app_returns_none() {
        let cache = GitHubTokenCache::new(None, String::new());
        let config = Config::default();
        let token = cache.get(&config, None);
        assert!(
            token.is_none(),
            "expected None when bot_name is None and no GitHub App configured"
        );
    }

    #[test]
    fn test_token_cache_stale_success_triggers_refresh() {
        let cache = GitHubTokenCache::new(None, String::new());
        let config = Config::default();
        let stale_instant = Instant::now() - Duration::from_secs(56 * 60);
        cache.set_cache_for_test(Some("old-token".into()), stale_instant);
        let token = cache.get(&config, Some("bot"));
        assert!(
            token.is_none(),
            "stale success entry must trigger refresh; cached token must not be returned"
        );
    }

    #[test]
    fn test_token_cache_fresh_success_no_refresh() {
        let cache = GitHubTokenCache::new(None, String::new());
        let config = Config::default();
        let fresh_instant = Instant::now() - Duration::from_secs(60);
        cache.set_cache_for_test(Some("live-token".into()), fresh_instant);
        let token = cache.get(&config, Some("bot"));
        assert_eq!(
            token.as_deref(),
            Some("live-token"),
            "fresh success entry must be returned from cache without refresh"
        );
    }

    #[test]
    fn test_token_cache_stale_failure_triggers_refresh() {
        let cache = GitHubTokenCache::new(None, String::new());
        let config = Config::default();
        let stale_instant = Instant::now() - Duration::from_secs(31);
        cache.set_cache_for_test(None, stale_instant);
        let token = cache.get(&config, Some("bot"));
        assert!(
            token.is_none(),
            "refresh after stale failure should return None"
        );
        let token2 = cache.get(&config, Some("bot"));
        assert!(token2.is_none());
    }

    #[test]
    fn test_token_cache_fresh_failure_no_refresh() {
        let cache = GitHubTokenCache::new(None, String::new());
        let config = Config::default();
        let fresh_instant = Instant::now() - Duration::from_secs(15);
        cache.set_cache_for_test(None, fresh_instant);
        let token = cache.get(&config, Some("bot"));
        assert!(
            token.is_none(),
            "fresh failure entry must be returned from cache (None) without refresh"
        );
    }
}
