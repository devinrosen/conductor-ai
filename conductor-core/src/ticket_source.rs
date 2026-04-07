use crate::error::{ConductorError, Result};
use crate::github;
use crate::issue_source::{GitHubConfig, IssueSource, JiraConfig, VantageConfig};
use crate::jira_acli;
use crate::tickets::TicketInput;
use crate::vantage;

/// Typed dispatch for ticket sources.
///
/// Collapses all `match source.source_type.as_str()` dispatch sites to a single
/// `TicketSource::from_issue_source(&source)?` call. Adding a new ticket source
/// only requires updating this enum and its `impl` block.
#[derive(Debug)]
pub enum TicketSource {
    GitHub(GitHubConfig),
    Jira(JiraConfig),
    /// `(config, repo_slug)` — `repo_slug` filters deliverables by codebase on sync.
    /// Starts as `None`; call [`TicketSource::with_repo_slug`] before [`TicketSource::sync`].
    Vantage(VantageConfig, Option<String>),
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
            "vantage" => {
                let cfg = serde_json::from_str::<VantageConfig>(&s.config_json).map_err(|e| {
                    ConductorError::TicketSync(format!("invalid vantage config: {e}"))
                })?;
                Ok(Self::Vantage(cfg, None))
            }
            other => Err(ConductorError::UnknownSourceType(other.to_string())),
        }
    }

    /// Set the `repo_slug` used by Vantage syncs to filter deliverables by codebase.
    ///
    /// No-op for GitHub and Jira sources. Must be called before [`Self::sync`] on a
    /// Vantage source, otherwise sync returns an error.
    pub fn with_repo_slug(self, slug: &str) -> Self {
        match self {
            Self::Vantage(cfg, _) => Self::Vantage(cfg, Some(slug.to_string())),
            other => other,
        }
    }

    /// Sync all tickets for this source.
    ///
    /// `token` is an optional auth token passed to GitHub syncs; Jira/Vantage ignore it.
    /// For Vantage sources, call [`Self::with_repo_slug`] first to set the codebase filter.
    pub fn sync(&self, token: Option<&str>) -> Result<Vec<TicketInput>> {
        match self {
            Self::GitHub(cfg) => github::sync_github_issues(&cfg.owner, &cfg.repo, token),
            Self::Jira(cfg) => jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url),
            Self::Vantage(cfg, repo_slug) => {
                let slug = repo_slug.as_deref().ok_or_else(|| {
                    ConductorError::InvalidInput(
                        "Vantage sync requires a repo_slug; call with_repo_slug() before sync()"
                            .to_string(),
                    )
                })?;
                vantage::sync_vantage_deliverables(&cfg.project_id, &cfg.sdlc_root, slug)
            }
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
            Self::Vantage(cfg, _) => vantage::fetch_vantage_deliverable(source_id, &cfg.sdlc_root),
        }
    }

    /// Returns the canonical source-type string (`"github"` / `"jira"` / `"vantage"`).
    ///
    /// Used when passing `source_type` to `sync_and_close_tickets`.
    pub fn source_type_str(&self) -> &'static str {
        match self {
            Self::GitHub(_) => "github",
            Self::Jira(_) => "jira",
            Self::Vantage(_, _) => "vantage",
        }
    }

    /// Validate and/or infer a config JSON string for the given source type.
    ///
    /// - `"github"` with `None`: auto-infers `{"owner":…,"repo":…}` from `remote_url`.
    /// - `"github"` with `Some(json)`: validates it parses as JSON and returns it.
    /// - `"jira"` with `None`: returns an error (config is required).
    /// - `"jira"` with `Some(json)`: validates and returns it.
    /// - `"vantage"` with `None`: returns an error (config is required).
    /// - `"vantage"` with `Some(json)`: validates and returns it.
    /// - Any other type: returns `UnknownSourceType`.
    pub fn default_config(
        source_type: &str,
        config_json: Option<&str>,
        remote_url: &str,
    ) -> Result<String> {
        match (source_type, config_json) {
            ("github" | "jira" | "vantage", Some(json)) => {
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
            ("vantage", None) => Err(ConductorError::InvalidInput(
                "--config is required for vantage sources \
                 (e.g. --config '{\"project_id\":\"PROJ-001\",\"sdlc_root\":\"/path/to/sdlc\"}')"
                    .to_string(),
            )),
            (other, _) => Err(ConductorError::UnknownSourceType(other.to_string())),
        }
    }
}

