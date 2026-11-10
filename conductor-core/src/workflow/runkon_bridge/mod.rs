//! Bridge adapters between `conductor-core` types and `runkon-flow` traits.
//!
//! This module converts between the two type universes so that
//! `execute_workflow_standalone` can delegate to `runkon_flow::FlowEngine::run()`.
//!
//! All items are `pub(super)` — visible to the parent `workflow` module only.

use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use crate::error::ConductorError;

mod action_executor_adapter;
mod child_workflow_runner;
mod item_providers;
mod script_env;
mod turn_completion;

pub(super) use action_executor_adapter::RkActionExecutorAdapter;
pub(super) use child_workflow_runner::{ConductorChildWorkflowRunner, FreshChildren};
pub(super) use item_providers::{
    RkReposItemProvider, RkTicketsItemProvider, RkWorkflowRunsItemProvider, RkWorktreesItemProvider,
};
pub(super) use script_env::ConductorScriptEnvProvider;

/// Convert `ConductorError` to `EngineError`, preserving the cancellation
/// signal: `WorkflowCancelled` maps to `Cancelled`, all other errors to
/// `Workflow`.  Centralising this avoids the special-case match being
/// copy-pasted at every `map_err` site.
impl From<ConductorError> for EngineError {
    fn from(e: ConductorError) -> Self {
        match e {
            ConductorError::WorkflowCancelled => EngineError::Cancelled(
                runkon_flow::cancellation_reason::CancellationReason::UserRequested(None),
            ),
            other => EngineError::Workflow(other.to_string()),
        }
    }
}

/// Wraps conductor-core's `ClaudeAgentExecutor` behind the runkon-flow
/// `ActionExecutor` trait.
pub(super) fn bridge_lock_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Workflow(format!("db mutex poisoned: {e}"))
}

/// The agent and prompt root runkon searches: conductor's repos keep them under
/// `.conductor/`, overriding runkon's `.runkon` default. Every runkon call that
/// loads an agent or prompt takes this, so the root is defined once.
pub(super) fn conductor_config_dir() -> runkon_flow_executors::ConfigDir {
    runkon_flow_executors::ConfigDir::new(".conductor")
        .expect("`.conductor` is a valid plain relative directory name")
}

/// Wrap a `ConductorError` from a child-workflow execute/resume call into
/// an `EngineError`, preserving cancellation passthrough so a child cancel
/// propagates as a cancellation rather than a generic workflow failure.
pub(super) fn wrap_child_workflow_err(e: ConductorError, ctx: String) -> EngineError {
    match e {
        ConductorError::WorkflowCancelled => EngineError::from(e),
        other => EngineError::Workflow(format!("{ctx}: {other}")),
    }
}

/// Build a runkon-flow `ActionRegistry` backed by a `RkActionExecutorAdapter`
/// as the catch-all fallback executor.
///
/// `runtime_override` is the workflow-run-level explicit runtime selection
/// (e.g. from the host's model picker). When `None`, the adapter falls back
/// to deriving a runtime from each step's `params.model`.
pub(super) fn build_rk_action_registry(
    config: &crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    db_path: &std::path::Path,
    runtime_override: Option<String>,
) -> runkon_flow::traits::action_executor::ActionRegistry {
    let adapter = RkActionExecutorAdapter::new(
        config.clone(),
        conn,
        db_path.to_path_buf(),
        runtime_override,
    );
    runkon_flow::traits::action_executor::ActionRegistry::from_executors(
        std::collections::HashMap::new(),
        Some(Box::new(adapter)),
    )
}

/// Build the action registry of a smoke run: the catch-all is a
/// [`crate::workflow::smoke::SmokeStopActionExecutor`], so no agent can start.
pub(super) fn build_rk_smoke_action_registry(
    smoke: crate::workflow::SmokeStop,
) -> runkon_flow::traits::action_executor::ActionRegistry {
    runkon_flow::traits::action_executor::ActionRegistry::from_executors(
        std::collections::HashMap::new(),
        Some(Box::new(
            crate::workflow::smoke::SmokeStopActionExecutor::new(smoke),
        )),
    )
}

/// Build a validation-only `ItemProviderRegistry` with all four built-in providers.
///
/// Uses an in-memory SQLite connection so the providers can be instantiated for
/// metadata queries (`parse_scope`, `requires_filter`, `validate_filter`,
/// `supports_ordered`) without requiring a real database.  The `items()` method
/// on these providers is never called during validation.
pub(super) fn build_rk_validation_registry(
) -> runkon_flow::traits::item_provider::ItemProviderRegistry {
    let conn = Arc::new(Mutex::new(
        rusqlite::Connection::open_in_memory().expect("validation registry in-memory db"),
    ));
    let config = crate::config::Config::default();
    build_rk_item_provider_registry(conn, &config, None)
}

