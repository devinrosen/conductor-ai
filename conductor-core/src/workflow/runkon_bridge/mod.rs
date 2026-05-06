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

pub(super) use action_executor_adapter::RkActionExecutorAdapter;
pub(super) use child_workflow_runner::ConductorChildWorkflowRunner;
pub(super) use item_providers::{
    RkReposItemProvider, RkTicketsItemProvider, RkWorkflowRunsItemProvider,
    RkWorktreesItemProvider,
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
pub(super) fn build_rk_action_registry(
    config: &crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    db_path: &std::path::Path,
) -> runkon_flow::traits::action_executor::ActionRegistry {
    let adapter = RkActionExecutorAdapter::new(config.clone(), conn, db_path.to_path_buf());
    runkon_flow::traits::action_executor::ActionRegistry::from_executors(
        std::collections::HashMap::new(),
        Some(Box::new(adapter)),
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

/// Build a runkon-flow `ItemProviderRegistry` with all four built-in providers.
pub(super) fn build_rk_item_provider_registry(
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: &crate::config::Config,
    repo_id: Option<String>,
) -> runkon_flow::traits::item_provider::ItemProviderRegistry {
    let mut registry = runkon_flow::traits::item_provider::ItemProviderRegistry::new();

    registry.register(RkTicketsItemProvider::new(
        Arc::clone(&conn),
        config.clone(),
        repo_id.clone(),
    ));
    registry.register(RkReposItemProvider::new(Arc::clone(&conn), config.clone()));
    registry.register(RkWorkflowRunsItemProvider::new(
        Arc::clone(&conn),
        config.clone(),
    ));
    registry.register(RkWorktreesItemProvider::new(
        Arc::clone(&conn),
        config.clone(),
        repo_id,
    ));

    registry
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
) -> Arc<dyn runkon_flow::traits::script_env_provider::ScriptEnvProvider> {
    Arc::new(ConductorScriptEnvProvider::new(
        conductor_bin_dir,
        extra_plugin_dirs,
        config,
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
