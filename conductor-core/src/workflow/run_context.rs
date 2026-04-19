use std::collections::HashMap;
use std::path::Path;

use super::engine::{ExecutionState, ENGINE_INJECTED_KEYS};

/// Abstraction over the per-run context consumed by executors and prompt builders.
///
/// `WorktreeRunContext` is the concrete implementation backed by `ExecutionState`.
/// The trait exists as a seam so future runtimes (Gemini CLI, script-only runs, etc.)
/// can provide their own context without carrying the full `ExecutionState` shape.
#[allow(dead_code)]
pub(crate) trait RunContext {
    /// Returns the subset of variables that the engine injects from run metadata
    /// (ticket and repo fields, plus `workflow_run_id`). Keys are `&'static str`
    /// (from `ENGINE_INJECTED_KEYS`) so callers can insert directly into a
    /// `HashMap<&'static str, String>` without an extra key-recovery scan.
    fn injected_variables(&self) -> HashMap<&'static str, String>;

    /// Absolute path to the working directory for this run.
    fn working_dir(&self) -> &Path;

    /// Absolute path to the repository root for this run.
    fn repo_path(&self) -> &Path;

    /// Worktree ID, if this run is tied to a registered worktree.
    fn worktree_id(&self) -> Option<&str>;

    /// Worktree slug (empty string for repo-level runs).
    fn worktree_slug(&self) -> &str;

    /// Ticket ID linked to this run, if any.
    fn ticket_id(&self) -> Option<&str>;

    /// Repo ID for this run, if any.
    fn repo_id(&self) -> Option<&str>;

    /// Directory containing the conductor binary (for PATH injection in script steps).
    fn conductor_bin_dir(&self) -> Option<&Path>;

    /// Additional plugin directories passed via `--plugin-dir`.
    fn extra_plugin_dirs(&self) -> &[String];

    /// Environment variables to pass to script steps.
    ///
    /// Default implementation prepends `conductor_bin_dir` to PATH if set.
    fn script_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Some(bin_dir) = self.conductor_bin_dir() {
            let existing_path = std::env::var("PATH").unwrap_or_default();
            env.insert(
                "PATH".to_string(),
                format!("{}:{}", bin_dir.display(), existing_path),
            );
        }
        env
    }
}

/// `RunContext` implementation backed by an `ExecutionState`.
///
/// Holds a shared reference to `ExecutionState` and delegates to its fields.
/// No data is owned; all field lifetimes come from the borrowed state.
pub(crate) struct WorktreeRunContext<'s, 'conn> {
    state: &'s ExecutionState<'conn>,
}

impl<'s, 'conn> WorktreeRunContext<'s, 'conn> {
    // pub(in crate::workflow): accessible throughout the workflow module tree,
    // including executors and manager helpers.
    pub(in crate::workflow) fn new(state: &'s ExecutionState<'conn>) -> Self {
        WorktreeRunContext { state }
    }
}

