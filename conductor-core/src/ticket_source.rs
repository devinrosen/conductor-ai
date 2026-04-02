use crate::error::{ConductorError, Result};
use crate::github;
use crate::issue_source::{GitHubConfig, IssueSource, JiraConfig};
use crate::jira_acli;
use crate::tickets::TicketInput;

/// Typed dispatch for ticket sources.
///
/// Collapses all `match source.source_type.as_str()` dispatch sites to a single
/// `TicketSource::from_issue_source(&source)?` call. Adding a new ticket source
/// only requires updating this enum and its `impl` block.
pub enum TicketSource {
    GitHub(GitHubConfig),
    Jira(JiraConfig),
}

impl TicketSource {
    /// Construct a `TicketSource` from a stored `IssueSource` record.
    ///
    /// Returns `UnknownSourceType` if `source.source_type` is not recognised.
    pub fn from_issue_source(s: &IssueSource) -> Result<Self> {
        match s.source_type.as_str() {
            "github" => {
                let cfg = serde_json::from_str::<GitHubConfig>(&s.config_json).map_err(|e| {
                    ConductorError::TicketSync(format!("invalid github config: {e}"))
                })?;
                Ok(Self::GitHub(cfg))
            }
            "jira" => {
                let cfg = serde_json::from_str::<JiraConfig>(&s.config_json)
                    .map_err(|e| ConductorError::TicketSync(format!("invalid jira config: {e}")))?;
                Ok(Self::Jira(cfg))
            }
            other => Err(ConductorError::UnknownSourceType(other.to_string())),
        }
    }

    /// Sync all tickets for this source.
    ///
    /// `token` is an optional auth token passed to GitHub syncs; Jira ignores it.
    pub fn sync(&self, token: Option<&str>) -> Result<Vec<TicketInput>> {
        match self {
            Self::GitHub(cfg) => github::sync_github_issues(&cfg.owner, &cfg.repo, token),
            Self::Jira(cfg) => jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url),
        }
    }

    /// Fetch a single ticket by its source-specific ID string.
    ///
    /// For GitHub the `source_id` is an issue number; for Jira it is an issue key.
    pub fn fetch_one(&self, source_id: &str) -> Result<TicketInput> {
        match self {
            Self::GitHub(cfg) => {
                let issue_number: i64 = source_id.parse().map_err(|_| {
                    ConductorError::InvalidInput(format!(
                        "invalid GitHub issue number: {source_id}"
                    ))
                })?;
                github::fetch_github_issue(&cfg.owner, &cfg.repo, issue_number, None)
            }
            Self::Jira(cfg) => jira_acli::fetch_jira_issue(source_id, &cfg.url),
        }
    }

    /// Returns the canonical source-type string (`"github"` / `"jira"`).
    ///
    /// Used when passing `source_type` to `sync_and_close_tickets`.
    pub fn source_type_str(&self) -> &'static str {
        match self {
            Self::GitHub(_) => "github",
            Self::Jira(_) => "jira",
        }
    }

    /// Validate and/or infer a config JSON string for the given source type.
    ///
    /// - `"github"` with `None`: auto-infers `{"owner":…,"repo":…}` from `remote_url`.
    /// - `"github"` with `Some(json)`: validates it parses as JSON and returns it.
    /// - `"jira"` with `None`: returns an error (config is required).
    /// - `"jira"` with `Some(json)`: validates and returns it.
    /// - Any other type: returns `UnknownSourceType`.
    pub fn default_config(
        source_type: &str,
        config_json: Option<&str>,
        remote_url: &str,
    ) -> Result<String> {
        match (source_type, config_json) {
            ("github", Some(json)) => {
                serde_json::from_str::<serde_json::Value>(json).map_err(|e| {
                    ConductorError::InvalidInput(format!("invalid JSON config: {e}"))
                })?;
                Ok(json.to_string())
            }
            ("github", None) => {
                let (owner, repo) = github::parse_github_remote(remote_url).ok_or_else(|| {
                    ConductorError::InvalidInput(format!(
                        "cannot infer GitHub config from remote URL: {remote_url}. \
                         Use --config to specify manually."
                    ))
                })?;
                serde_json::to_string(&GitHubConfig { owner, repo }).map_err(|e| {
                    ConductorError::Config(format!("failed to serialize github config: {e}"))
                })
            }
            ("jira", Some(json)) => {
                serde_json::from_str::<serde_json::Value>(json).map_err(|e| {
                    ConductorError::InvalidInput(format!("invalid JSON config: {e}"))
                })?;
                Ok(json.to_string())
            }
            ("jira", None) => Err(ConductorError::InvalidInput(
                "--config is required for jira sources \
                 (e.g. --config '{\"jql\":\"project = KEY AND status != Done\",\"url\":\"https://...\"}')"
                    .to_string(),
            )),
            (other, _) => Err(ConductorError::UnknownSourceType(other.to_string())),
        }
    }
}