/// Return the ticket IDs that the given ticket depends on, based on its source type.
///
/// Currently only Vantage deliverables carry dependency metadata inside `raw_json`.
/// Returns an empty vec for all other source types.
pub fn get_dependency_ids(raw_json: &str, source_type: &str) -> Vec<String> {
    match source_type {
        "vantage" => vantage::get_parent_deliverable_ids(raw_json),
        _ => vec![],
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
    fn from_issue_source_valid_vantage() {
        let src = make_issue_source(
            "vantage",
            r#"{"project_id":"PROJ-001","sdlc_root":"/path/to/sdlc"}"#,
        );
        let ts = TicketSource::from_issue_source(&src).unwrap();
        match ts {
            TicketSource::Vantage(cfg, slug) => {
                assert_eq!(cfg.project_id, "PROJ-001");
                assert_eq!(cfg.sdlc_root, "/path/to/sdlc");
                assert_eq!(slug, None, "repo_slug should default to None");
            }
            _ => panic!("expected Vantage variant"),
        }
    }

    #[test]
    fn from_issue_source_invalid_vantage_config() {
        let src = make_issue_source("vantage", "not-json");
        let err = TicketSource::from_issue_source(&src).unwrap_err();
        match err {
            ConductorError::TicketSync(msg) => {
                assert!(
                    msg.contains("invalid vantage config"),
                    "unexpected msg: {msg}"
                );
            }
            _ => panic!("expected TicketSync error, got {err:?}"),
        }
    }

    #[test]
    fn with_repo_slug_sets_slug_for_vantage() {
        let src = make_issue_source(
            "vantage",
            r#"{"project_id":"PROJ-001","sdlc_root":"/path"}"#,
        );
        let ts = TicketSource::from_issue_source(&src)
            .unwrap()
            .with_repo_slug("my-repo");
        match ts {
            TicketSource::Vantage(_, slug) => assert_eq!(slug, Some("my-repo".to_string())),
            _ => panic!("expected Vantage variant"),
        }
    }

    #[test]
    fn with_repo_slug_is_noop_for_github() {
        let src = make_issue_source("github", r#"{"owner":"acme","repo":"widget"}"#);
        let ts = TicketSource::from_issue_source(&src)
            .unwrap()
            .with_repo_slug("ignored");
        assert!(matches!(ts, TicketSource::GitHub(_)));
    }

    #[test]
    fn sync_vantage_without_repo_slug_returns_error() {
        let src = make_issue_source(
            "vantage",
            r#"{"project_id":"PROJ-001","sdlc_root":"/path"}"#,
        );
        let ts = TicketSource::from_issue_source(&src).unwrap();
        let err = ts.sync(None).err().expect("expected error");
        match err {
            ConductorError::InvalidInput(msg) => {
                assert!(msg.contains("repo_slug"), "unexpected msg: {msg}");
            }
            _ => panic!("expected InvalidInput error, got {err:?}"),
        }
    }

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
    fn default_config_vantage_with_valid_json() {
        let json = r#"{"project_id":"PROJ-001","sdlc_root":"/path/to/sdlc"}"#;
        let result = TicketSource::default_config("vantage", Some(json), "").unwrap();
        assert_eq!(result, json);
    }

    #[test]
    fn default_config_vantage_no_config_returns_error() {
        let err = TicketSource::default_config("vantage", None, "").unwrap_err();
        match err {
            ConductorError::InvalidInput(msg) => {
                assert!(
                    msg.contains("--config is required for vantage"),
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

    // --- Vantage variant (from main, deduplicated) ---

    #[test]
    fn source_type_str_vantage() {
        let src = make_issue_source(
            "vantage",
            r#"{"project_id":"PROJ-001","sdlc_root":"/path"}"#,
        );
        let ts = TicketSource::from_issue_source(&src).unwrap();
        assert_eq!(ts.source_type_str(), "vantage");
    }

    #[test]
    fn get_dependency_ids_delegates_to_vantage() {
        let json = serde_json::json!({ "id": "D-001", "dependencies": ["D-002", "D-003"] });
        let ids = super::get_dependency_ids(&serde_json::to_string(&json).unwrap(), "vantage");
        assert_eq!(ids, vec!["D-002", "D-003"]);
    }

    #[test]
    fn get_dependency_ids_empty_for_missing_field() {
        let json = serde_json::json!({ "id": "D-001" });
        let ids = super::get_dependency_ids(&serde_json::to_string(&json).unwrap(), "vantage");
        assert!(ids.is_empty());
    }

    #[test]
    fn get_dependency_ids_empty_for_non_vantage() {
        let json = serde_json::json!({ "id": "D-001", "dependencies": ["D-002"] });
        let ids = super::get_dependency_ids(&serde_json::to_string(&json).unwrap(), "github");
        assert!(ids.is_empty());
    }
}
