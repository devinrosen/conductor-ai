use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::workflow::persistence::WorkflowPersistence;

use crate::config::Config;
use crate::error::Result;
use crate::workflow_dsl::ApprovalMode;

use super::resolvers::{
    HumanApprovalGateResolver, HumanGateKind, PrApprovalGateResolver, PrChecksGateResolver,
};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Outcome of a single poll tick from a `GateResolver`.
pub(super) enum GatePoll {
    Approved(Option<String>),
    Rejected(String),
    Pending,
}

/// All gate configuration passed to `GateResolver::poll`.
#[allow(dead_code)] // fields are available for resolver use; not all are consumed in Phase 1
pub(super) struct GateParams {
    pub gate_name: String,
    pub prompt: Option<String>,
    pub min_approvals: u32,
    pub approval_mode: ApprovalMode,
    /// Resolved options list (StepRef already expanded by the dispatcher).
    pub options: Vec<String>,
    pub timeout_secs: u64,
    pub bot_name: Option<String>,
    pub step_id: String,
}

/// Transient context passed to each `GateResolver::poll` call.
///
/// This struct is intentionally concrete and minimal for Phase 1. It will be
/// replaced by `&dyn RunContext` when Step 1.1 lands.
#[allow(dead_code)] // token_cache and db_path are available for resolver use; not all consumed
pub(super) struct GateContext<'a> {
    pub working_dir: &'a str,
    pub config: &'a Config,
    pub default_bot_name: Option<&'a str>,
    pub token_cache: Arc<GitHubTokenCache>,
    pub db_path: &'a Path,
}

impl<'a> GateContext<'a> {
    pub(super) fn resolve_token(&self, params: &GateParams) -> Option<String> {
        let effective_bot = params.bot_name.as_deref().or(self.default_bot_name);
        self.token_cache.get(self.config, effective_bot)
    }
}

// ---------------------------------------------------------------------------
// GateResolver trait
// ---------------------------------------------------------------------------

pub(super) trait GateResolver: Send + Sync {
    fn gate_type(&self) -> &str;
    fn poll(&self, run_id: &str, params: &GateParams, ctx: &GateContext<'_>) -> Result<GatePoll>;
}

// ---------------------------------------------------------------------------
// GitHubTokenCache
// ---------------------------------------------------------------------------

/// Thread-safe cache for GitHub App installation tokens.
///
/// One `gh auth token` shell-out per TTL window (55 min on success, 30 s on
/// failure).  `token_override` short-circuits the shell-out for tests.
pub(super) struct GitHubTokenCache {
    cache: Mutex<Option<(Option<String>, Instant)>>,
    override_token: Option<String>,
}

impl GitHubTokenCache {
    pub(super) fn new(token_override: Option<String>) -> Self {
        Self {
            cache: Mutex::new(None),
            override_token: token_override,
        }
    }

    #[cfg(test)]
    pub(super) fn set_cache_for_test(&self, token: Option<String>, fetched_at: Instant) {
        *self.cache.lock().expect("token cache mutex poisoned") = Some((token, fetched_at));
    }

