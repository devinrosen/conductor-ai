mod human_approval;
mod pr_approval;
mod pr_checks;

pub(in crate::workflow) use human_approval::{HumanApprovalGateResolver, HumanGateKind};
pub(in crate::workflow) use pr_approval::PrApprovalGateResolver;
pub(in crate::workflow) use pr_checks::PrChecksGateResolver;

use std::path::PathBuf;
use std::process::Command;
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
}

impl GitHubTokenCache {
    pub(in crate::workflow) fn new(token_override: Option<String>) -> Self {
        Self {
            cache: Mutex::new(None),
            override_token: token_override,
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
            let token = crate::github_app::resolve_named_app_token(config, bot_name, "gate")
                .token()
                .map(String::from);
            *cache = Some((token.clone(), Instant::now()));
            token
        } else {
            cache.as_ref().and_then(|(t, _)| t.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// GhGateCommon
// ---------------------------------------------------------------------------

/// Shared state for gate resolvers that call `gh` on behalf of a bot account.
///
/// Centralises the constructor and token resolution so concrete
/// resolver types stay thin and a single-point change (e.g. adding a timeout)
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
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("gh command failed: {e}");
            return None;
        }
    };
    process_gh_output(output.status.success(), &output.stdout, &output.stderr)
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
        let cache = GitHubTokenCache::new(Some("test-token-override".into()));
        let config = Config::default();
        let token = cache.get(&config, None);
        assert_eq!(token.as_deref(), Some("test-token-override"));
    }

    #[test]
    fn test_token_cache_none_when_no_app_configured() {
        let cache = GitHubTokenCache::new(None);
        let config = Config::default(); // no github.app configured
        let token = cache.get(&config, None);
        assert!(
            token.is_none(),
            "expected None when no GitHub App configured"
        );
    }

    #[test]
    fn test_token_cache_ttl_success_path_override() {
        let cache = GitHubTokenCache::new(Some("my-override-token".into()));
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
        let cache = GitHubTokenCache::new(None);
        let config = Config::default();
        let token = cache.get(&config, None);
        assert!(
            token.is_none(),
            "expected None when bot_name is None and no GitHub App configured"
        );
    }

    #[test]
    fn test_token_cache_stale_success_triggers_refresh() {
        let cache = GitHubTokenCache::new(None);
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
        let cache = GitHubTokenCache::new(None);
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
        let cache = GitHubTokenCache::new(None);
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
        let cache = GitHubTokenCache::new(None);
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
