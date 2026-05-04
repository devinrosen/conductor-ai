use std::collections::HashMap;
use std::path::{Path, PathBuf};

use runkon_flow::traits::run_context::{keys, RunContext};

/// Production `RunContext` implementation for conductor worktree runs.
///
/// Carries all conductor-domain fields (`repo_path`, `worktree_id`, `ticket_id`,
/// `repo_id`) via the `injected_variables()` map, keyed by the `keys` constants.
/// The internal map is built once at construction so `get()` is O(1) without
/// allocating a full HashMap per call.
pub(crate) struct WorktreeRunContext {
    working_dir: PathBuf,
    injected: HashMap<&'static str, String>,
}

impl WorktreeRunContext {
    pub(crate) fn new(
        working_dir: impl Into<PathBuf>,
        repo_path: impl Into<String>,
        worktree_id: Option<String>,
        ticket_id: Option<String>,
        repo_id: Option<String>,
    ) -> Self {
        let mut injected = HashMap::new();
        injected.insert(keys::REPO_PATH, repo_path.into());
        if let Some(id) = worktree_id {
            injected.insert(keys::WORKTREE_ID, id);
        }
        if let Some(id) = ticket_id {
            injected.insert(keys::TICKET_ID, id);
        }
        if let Some(id) = repo_id {
            injected.insert(keys::REPO_ID, id);
        }
        Self {
            working_dir: working_dir.into(),
            injected,
        }
    }
}

impl RunContext for WorktreeRunContext {
    fn injected_variables(&self) -> HashMap<&'static str, String> {
        self.injected.clone()
    }

    fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn get(&self, key: &str) -> Option<String> {
        self.injected.get(key).cloned()
    }
}
