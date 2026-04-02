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
#[derive(Debug)]
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
            ("github" | "jira", Some(json)) => {
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
            ("jira", None) => Err(ConductorError::InvalidInput(
                "--config is required for jira sources \
                 (e.g. --config '{\"jql\":\"project = KEY AND status != Done\",\"url\":\"https://...\"}')"
                    .to_string(),
            )),
            (other, _) => Err(ConductorError::UnknownSourceType(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue_source(source_type: &str, config_json: &str) -> IssueSource {
        IssueSource {
            id: "test-id".to_string(),
            repo_id: "test-repo".to_string(),
            source_type: source_type.to_string(),
            config_json: config_json.to_string(),
        }
    }

    // --- from_issue_source ---

    #[test]
    fn from_issue_source_valid_github() {
        let src = make_issue_source("github", r#"{"owner":"acme","repo":"widget"}"#);
        let ts = TicketSource::from_issue_source(&src).unwrap();
        match ts {
            TicketSource::GitHub(cfg) => {
                assert_eq!(cfg.owner, "acme");
                assert_eq!(cfg.repo, "widget");
            }
            _ => panic!("expected GitHub variant"),
        }
    }

    #[test]
    fn from_issue_source_valid_jira() {
        let src = make_issue_source(
            "jira",
            r#"{"jql":"project = FOO AND status != Done","url":"https://acme.atlassian.net"}"#,
        );
        let ts = TicketSource::from_issue_source(&src).unwrap();
        match ts {
            TicketSource::Jira(cfg) => {
                assert_eq!(cfg.jql, "project = FOO AND status != Done");
                assert_eq!(cfg.url, "https://acme.atlassian.net");
            }
            _ => panic!("expected Jira variant"),
        }
    }

    #[test]
    fn from_issue_source_invalid_github_config() {
        let src = make_issue_source("github", "not-json");
        let err = TicketSource::from_issue_source(&src).unwrap_err();
        match err {
            ConductorError::TicketSync(msg) => {
                assert!(
                    msg.contains("invalid github config"),
                    "unexpected msg: {msg}"
                );
            }
            _ => panic!("expected TicketSync error, got {err:?}"),
        }
    }

    #[test]
    fn from_issue_source_invalid_jira_config() {
        let src = make_issue_source("jira", "{bad json}");
        let err = TicketSource::from_issue_source(&src).unwrap_err();
        match err {
            ConductorError::TicketSync(msg) => {
                assert!(msg.contains("invalid jira config"), "unexpected msg: {msg}");
            }
            _ => panic!("expected TicketSync error, got {err:?}"),
        }
    }

    #[test]
    fn from_issue_source_unknown_source_type() {
        let src = make_issue_source("linear", r#"{}"#);
        let err = TicketSource::from_issue_source(&src).unwrap_err();
        match err {
            ConductorError::UnknownSourceType(t) => assert_eq!(t, "linear"),
            _ => panic!("expected UnknownSourceType error, got {err:?}"),
        }
    }

    // --- default_config ---

    #[test]
    fn default_config_github_with_valid_json() {
        let json = r#"{"owner":"acme","repo":"widget"}"#;
        let result = TicketSource::default_config("github", Some(json), "").unwrap();
        assert_eq!(result, json);
    }

    #[test]
    fn default_config_jira_with_valid_json() {
        let json = r#"{"jql":"project = FOO","url":"https://acme.atlassian.net"}"#;
        let result = TicketSource::default_config("jira", Some(json), "").unwrap();
        assert_eq!(result, json);
    }

    #[test]
    fn default_config_github_with_invalid_json() {
        let err = TicketSource::default_config("github", Some("not-json"), "").unwrap_err();
        match err {
            ConductorError::InvalidInput(msg) => {
                assert!(msg.contains("invalid JSON config"), "unexpected msg: {msg}");
            }
            _ => panic!("expected InvalidInput error, got {err:?}"),
        }
    }

    #[test]
    fn default_config_jira_with_invalid_json() {
        let err = TicketSource::default_config("jira", Some("{bad}"), "").unwrap_err();
        match err {
            ConductorError::InvalidInput(msg) => {
                assert!(msg.contains("invalid JSON config"), "unexpected msg: {msg}");
            }
            _ => panic!("expected InvalidInput error, got {err:?}"),
        }
    }

    #[test]
    fn default_config_github_no_config_valid_remote_https() {
        let result =
            TicketSource::default_config("github", None, "https://github.com/acme/widget.git")
                .unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["owner"], "acme");
        assert_eq!(v["repo"], "widget");
    }

    #[test]
    fn default_config_github_no_config_valid_remote_ssh() {
        let result =
            TicketSource::default_config("github", None, "git@github.com:acme/widget.git").unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["owner"], "acme");
        assert_eq!(v["repo"], "widget");
    }

    #[test]
    fn default_config_github_no_config_invalid_remote() {
        let err = TicketSource::default_config("github", None, "not-a-url").unwrap_err();
        match err {
            ConductorError::InvalidInput(msg) => {
                assert!(
                    msg.contains("cannot infer GitHub config"),
                    "unexpected msg: {msg}"
                );
            }
            _ => panic!("expected InvalidInput error, got {err:?}"),
        }
    }

    #[test]
    fn default_config_jira_no_config_returns_error() {
        let err = TicketSource::default_config("jira", None, "").unwrap_err();
        match err {
            ConductorError::InvalidInput(msg) => {
                assert!(
                    msg.contains("--config is required for jira"),
                    "unexpected msg: {msg}"
                );
            }
            _ => panic!("expected InvalidInput error, got {err:?}"),
        }
    }

    #[test]
    fn default_config_unknown_source_type() {
        let err = TicketSource::default_config("linear", Some("{}"), "").unwrap_err();
        match err {
            ConductorError::UnknownSourceType(t) => assert_eq!(t, "linear"),
            _ => panic!("expected UnknownSourceType error, got {err:?}"),
        }
    }
}
