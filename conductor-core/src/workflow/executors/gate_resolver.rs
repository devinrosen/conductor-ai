use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    db_path: PathBuf,
    token_cache: Arc<GitHubTokenCache>,
) -> HashMap<String, Box<dyn GateResolver>> {
    let mut map: HashMap<String, Box<dyn GateResolver>> = HashMap::new();
    register(
        &mut map,
        Box::new(PrApprovalGateResolver::new(Arc::clone(&token_cache))),
    );
    register(
        &mut map,
        Box::new(PrChecksGateResolver::new(Arc::clone(&token_cache))),
    );
    register(
        &mut map,
        Box::new(HumanApprovalGateResolver::new(
            db_path.clone(),
            HumanGateKind::HumanApproval,
        )),
    );
    register(
        &mut map,
        Box::new(HumanApprovalGateResolver::new(
            db_path,
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
        let token_cache = Arc::new(GitHubTokenCache::new(None));
        let resolvers = build_default_gate_resolvers(PathBuf::from("/tmp/test.db"), token_cache);
        assert!(
            !resolvers.contains_key("unknown_gate_xyz"),
            "unknown gate type should not be registered"
        );
    }
}