/// The one list of built-in item providers. Runs `$body` once per provider with
/// `$p` bound to it, so a run's registry and the validation engine's builder
/// can't disagree on what is registered.
macro_rules! for_each_rk_item_provider {
    ($conn:expr, $config:expr, $repo_id:expr, |$p:ident| $body:expr) => {{
        let $p = RkTicketsItemProvider::new(Arc::clone(&$conn), $config.clone(), $repo_id.clone());
        $body;
        let $p = RkReposItemProvider::new(Arc::clone(&$conn), $config.clone());
        $body;
        let $p = RkWorkflowRunsItemProvider::new(Arc::clone(&$conn), $config.clone());
        $body;
        let $p =
            RkWorktreesItemProvider::new(Arc::clone(&$conn), $config.clone(), $repo_id.clone());
        $body;
    }};
}

/// Build a runkon-flow `ItemProviderRegistry` with all four built-in providers.
pub(super) fn build_rk_item_provider_registry(
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: &crate::config::Config,
    repo_id: Option<String>,
) -> runkon_flow::traits::item_provider::ItemProviderRegistry {
    let mut registry = runkon_flow::traits::item_provider::ItemProviderRegistry::new();
    for_each_rk_item_provider!(conn, config, repo_id, |p| registry.register(p));
    registry
}

/// Register the four built-in item providers on an engine builder.
///
/// Shares its provider list with [`build_rk_item_provider_registry`]; the
/// validation engine uses this one because a builder owns its own registry.
pub(super) fn with_rk_item_providers(
    builder: runkon_flow::FlowEngineBuilder,
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: &crate::config::Config,
    repo_id: Option<String>,
) -> runkon_flow::FlowEngineBuilder {
    let mut builder = builder;
    for_each_rk_item_provider!(conn, config, repo_id, |p| builder =
        builder.item_provider(p));
    builder
}

/// Build a `ScriptEnvProvider` for use with runkon-flow.
///
/// Uses `ConductorScriptEnvProvider` so that script steps inherit:
/// - the conductor binary directory and any extra plugin directories on `PATH`
/// - a `GH_TOKEN` resolved from a per-step `as = "..."` (or workflow-level
///   default bot) when the named bot is configured under `[github.apps.<name>]`
///
/// `config` is wrapped in an `Arc` so the provider can stay alive past the
/// caller's stack frame and resolve a fresh installation token per script
/// step (tokens have a 1-hour lifetime, so re-resolving each call is fine).
pub(super) fn build_rk_script_env_provider(
    conductor_bin_dir: Option<std::path::PathBuf>,
    extra_plugin_dirs: Vec<String>,
    config: Arc<crate::config::Config>,
    owner: Option<String>,
) -> Arc<dyn runkon_flow::traits::script_env_provider::ScriptEnvProvider> {
    Arc::new(ConductorScriptEnvProvider::new(
        conductor_bin_dir,
        extra_plugin_dirs,
        config,
        owner,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conductor_error_workflow_cancelled_becomes_engine_cancelled() {
        let err: EngineError = ConductorError::WorkflowCancelled.into();
        assert!(
            matches!(err, EngineError::Cancelled(_)),
            "WorkflowCancelled should map to EngineError::Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn conductor_error_other_becomes_engine_workflow() {
        let err: EngineError = ConductorError::Workflow("some error".to_string()).into();
        assert!(
            matches!(err, EngineError::Workflow(_)),
            "non-cancellation error should map to EngineError::Workflow, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("some error"),
            "error message should be preserved: {msg}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::conductor_config_dir;
    use runkon_flow_executors::agent_loader::{load_agent, load_and_concat_snippets};

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn agent(desc: &str) -> String {
        format!("---\nrole: reviewer\n---\n{desc}\n")
    }

    #[test]
    fn agents_resolve_workflow_local_then_shared_then_worktree_before_repo() {
        let wt = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let (w, r) = (wt.path(), repo.path());
        let load = || {
            load_agent(
                &w.to_string_lossy(),
                &r.to_string_lossy(),
                "x",
                Some("wf"),
                &conductor_config_dir(),
                &[],
                None,
            )
            .map(|a| a.prompt)
        };
        assert!(load().is_err());
        write(r, ".conductor/agents/x.md", &agent("repo-shared"));
        assert!(load().unwrap().contains("repo-shared"));
        write(w, ".conductor/agents/x.md", &agent("wt-shared"));
        assert!(load().unwrap().contains("wt-shared"));
        write(r, ".conductor/workflows/wf/agents/x.md", &agent("repo-local"));
        assert!(load().unwrap().contains("repo-local"));
        write(w, ".conductor/workflows/wf/agents/x.md", &agent("wt-local"));
        assert!(load().unwrap().contains("wt-local"));
    }

    #[test]
    fn snippets_resolve_from_conductor_prompts() {
        let wt = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), ".conductor/prompts/s.md", "repo-snippet");
        let out = load_and_concat_snippets(
            &wt.path().to_string_lossy(),
            &repo.path().to_string_lossy(),
            &["s".to_string()],
            Some("wf"),
            &conductor_config_dir(),
        )
        .unwrap();
        assert!(out.contains("repo-snippet"));
        write(wt.path(), ".conductor/prompts/s.md", "wt-snippet");
        let out = load_and_concat_snippets(
            &wt.path().to_string_lossy(),
            &repo.path().to_string_lossy(),
            &["s".to_string()],
            Some("wf"),
            &conductor_config_dir(),
        )
        .unwrap();
        assert!(out.contains("wt-snippet"));
    }
}
