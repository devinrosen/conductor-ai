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

#[cfg(test)]
mod tests {
    use super::*;
    use runkon_flow::traits::run_context::keys;

    #[test]
    fn always_inserts_repo_path() {
        let ctx = WorktreeRunContext::new("/work", "/repo/path", None, None, None);
        assert_eq!(ctx.get(keys::REPO_PATH).as_deref(), Some("/repo/path"));
    }

    #[test]
    fn none_optionals_are_absent() {
        let ctx = WorktreeRunContext::new("/work", "/repo", None, None, None);
        assert!(ctx.get(keys::WORKTREE_ID).is_none());
        assert!(ctx.get(keys::TICKET_ID).is_none());
        assert!(ctx.get(keys::REPO_ID).is_none());
    }

    #[test]
    fn some_optionals_are_present() {
        let ctx = WorktreeRunContext::new(
            "/work",
            "/repo",
            Some("wt-01".into()),
            Some("tk-99".into()),
            Some("repo-42".into()),
        );
        assert_eq!(ctx.get(keys::WORKTREE_ID).as_deref(), Some("wt-01"));
        assert_eq!(ctx.get(keys::TICKET_ID).as_deref(), Some("tk-99"));
        assert_eq!(ctx.get(keys::REPO_ID).as_deref(), Some("repo-42"));
    }

    #[test]
    fn partial_optionals() {
        let ctx = WorktreeRunContext::new("/work", "/repo", Some("wt-01".into()), None, None);
        assert_eq!(ctx.get(keys::WORKTREE_ID).as_deref(), Some("wt-01"));
        assert!(ctx.get(keys::TICKET_ID).is_none());
        assert!(ctx.get(keys::REPO_ID).is_none());
    }

    #[test]
    fn working_dir_is_set() {
        let ctx = WorktreeRunContext::new("/my/work/dir", "/repo", None, None, None);
        assert_eq!(ctx.working_dir(), std::path::Path::new("/my/work/dir"));
    }

    #[test]
    fn injected_variables_matches_get() {
        let ctx = WorktreeRunContext::new(
            "/work",
            "/repo",
            Some("wt-1".into()),
            Some("tk-1".into()),
            Some("r-1".into()),
        );
        let map = ctx.injected_variables();
        assert_eq!(map.get(keys::REPO_PATH).map(String::as_str), Some("/repo"));
        assert_eq!(map.get(keys::WORKTREE_ID).map(String::as_str), Some("wt-1"));
        assert_eq!(map.get(keys::TICKET_ID).map(String::as_str), Some("tk-1"));
        assert_eq!(map.get(keys::REPO_ID).map(String::as_str), Some("r-1"));
    }
}