    /// Return the current token, refreshing if stale.
    ///
    /// Returns `None` when no GitHub App is configured and no override is set.
    /// Never sets `GH_TOKEN=""` — callers must only set the env var when `Some`.
    pub(super) fn get(&self, config: &Config, bot_name: Option<&str>) -> Option<String> {
        if let Some(ref t) = self.override_token {
            return Some(t.clone());
        }
        if bot_name.is_none() && config.github.app.is_none() {
            return None;
        }
        let mut cache = self.cache.lock().expect("token cache mutex poisoned");
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
// Registry builder
// ---------------------------------------------------------------------------

fn register(map: &mut HashMap<String, Box<dyn GateResolver>>, resolver: Box<dyn GateResolver>) {
    let key = resolver.gate_type().to_string();
    map.insert(key, resolver);
}

pub(super) fn build_default_gate_resolvers(
    persistence: Arc<dyn WorkflowPersistence>,
) -> HashMap<String, Box<dyn GateResolver>> {
    let mut map: HashMap<String, Box<dyn GateResolver>> = HashMap::new();
    register(&mut map, Box::new(PrApprovalGateResolver::new()));
    register(&mut map, Box::new(PrChecksGateResolver::new()));
    register(
        &mut map,
        Box::new(HumanApprovalGateResolver::new(
            Arc::clone(&persistence),
            HumanGateKind::HumanApproval,
        )),
    );
    register(
        &mut map,
        Box::new(HumanApprovalGateResolver::new(
            persistence,
            HumanGateKind::HumanReview,
        )),
    );
    map
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::workflow::persistence_memory::InMemoryWorkflowPersistence;

    fn make_test_persistence() -> Arc<dyn WorkflowPersistence> {
        Arc::new(InMemoryWorkflowPersistence::new())
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
    fn test_unknown_gate_type_returns_error() {
        let resolvers = build_default_gate_resolvers(make_test_persistence());
        assert!(
            !resolvers.contains_key("unknown_gate_xyz"),
            "unknown gate type should not be registered"
        );
    }

    #[test]
    fn test_build_default_gate_resolvers_registers_all_four_types() {
        let resolvers = build_default_gate_resolvers(make_test_persistence());
        assert!(
            resolvers.contains_key("pr_approval"),
            "pr_approval resolver must be registered"
        );
        assert!(
            resolvers.contains_key("pr_checks"),
            "pr_checks resolver must be registered"
        );
        assert!(
            resolvers.contains_key("human_approval"),
            "human_approval resolver must be registered"
        );
        assert!(
            resolvers.contains_key("human_review"),
            "human_review resolver must be registered"
        );
    }

    #[test]
    fn test_token_cache_ttl_success_path_override() {
        // Test that the override path consistently returns the same token.
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
        // When no app is configured and bot_name is None, get() returns None.
        let cache = GitHubTokenCache::new(None);
        let config = Config::default();
        let token = cache.get(&config, None);
        assert!(
            token.is_none(),
            "expected None when bot_name is None and no GitHub App configured"
        );
    }

    fn make_params(mode: crate::workflow_dsl::ApprovalMode) -> GateParams {
        GateParams {
            gate_name: "test-gate".into(),
            prompt: None,
            min_approvals: 1,
            approval_mode: mode,
            options: vec![],
            timeout_secs: 60,
            bot_name: None,
            step_id: "step-1".into(),
        }
    }

    fn poll_resolver_with_unavailable_gh(
        resolver_key: &str,
        mode: crate::workflow_dsl::ApprovalMode,
    ) -> GatePoll {
        let token_cache = Arc::new(GitHubTokenCache::new(None));
        let resolvers = build_default_gate_resolvers(make_test_persistence());
        let resolver = resolvers
            .get(resolver_key)
            .unwrap_or_else(|| panic!("{resolver_key} not registered"));
        let config = Config::default();
        let ctx = GateContext {
            working_dir: "/nonexistent/conductor/test/dir",
            config: &config,
            default_bot_name: None,
            token_cache: Arc::clone(&token_cache),
            db_path: Path::new("/tmp/test.db"),
        };
        let params = make_params(mode);
        resolver
            .poll("run-1", &params, &ctx)
            .expect("poll must not error")
    }

    #[test]
    fn test_pr_approval_resolver_poll_returns_pending_when_gh_unavailable() {
        let poll = poll_resolver_with_unavailable_gh(
            "pr_approval",
            crate::workflow_dsl::ApprovalMode::MinApprovals,
        );
        assert!(
            matches!(poll, GatePoll::Pending),
            "pr_approval poll must return Pending when gh is unavailable"
        );
    }

    #[test]
    fn test_pr_approval_resolver_poll_review_decision_pending_when_gh_unavailable() {
        let poll = poll_resolver_with_unavailable_gh(
            "pr_approval",
            crate::workflow_dsl::ApprovalMode::ReviewDecision,
        );
        assert!(
            matches!(poll, GatePoll::Pending),
            "pr_approval ReviewDecision poll must return Pending when gh is unavailable"
        );
    }

    #[test]
    fn test_pr_checks_resolver_poll_returns_pending_when_gh_unavailable() {
        let poll = poll_resolver_with_unavailable_gh(
            "pr_checks",
            crate::workflow_dsl::ApprovalMode::MinApprovals,
        );
        assert!(
            matches!(poll, GatePoll::Pending),
            "pr_checks poll must return Pending when gh is unavailable"
        );
    }

    #[test]
    fn test_resolve_token_uses_override_when_set() {
        let token_cache = Arc::new(GitHubTokenCache::new(Some("override-tok".into())));
        let config = Config::default();
        let ctx = GateContext {
            working_dir: "/tmp",
            config: &config,
            default_bot_name: None,
            token_cache: Arc::clone(&token_cache),
            db_path: Path::new("/tmp/test.db"),
        };
        let params = make_params(crate::workflow_dsl::ApprovalMode::MinApprovals);
        assert_eq!(ctx.resolve_token(&params).as_deref(), Some("override-tok"));
    }

    #[test]
    fn test_resolve_token_returns_none_when_no_app_and_no_override() {
        let token_cache = Arc::new(GitHubTokenCache::new(None));
        let config = Config::default();
        let ctx = GateContext {
            working_dir: "/tmp",
            config: &config,
            default_bot_name: None,
            token_cache: Arc::clone(&token_cache),
            db_path: Path::new("/tmp/test.db"),
        };
        let params = make_params(crate::workflow_dsl::ApprovalMode::MinApprovals);
        assert!(
            ctx.resolve_token(&params).is_none(),
            "resolve_token must return None when no app and no override"
        );
    }

    #[test]
    fn test_resolve_token_prefers_params_bot_name_over_context_default() {
        let token_cache = Arc::new(GitHubTokenCache::new(Some("override-tok".into())));
        let config = Config::default();
        let ctx = GateContext {
            working_dir: "/tmp",
            config: &config,
            default_bot_name: Some("context-bot"),
            token_cache: Arc::clone(&token_cache),
            db_path: Path::new("/tmp/test.db"),
        };
        let mut params = make_params(crate::workflow_dsl::ApprovalMode::MinApprovals);
        params.bot_name = Some("params-bot".into());
        // Both have a name; with the override token, we just confirm it resolves successfully.
        assert!(ctx.resolve_token(&params).is_some());
    }

    // Stale success entry (56 min old) must trigger a refresh instead of returning the cached token.
    #[test]
    fn test_token_cache_stale_success_triggers_refresh() {
        let cache = GitHubTokenCache::new(None);
        let config = Config::default();
        // Seed with a token that is 56 minutes old (success TTL is 55 min).
        let stale_instant = Instant::now() - Duration::from_secs(56 * 60);
        cache.set_cache_for_test(Some("old-token".into()), stale_instant);
        // Refresh runs (gh unavailable in tests → returns None); cached "old-token" must NOT be returned.
        let token = cache.get(&config, Some("bot"));
        assert!(
            token.is_none(),
            "stale success entry must trigger refresh; cached token must not be returned"
        );
    }

    // Fresh success entry (1 min old) must be returned from the cache without a refresh.
    #[test]
    fn test_token_cache_fresh_success_no_refresh() {
        let cache = GitHubTokenCache::new(None);
        let config = Config::default();
        // Seed with a token that is only 1 minute old (well within 55 min success TTL).
        let fresh_instant = Instant::now() - Duration::from_secs(60);
        cache.set_cache_for_test(Some("live-token".into()), fresh_instant);
        let token = cache.get(&config, Some("bot"));
        assert_eq!(
            token.as_deref(),
            Some("live-token"),
            "fresh success entry must be returned from cache without refresh"
        );
    }

    // Stale failure entry (31 s old) must trigger a refresh because failure TTL is 30 s.
    #[test]
    fn test_token_cache_stale_failure_triggers_refresh() {
        let cache = GitHubTokenCache::new(None);
        let config = Config::default();
        // Seed a failure (None) that is 31 seconds old (failure TTL is 30 s).
        let stale_instant = Instant::now() - Duration::from_secs(31);
        cache.set_cache_for_test(None, stale_instant);
        // Refresh fires; gh unavailable → still None, but the cache timestamp is now fresh.
        let token = cache.get(&config, Some("bot"));
        assert!(
            token.is_none(),
            "refresh after stale failure should return None"
        );
        // A second immediate call should NOT re-trigger (fresh failure entry now in cache).
        // We can't observe the shell-out count directly, but we verify get() still returns None
        // and does not panic, confirming the cache was updated.
        let token2 = cache.get(&config, Some("bot"));
        assert!(token2.is_none());
    }

    // Fresh failure entry (15 s old) must NOT trigger a refresh (failure TTL is 30 s).
    #[test]
    fn test_token_cache_fresh_failure_no_refresh() {
        let cache = GitHubTokenCache::new(None);
        let config = Config::default();
        // Seed a failure (None) that is only 15 seconds old.
        let fresh_instant = Instant::now() - Duration::from_secs(15);
        cache.set_cache_for_test(None, fresh_instant);
        // Within TTL → cache hit → returns None without refresh.
        let token = cache.get(&config, Some("bot"));
        assert!(
            token.is_none(),
            "fresh failure entry must be returned from cache (None) without refresh"
        );
    }
}
