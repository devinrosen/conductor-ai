use std::collections::HashMap;
use std::sync::Arc;

use runkon_flow::traits::run_context::RunContext;
use runkon_flow::traits::script_env_provider::ScriptEnvProvider;

use crate::config::Config;
use crate::github_app::{resolve_named_app_token, TokenResolution};

/// Conductor-specific script env provider.
///
/// Composes:
///
/// 1. A `PATH` that prepends `conductor_bin_dir` and any extra plugin
///    directories in front of the inherited `PATH`, so script steps can
///    invoke the `conductor` binary without it needing to be on the user's
///    `$PATH`.
/// 2. A `GH_TOKEN` env var resolved from the workflow step's `as = "..."`
///    directive (or the workflow-level default bot), so `gh` calls in the
///    script run as that GitHub App identity rather than the conductor
///    user. Falls back to the user's `gh` credentials when no bot is
///    requested or token resolution fails.
pub(crate) struct ConductorScriptEnvProvider {
    conductor_bin_dir: Option<std::path::PathBuf>,
    extra_plugin_dirs: Vec<String>,
    config: Arc<Config>,
}

impl ConductorScriptEnvProvider {
    pub(crate) fn new(
        conductor_bin_dir: Option<std::path::PathBuf>,
        extra_plugin_dirs: Vec<String>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            conductor_bin_dir,
            extra_plugin_dirs,
            config,
        }
    }
}

impl ScriptEnvProvider for ConductorScriptEnvProvider {
    fn env(&self, _ctx: &dyn RunContext, bot_name: Option<&str>) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Some(ref bin_dir) = self.conductor_bin_dir {
            let existing = std::env::var("PATH").unwrap_or_default();
            let mut parts = vec![bin_dir.display().to_string()];
            parts.extend(self.extra_plugin_dirs.iter().cloned());
            if !existing.is_empty() {
                parts.push(existing);
            }
            env.insert("PATH".to_string(), parts.join(":"));
        }

        // Resolve a GitHub App installation token for the requested bot
        // identity. NotConfigured / Fallback both leave GH_TOKEN unset so
        // the script falls back to the user's `gh auth` credentials.
        if let Some(name) = bot_name {
            match resolve_named_app_token(&self.config, Some(name), "script") {
                TokenResolution::AppToken(token) => {
                    env.insert("GH_TOKEN".to_string(), token);
                }
                TokenResolution::Fallback { reason } => {
                    tracing::warn!(
                        bot = name,
                        reason = %reason,
                        "GitHub App token resolution failed for `as = \"{}\"`; \
                         script will use the gh CLI user identity",
                        name
                    );
                }
                TokenResolution::NotConfigured => {
                    tracing::warn!(
                        bot = name,
                        "Workflow requested `as = \"{}\"` but no matching \
                         [github.apps.{}] is configured; script will use the \
                         gh CLI user identity",
                        name,
                        name
                    );
                }
            }
        }

        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_ctx() -> runkon_flow::traits::run_context::NoopRunContext {
        runkon_flow::traits::run_context::NoopRunContext::default()
    }

    #[test]
    fn test_env_empty_when_no_bin_dir_and_no_bot() {
        let provider = ConductorScriptEnvProvider::new(None, vec![], Arc::new(Config::default()));
        let env = provider.env(&noop_ctx(), None);
        assert!(
            env.is_empty(),
            "expected empty env when no bin_dir and no bot, got: {env:?}"
        );
    }

    #[test]
    fn test_env_path_starts_with_bin_dir() {
        let bin_dir = std::path::PathBuf::from("/usr/local/bin/conductor-dir");
        let provider =
            ConductorScriptEnvProvider::new(Some(bin_dir), vec![], Arc::new(Config::default()));
        let env = provider.env(&noop_ctx(), None);
        let path = env.get("PATH").expect("PATH should be set");
        assert!(
            path.starts_with("/usr/local/bin/conductor-dir"),
            "PATH should start with conductor_bin_dir, got: {path}"
        );
    }

    #[test]
    fn test_env_path_includes_plugin_dirs() {
        let bin_dir = std::path::PathBuf::from("/bin/conductor");
        let provider = ConductorScriptEnvProvider::new(
            Some(bin_dir),
            vec!["/opt/plugins".to_string(), "/home/user/plugins".to_string()],
            Arc::new(Config::default()),
        );
        let env = provider.env(&noop_ctx(), None);
        let path = env.get("PATH").expect("PATH should be set");
        let parts: Vec<&str> = path.splitn(4, ':').collect();
        assert_eq!(parts[0], "/bin/conductor");
        assert_eq!(parts[1], "/opt/plugins");
        assert_eq!(parts[2], "/home/user/plugins");
    }

    #[test]
    fn bot_name_with_no_app_configured_omits_gh_token() {
        // No [github.apps.<name>] configured → NotConfigured branch.
        // Caller must NOT see a GH_TOKEN in the env (otherwise scripts
        // would inherit a stale or empty token).
        let provider = ConductorScriptEnvProvider::new(None, vec![], Arc::new(Config::default()));
        let env = provider.env(&noop_ctx(), Some("reviewer"));
        assert!(
            !env.contains_key("GH_TOKEN"),
            "GH_TOKEN must not be set when the named bot is not configured"
        );
    }

    #[test]
    fn no_bot_name_omits_gh_token() {
        let provider = ConductorScriptEnvProvider::new(None, vec![], Arc::new(Config::default()));
        let env = provider.env(&noop_ctx(), None);
        assert!(!env.contains_key("GH_TOKEN"));
    }
}
