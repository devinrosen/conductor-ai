use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// Opaque permission mode forwarded through to the headless agent CLI.
///
/// `runkon-runtimes` deliberately does not encode any specific permission
/// taxonomy (Claude's `plan` / `repo-safe`, etc.). Vendor-specific values
/// live in the host crate (e.g. conductor-core's `AgentPermissionMode`) and
/// are converted into this opaque form when constructing a `RuntimeRequest`.
///
/// `Default` produces no permission flag value; `Other(s)` forwards `s` as-is
/// as the value argument to whichever permission flag the host adds to the
/// CLI invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    /// No permission flag value forwarded.
    #[default]
    Default,
    /// Forward this raw flag value to the headless arg builder.
    Other(Cow<'static, str>),
}

impl PermissionMode {
    /// Returns the optional value argument that follows the host-specific
    /// permission flag in the headless CLI invocation. `None` means no
    /// permission flag is appended to the args.
    pub fn cli_flag_value(&self) -> Option<&str> {
        match self {
            Self::Default => None,
            Self::Other(s) => Some(s.as_ref()),
        }
    }
}