impl RunContext for WorktreeRunContext<'_, '_> {
    fn injected_variables(&self) -> HashMap<&'static str, String> {
        let mut map = HashMap::new();
        for &key in ENGINE_INJECTED_KEYS {
            if key == "workflow_run_id" {
                map.insert(key, self.state.workflow_run_id.clone());
            } else if let Some(v) = self.state.inputs.get(key) {
                map.insert(key, v.clone());
            }
        }
        map
    }

    fn working_dir(&self) -> &Path {
        Path::new(&self.state.worktree_ctx.working_dir)
    }

    fn repo_path(&self) -> &Path {
        Path::new(&self.state.worktree_ctx.repo_path)
    }

    fn worktree_id(&self) -> Option<&str> {
        self.state.worktree_ctx.worktree_id.as_deref()
    }

    fn worktree_slug(&self) -> &str {
        &self.state.worktree_ctx.worktree_slug
    }

    fn ticket_id(&self) -> Option<&str> {
        self.state.worktree_ctx.ticket_id.as_deref()
    }

    fn repo_id(&self) -> Option<&str> {
        self.state.worktree_ctx.repo_id.as_deref()
    }

    fn conductor_bin_dir(&self) -> Option<&Path> {
        self.state.worktree_ctx.conductor_bin_dir.as_deref()
    }

    fn extra_plugin_dirs(&self) -> &[String] {
        &self.state.worktree_ctx.extra_plugin_dirs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::engine::{WorktreeContext, ENGINE_INJECTED_KEYS};

    fn make_state_with_all_injected(conn: &rusqlite::Connection) -> ExecutionState<'_> {
        let config: &'static crate::config::Config =
            Box::leak(Box::new(crate::config::Config::default()));
        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "tid-001".to_string());
        inputs.insert("ticket_source_id".to_string(), "GH-42".to_string());
        inputs.insert("ticket_source_type".to_string(), "github".to_string());
        inputs.insert("ticket_title".to_string(), "Fix the bug".to_string());
        inputs.insert("ticket_body".to_string(), "Body text".to_string());
        inputs.insert(
            "ticket_url".to_string(),
            "https://github.com/org/repo/issues/42".to_string(),
        );
        inputs.insert(
            "ticket_raw_json".to_string(),
            r#"{"number":42}"#.to_string(),
        );
        inputs.insert("repo_id".to_string(), "repo-abc".to_string());
        inputs.insert("repo_path".to_string(), "/home/user/repo".to_string());
        inputs.insert("repo_name".to_string(), "my-repo".to_string());
        ExecutionState {
            workflow_run_id: "wfrun-xyz".to_string(),
            workflow_name: "test-wf".to_string(),
            worktree_ctx: WorktreeContext {
                working_dir: "/home/user/repo/worktree".to_string(),
                repo_path: "/home/user/repo".to_string(),
                worktree_id: None,
                worktree_slug: String::new(),
                ticket_id: None,
                repo_id: None,
                conductor_bin_dir: None,
                extra_plugin_dirs: vec![],
            },
            inputs,
            ..crate::workflow::tests::common::base_execution_state(
                conn,
                config,
                String::new(),
                String::new(),
            )
        }
    }

    #[test]
    fn test_all_engine_injected_keys_round_trip() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state_with_all_injected(&conn);
        let ctx = WorktreeRunContext::new(&state);
        let vars = ctx.injected_variables();

        for &key in ENGINE_INJECTED_KEYS {
            assert!(
                vars.contains_key(key),
                "injected_variables() missing key: {key}"
            );
        }
    }

    #[test]
    fn test_injected_variables_ticket_fields() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state_with_all_injected(&conn);
        let ctx = WorktreeRunContext::new(&state);
        let vars = ctx.injected_variables();

        assert_eq!(vars.get("ticket_id").map(String::as_str), Some("tid-001"));
        assert_eq!(
            vars.get("ticket_source_id").map(String::as_str),
            Some("GH-42")
        );
        assert_eq!(
            vars.get("ticket_source_type").map(String::as_str),
            Some("github")
        );
        assert_eq!(
            vars.get("ticket_title").map(String::as_str),
            Some("Fix the bug")
        );
        assert_eq!(
            vars.get("ticket_body").map(String::as_str),
            Some("Body text")
        );
        assert_eq!(
            vars.get("ticket_url").map(String::as_str),
            Some("https://github.com/org/repo/issues/42")
        );
        assert_eq!(
            vars.get("ticket_raw_json").map(String::as_str),
            Some(r#"{"number":42}"#)
        );
    }

    #[test]
    fn test_injected_variables_repo_fields() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state_with_all_injected(&conn);
        let ctx = WorktreeRunContext::new(&state);
        let vars = ctx.injected_variables();

        assert_eq!(vars.get("repo_id").map(String::as_str), Some("repo-abc"));
        assert_eq!(
            vars.get("repo_path").map(String::as_str),
            Some("/home/user/repo")
        );
        assert_eq!(vars.get("repo_name").map(String::as_str), Some("my-repo"));
    }

    #[test]
    fn test_injected_variables_workflow_run_id_from_struct_field() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state_with_all_injected(&conn);
        let ctx = WorktreeRunContext::new(&state);
        let vars = ctx.injected_variables();

        assert_eq!(
            vars.get("workflow_run_id").map(String::as_str),
            Some("wfrun-xyz")
        );
    }

    #[test]
    fn test_injected_variables_excludes_non_injected_inputs() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state_with_all_injected(&conn);
        state
            .inputs
            .insert("custom_var".to_string(), "custom_val".to_string());
        let ctx = WorktreeRunContext::new(&state);
        let vars = ctx.injected_variables();

        assert!(
            !vars.contains_key("custom_var"),
            "injected_variables() should not include non-injected inputs"
        );
    }

    #[test]
    fn test_injected_variables_absent_keys_not_inserted() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state_with_all_injected(&conn);
        state.workflow_run_id = "wf-empty".to_string();
        state.inputs.clear();
        let ctx = WorktreeRunContext::new(&state);
        let vars = ctx.injected_variables();

        assert_eq!(vars.len(), 1);
        assert_eq!(
            vars.get("workflow_run_id").map(String::as_str),
            Some("wf-empty")
        );
    }

    #[test]
    fn test_working_dir() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state_with_all_injected(&conn);
        let ctx = WorktreeRunContext::new(&state);
        assert_eq!(
            ctx.working_dir(),
            std::path::Path::new("/home/user/repo/worktree")
        );
    }

    #[test]
    fn test_script_env_empty_when_no_bin_dir() {
        let conn = crate::test_helpers::create_test_conn();
        let state = make_state_with_all_injected(&conn);
        let ctx = WorktreeRunContext::new(&state);
        assert!(ctx.script_env().is_empty());
    }

    #[test]
    fn test_script_env_injects_path_when_bin_dir_set() {
        let conn = crate::test_helpers::create_test_conn();
        let mut state = make_state_with_all_injected(&conn);
        state.worktree_ctx.conductor_bin_dir =
            Some(std::path::PathBuf::from("/usr/local/bin/conductor-dir"));
        let ctx = WorktreeRunContext::new(&state);
        let env = ctx.script_env();
        let path = env.get("PATH").expect("PATH should be set");
        assert!(
            path.starts_with("/usr/local/bin/conductor-dir:"),
            "PATH should start with conductor_bin_dir, got: {path}"
        );
    }
}
