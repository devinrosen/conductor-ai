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
    /// Returns the optional value argument that follows the generic permission flag.
    ///
    /// This is the only permission-mode method retained in the portable crate because
    /// the headless arg builder (`push_optional_agent_flags`) needs it. All other
    /// vendor-specific flag mappings live in the host crate (conductor-core).
    pub fn cli_flag_value(&self) -> Option<&str> {
        match self {
            Self::Plan => Some("plan"),
            Self::RepoSafe => Some("repo-safe"),
            _ => None,
        }
    }
}
