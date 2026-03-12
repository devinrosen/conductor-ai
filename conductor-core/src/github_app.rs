//! GitHub App authentication: JWT generation and installation token exchange.
//!
//! When a GitHub App is configured in `config.toml`, this module generates
//! short-lived installation tokens so that PR comments appear under the bot
//! identity (e.g. `conductor-ai[bot]`) instead of the human `gh` user.

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::config::{Config, GitHubAppConfig};
use crate::error::{ConductorError, Result};

/// JWT claims for GitHub App authentication (RS256).
#[derive(Serialize)]
struct Claims {
    iat: u64,
    exp: u64,
    iss: String,
}

/// Response from the GitHub installation token endpoint.
#[derive(Deserialize)]
struct InstallationTokenResponse {
    token: String,
}

/// Generate a short-lived JWT for the GitHub App.
///
/// The JWT is valid for 10 minutes (GitHub's maximum) and is signed with
/// the App's private key using RS256.
fn generate_jwt(app_config: &GitHubAppConfig) -> Result<String> {
    let key_path = shellexpand_tilde(&app_config.private_key_path);
    let pem = std::fs::read(&key_path).map_err(|e| {
        ConductorError::Config(format!(
            "failed to read GitHub App private key at {key_path}: {e}"
        ))
    })?;

    let encoding_key = EncodingKey::from_rsa_pem(&pem)
        .map_err(|e| ConductorError::Config(format!("invalid GitHub App private key: {e}")))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let iss = app_config
        .client_id
        .clone()
        .unwrap_or_else(|| app_config.app_id.to_string());

    // GitHub App client IDs follow the pattern "Iv..." (e.g. "Iv23liXXXXXXXXXXXXXX").
    // Warn early when a configured value looks wrong so auth failures are easier to diagnose.
    if let Some(ref cid) = app_config.client_id {
        if !cid.starts_with("Iv") {
            tracing::warn!(
                client_id = %cid,
                "github.app.client_id does not match the expected GitHub App client ID format \
                 (\"Iv...\"). Verify the value on your App's settings page."
            );
        }
    }

    let claims = Claims {
        // Issue 60s in the past to account for clock drift
        iat: now.saturating_sub(60),
        // Expire in 10 minutes (GitHub maximum)
        exp: now + 600,
        iss,
    };

    let header = Header::new(Algorithm::RS256);
    encode(&header, &claims, &encoding_key)
        .map_err(|e| ConductorError::Config(format!("failed to sign JWT: {e}")))
}

/// Exchange a GitHub App JWT for a short-lived installation access token.
///
/// Makes a direct HTTPS request to the GitHub API instead of using `gh api`,
/// so that the JWT is kept in memory and never exposed as a command-line
/// argument (which would be visible to other processes via `ps`/`/proc`).
fn exchange_installation_token(app_config: &GitHubAppConfig, jwt: &str) -> Result<String> {
    let url = format!(
        "https://api.github.com/app/installations/{}/access_tokens",
        app_config.installation_id
    );

    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "conductor-ai")
        .call()
        .map_err(|e| {
            ConductorError::TicketSync(format!("GitHub App token exchange failed: {e}"))
        })?;

    let token_resp: InstallationTokenResponse = resp.into_json().map_err(|e| {
        ConductorError::TicketSync(format!("failed to parse installation token response: {e}"))
    })?;

    Ok(token_resp.token)
}

/// Obtain a GitHub App installation token, if configured.
///
/// Returns `Ok(Some(token))` when a GitHub App is configured and token
/// generation succeeds. Returns `Ok(None)` when no app is configured
/// (graceful fallback to the `gh` CLI user). Returns `Err` only on
/// hard failures (bad key, API error).
pub fn get_app_token(app_config: &GitHubAppConfig) -> Result<String> {
    let jwt = generate_jwt(app_config)?;
    exchange_installation_token(app_config, &jwt)
}

/// Result of attempting to resolve a GitHub App installation token.
///
/// Makes the authentication identity explicit so callers can distinguish
/// between a real App token and a fallback to the `gh` CLI user identity.
#[derive(Clone, PartialEq, Eq)]
pub enum TokenResolution {
    /// Successfully obtained a GitHub App installation token.
    AppToken(String),
    /// No GitHub App is configured; falling back to `gh` CLI user identity.
    NotConfigured,
    /// GitHub App is configured but token acquisition failed; falling back
    /// to `gh` CLI user identity.
    Fallback { reason: String },
}

impl std::fmt::Debug for TokenResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenResolution::AppToken(_) => write!(f, "AppToken([REDACTED])"),
            TokenResolution::NotConfigured => write!(f, "NotConfigured"),
            TokenResolution::Fallback { reason } => {
                f.debug_struct("Fallback").field("reason", reason).finish()
            }
        }
    }
}

impl TokenResolution {
    /// Returns the token string if an App token was obtained, `None` otherwise.
    pub fn token(&self) -> Option<&str> {
        match self {
            TokenResolution::AppToken(t) => Some(t),
            TokenResolution::NotConfigured | TokenResolution::Fallback { .. } => None,
        }
    }

    /// Returns `true` when using the `gh` CLI identity due to a token failure.
    pub fn is_fallback(&self) -> bool {
        matches!(self, TokenResolution::Fallback { .. })
    }
}

