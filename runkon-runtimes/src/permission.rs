use serde::{Deserialize, Serialize};

/// Controls which permission flag is passed to Claude Code when launching agent runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Use `--enable-auto-mode` (may prompt for permissions in headless agents).
    AutoMode,
    /// Use `--dangerously-skip-permissions` (default for headless agent runs).
    #[default]
    SkipPermissions,
    /// Use `--permission-mode plan` (read-only mode for repo-scoped agents).
    Plan,
    /// Use `--dangerously-skip-permissions` + `--allowedTools` read-safe pattern.
    RepoSafe,
}

impl PermissionMode {
    /// Returns the conductor CLI flag for this mode (used in `conductor agent run` passthrough args).
    pub fn cli_flag(&self) -> &str {
        match self {
            Self::AutoMode => "--enable-auto-mode",
            Self::SkipPermissions => "--dangerously-skip-permissions",
            Self::Plan => "--permission-mode",
            Self::RepoSafe => "--permission-mode",
        }
    }

    /// Returns the optional value argument that follows the conductor CLI flag.
    pub fn cli_flag_value(&self) -> Option<&str> {
        match self {
            Self::Plan => Some("plan"),
            Self::RepoSafe => Some("repo-safe"),
            _ => None,
        }
    }

    /// Returns the actual permission flag to pass to the `claude` subprocess.
    pub fn claude_permission_flag(&self) -> &str {
        match self {
            Self::AutoMode => "--enable-auto-mode",
            Self::SkipPermissions => "--dangerously-skip-permissions",
            Self::Plan => "--permission-mode",
            Self::RepoSafe => "--dangerously-skip-permissions",
        }
    }

    /// Returns the optional value argument that follows the claude permission flag.
    pub fn claude_permission_flag_value(&self) -> Option<&str> {
        match self {
            Self::Plan => Some("plan"),
            _ => None,
        }
    }

    /// Returns the `--allowedTools` pattern for this mode, if any.
    pub fn allowed_tools(&self) -> Option<&'static str> {
        match self {
            Self::Plan | Self::RepoSafe => {
                Some("Bash,Glob,Grep,Read,WebFetch,WebSearch,mcp__conductor__*,mcp__*")
            }
            _ => None,
        }
    }
}
