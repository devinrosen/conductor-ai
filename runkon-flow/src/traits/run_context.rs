use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Abstraction over the per-run context consumed by executors and prompt builders.
///
/// # Stability
///
/// This trait is **alpha-unstable**. Method signatures may change in a minor
/// version while the crate is pre-1.0. Do not implement outside of this
/// workspace without pinning to an exact version.
pub trait RunContext: Send + Sync {
    /// Returns the subset of variables that the engine injects from run metadata.
    fn injected_variables(&self) -> HashMap<&'static str, String>;

    /// Absolute path to the working directory for this run.
    fn working_dir(&self) -> &Path;

    /// Working directory as an owned `String` (convenience over `to_string_lossy`).
    fn working_dir_str(&self) -> String {
        self.working_dir().to_string_lossy().into_owned()
    }

    /// Look up a single injected variable by key.
    ///
    /// Default impl calls `injected_variables()` and removes the matching value.
    /// Impls with a persistent internal map (e.g. `WorktreeRunContext`) should
    /// override this to avoid the full map allocation.
    fn get(&self, key: &str) -> Option<String> {
        self.injected_variables().remove(key)
    }
}

/// Conventional key constants for values injected by the workflow engine.
///
/// Use these instead of bare string literals so that typos are caught at compile
/// time and renaming a key produces a single diff rather than a grep-and-replace.
pub mod keys {
    pub const REPO_PATH: &str = "repo_path";
    pub const WORKTREE_ID: &str = "worktree_id";
    pub const TICKET_ID: &str = "ticket_id";
    pub const REPO_ID: &str = "repo_id";
    pub const WORKING_DIR: &str = "working_dir";
    pub const WORKFLOW_RUN_ID: &str = "workflow_run_id";
}

/// No-op `RunContext` implementation for tests.
///
/// Returns an empty injected-variables map and `/tmp` as the working directory
/// by default. Use [`NoopRunContext::with_vars`] to inject specific values.
///
/// Only available when the `test-utils` feature is enabled or in `#[cfg(test)]`
/// contexts. Do not use in production code.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Default)]
pub struct NoopRunContext {
    vars: HashMap<&'static str, String>,
    working_dir: PathBuf,
}

#[cfg(any(test, feature = "test-utils"))]
impl NoopRunContext {
    /// Build a `NoopRunContext` with the given variable overrides.
    pub fn with_vars(vars: HashMap<&'static str, String>) -> Self {
        Self {
            vars,
            working_dir: PathBuf::from("/tmp"),
        }
    }

    /// Set a specific working directory.
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = dir.into();
        self
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl RunContext for NoopRunContext {
    fn injected_variables(&self) -> HashMap<&'static str, String> {
        self.vars.clone()
    }

    fn working_dir(&self) -> &Path {
        if self.working_dir == PathBuf::default() {
            Path::new("/tmp")
        } else {
            &self.working_dir
        }
    }

    fn get(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }
}