/// Resolve a GitHub App installation token, optionally looking up a named identity.
///
/// Resolution order:
/// 1. If `name` is `Some(n)`, look up `config.github.apps[n]` and obtain a token.
/// 2. If not found or `name` is `None`, fall back to `config.github.app`.
/// 3. If neither is configured, return [`TokenResolution::NotConfigured`].
pub fn resolve_named_app_token(
    config: &Config,
    name: Option<&str>,
    context: &str,
) -> TokenResolution {
    // Try named app first
    if let Some(n) = name {
        if let Some(app_config) = config.github.apps.get(n) {
            return match get_app_token(app_config) {
                Ok(token) => TokenResolution::AppToken(token),
                Err(e) => {
                    tracing::warn!(context, name = n, error = %e,
                        "Named GitHub App token failed, falling back to gh user");
                    TokenResolution::Fallback {
                        reason: e.to_string(),
                    }
                }
            };
        }
        // Named app not configured — warn and fall through to singleton
        tracing::warn!(
            context,
            name = n,
            "Named GitHub App '{}' not found in [github.apps], falling back to singleton [github.app]",
            n
        );
    }
    // Fall back to the singleton [github.app]
    let app_config = match config.github.app.as_ref() {
        Some(c) => c,
        None => return TokenResolution::NotConfigured,
    };
    match get_app_token(app_config) {
        Ok(token) => TokenResolution::AppToken(token),
        Err(e) => {
            tracing::warn!(context, error = %e, "GitHub App token failed, falling back to gh user");
            TokenResolution::Fallback {
                reason: e.to_string(),
            }
        }
    }
}

/// Attempt to obtain a GitHub App installation token from the config.
///
/// Returns a [`TokenResolution`] that tells callers exactly which identity
/// is being used and why, instead of silently falling back to `None`.
///
/// This is a thin wrapper around [`resolve_named_app_token`] with `name = None`.
pub fn resolve_app_token(config: &Config, context: &str) -> TokenResolution {
    resolve_named_app_token(config, None, context)
}

/// Expand `~` at the start of a path to the user's home directory.
fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shellexpand_tilde() {
        let expanded = shellexpand_tilde("~/some/path");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("some/path"));
    }

    #[test]
    fn test_shellexpand_no_tilde() {
        assert_eq!(shellexpand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(shellexpand_tilde("relative/path"), "relative/path");
    }

    #[test]
    fn test_jwt_generation_bad_key() {
        let config = GitHubAppConfig {
            app_id: 12345,
            client_id: None,
            private_key_path: "/nonexistent/key.pem".to_string(),
            installation_id: 67890,
        };
        let result = generate_jwt(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to read"));
    }

    #[test]
    fn test_token_resolution_token_returns_value_for_app_token() {
        let res = TokenResolution::AppToken("ghp_abc123".to_string());
        assert_eq!(res.token(), Some("ghp_abc123"));
        assert!(!res.is_fallback());
    }

    #[test]
    fn test_token_resolution_token_returns_none_for_not_configured() {
        let res = TokenResolution::NotConfigured;
        assert_eq!(res.token(), None);
        assert!(!res.is_fallback());
    }

    #[test]
    fn test_token_resolution_token_returns_none_for_fallback() {
        let res = TokenResolution::Fallback {
            reason: "API unreachable".to_string(),
        };
        assert_eq!(res.token(), None);
        assert!(res.is_fallback());
    }

    #[test]
    fn test_resolve_app_token_not_configured() {
        let config = Config::default();
        let res = resolve_app_token(&config, "test");
        assert_eq!(res, TokenResolution::NotConfigured);
    }

    #[test]
    fn test_resolve_named_app_token_uses_named_app() {
        let mut config = Config::default();
        config.github.apps.insert(
            "developer".to_string(),
            GitHubAppConfig {
                app_id: 11111,
                client_id: None,
                private_key_path: "/nonexistent/dev.pem".to_string(),
                installation_id: 22222,
            },
        );
        let res = resolve_named_app_token(&config, Some("developer"), "test");
        // No real key, so should return Fallback (not NotConfigured)
        assert!(res.is_fallback());
    }

    #[test]
    fn test_resolve_named_app_token_falls_back_to_singleton() {
        // Named app not configured, but singleton is
        let mut config = Config::default();
        config.github.app = Some(GitHubAppConfig {
            app_id: 99999,
            client_id: None,
            private_key_path: "/nonexistent/singleton.pem".to_string(),
            installation_id: 88888,
        });
        let res = resolve_named_app_token(&config, Some("developer"), "test");
        // Named "developer" not in apps, falls back to singleton → Fallback (bad key)
        assert!(res.is_fallback());
    }

    #[test]
    fn test_resolve_named_app_token_not_configured_when_nothing() {
        let config = Config::default();
        let res = resolve_named_app_token(&config, Some("developer"), "test");
        assert_eq!(res, TokenResolution::NotConfigured);
    }

    #[test]
    fn test_resolve_app_token_bad_key_returns_fallback() {
        let mut config = Config::default();
        config.github.app = Some(GitHubAppConfig {
            app_id: 12345,
            client_id: None,
            private_key_path: "/nonexistent/key.pem".to_string(),
            installation_id: 67890,
        });
        let res = resolve_app_token(&config, "test");
        assert!(res.is_fallback());
        assert_eq!(res.token(), None);
        if let TokenResolution::Fallback { reason } = &res {
            assert!(reason.contains("failed to read"));
        } else {
            panic!("expected Fallback variant");
        }
    }
}
