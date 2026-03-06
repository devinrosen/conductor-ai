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
            eprintln!(
                "warning: github.app.client_id {:?} does not match the expected \
                 GitHub App client ID format (\"Iv...\"). \
                 Verify the value on your App's settings page.",
                cid
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
/// Uses `gh api` with the JWT as an explicit `Authorization: Bearer` header to
/// call the `POST /app/installations/{installation_id}/access_tokens` endpoint.
///
/// Note: `GH_TOKEN` / `--auth-token` make `gh` send `Authorization: token <value>`,
/// which is the PAT format. GitHub App JWT auth requires `Authorization: Bearer <jwt>`,
/// so we override the header directly.
fn exchange_installation_token(app_config: &GitHubAppConfig, jwt: &str) -> Result<String> {
    let url = format!(
        "app/installations/{}/access_tokens",
        app_config.installation_id
    );

    let auth_header = format!("Authorization: Bearer {jwt}");
    let output =
        crate::github::build_gh_cmd(&["api", "--method", "POST", "-H", &auth_header, &url], None)
            .output()
            .map_err(|e| {
                ConductorError::TicketSync(format!(
                    "failed to exchange GitHub App installation token: {e}"
                ))
            })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConductorError::TicketSync(format!(
            "GitHub App token exchange failed: {stderr}"
        )));
    }

    let resp: InstallationTokenResponse = serde_json::from_slice(&output.stdout).map_err(|e| {
        ConductorError::TicketSync(format!("failed to parse installation token response: {e}"))
    })?;

    Ok(resp.token)
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

/// Attempt to obtain a GitHub App installation token from the config.
/// Returns `None` (graceful fallback) if the app is not configured or
/// token generation fails.
pub fn resolve_app_token(config: &Config, context: &str) -> Option<String> {
    let app_config = config.github.app.as_ref()?;
    match get_app_token(app_config) {
        Ok(token) => Some(token),
        Err(e) => {
            eprintln!("[{context}] GitHub App token failed, falling back to gh user: {e}");
            None
        }
    }
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
}
