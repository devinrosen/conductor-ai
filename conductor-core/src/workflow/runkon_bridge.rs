//! Bridge adapters between `conductor-core` types and `runkon-flow` traits.
//!
//! This module converts between the two type universes so that
//! `execute_workflow_standalone` can delegate to `runkon_flow::FlowEngine::run()`.
//!
//! All items are `pub(super)` — visible to the parent `workflow` module only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use crate::error::ConductorError;
use crate::workflow::item_provider::ItemProvider;

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

// ---------------------------------------------------------------------------
// 2. RkActionExecutorAdapter
// ---------------------------------------------------------------------------

/// Wraps conductor-core's `ClaudeAgentExecutor` behind the runkon-flow
/// `ActionExecutor` trait.
fn bridge_lock_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Workflow(format!("db mutex poisoned: {e}"))
}

/// Wrap a `ConductorError` from a child-workflow execute/resume call into
/// an `EngineError`, preserving cancellation passthrough so a child cancel
/// propagates as a cancellation rather than a generic workflow failure.
fn wrap_child_workflow_err(e: ConductorError, ctx: String) -> EngineError {
    match e {
        ConductorError::WorkflowCancelled => EngineError::from(e),
        other => EngineError::Workflow(format!("{ctx}: {other}")),
    }
}

///
/// The runkon-flow `ExecutionContext` does not carry `db_path`, so we store it
/// in the adapter and inject it when constructing the portable `ClaudeAgentContext`.
pub(super) struct RkActionExecutorAdapter {
    config: crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    db_path: std::path::PathBuf,
}

impl RkActionExecutorAdapter {
    pub(super) fn new(
        config: crate::config::Config,
        conn: Arc<Mutex<rusqlite::Connection>>,
        db_path: std::path::PathBuf,
    ) -> Self {
        Self {
            config,
            conn,
            db_path,
        }
    }
}

impl runkon_flow::traits::action_executor::ActionExecutor for RkActionExecutorAdapter {
    fn name(&self) -> &str {
        "__rk_claude_agent__"
    }

    fn execute(
        &self,
        ctx: &dyn runkon_flow::traits::run_context::RunContext,
        info: &runkon_flow::traits::action_executor::StepInfo,
        params: &runkon_flow::traits::action_executor::ActionParams,
    ) -> Result<runkon_flow::traits::action_executor::ActionOutput, EngineError> {
        // ClaudeAgentExecutor needs a pre-created agent_runs row ID as `run_id` so
        // it can track the subprocess. The step↔run link (child_run_id on the step
        // row) is written here — before execution starts — so the TUI can show live
        // agent output while the step is in flight. The engine also sets child_run_id
        // post-execution via ActionOutput, which is a no-op thanks to COALESCE.
        let child_run_id = {
            let conn = self.conn.lock().map_err(bridge_lock_err)?;
            let agent_mgr = crate::agent::AgentManager::new(&conn);
            let child_run = agent_mgr
                .create_child_run(
                    ctx.get(crate::workflow::engine_keys::WORKTREE_ID)
                        .as_deref(),
                    &format!("Workflow step: {}", params.name),
                    params.model.as_deref(),
                    ctx.parent_run_id().unwrap_or(""),
                    params.bot_name.as_deref(),
                )
                .map_err(|e| {
                    EngineError::Workflow(format!(
                        "step '{}': failed to create child agent run: {e}",
                        params.name
                    ))
                })?;

            if !info.step_id.is_empty() {
                // Best-effort pre-execution link so the TUI can show live agent output
                // while the step is running. The ActionOutput written by the executor
                // after execution completes is the authoritative source of child_run_id.
                if let Err(e) =
                    crate::workflow::update_step_child_run_id(&conn, &info.step_id, &child_run.id)
                {
                    tracing::warn!(
                        "step '{}' (step_id={}): failed to link child_run_id {}: {e}",
                        params.name,
                        info.step_id,
                        child_run.id,
                    );
                }
            }

            child_run.id
        };

        // Build per-step RuntimeOptions — max_turns is step-level so the resolver is fresh per call.
        let options = runkon_runtimes::RuntimeOptions {
            binary_path: crate::agent_runtime::resolve_conductor_bin().into(),
            log_path_for_run: std::sync::Arc::new(|run_id: &str| {
                crate::config::agent_log_path(run_id)
                    .unwrap_or_else(|_| std::env::temp_dir().join(format!("{run_id}.log")))
            }),
            workspace_root: self.config.general.workspace_root.clone(),
            argv_builder: crate::agent_runtime::conductor_argv_builder(),
            stall_threshold: Some(crate::agent_runtime::DEFAULT_STALL_THRESHOLD),
            max_turns: Some(
                params
                    .max_turns
                    .unwrap_or(crate::agent_runtime::DEFAULT_MAX_TURNS),
            ),
        };
        let resolver = std::sync::Arc::new(crate::runtime::adapter::ConductorRuntimeResolver {
            permission_mode: self
                .config
                .general
                .agent_permission_mode
                .to_runtime_permission_mode(),
            runtimes: self.config.runtimes.clone(),
            options,
        });

        let host_adapter = std::sync::Arc::new(
            crate::runtime::adapter::SqliteHostAdapter::new(self.db_path.clone())
                .map_err(|e| EngineError::Workflow(e.to_string()))?,
        );

        let agent_ctx = runkon_flow_executors::claude_agent::ClaudeAgentContext {
            run_id: child_run_id.clone(),
            working_dir: ctx.working_dir().to_path_buf(),
            repo_path: ctx
                .get(crate::workflow::engine_keys::REPO_PATH)
                .unwrap_or_default(),
            step_timeout: info.step_timeout,
            shutdown: ctx.shutdown().cloned(),
            model: params.model.clone(),
            bot_name: params.bot_name.clone(),
            plugin_dirs: params.plugin_dirs.clone(),
            workflow_name: ctx.workflow_name().to_string(),
            tracker: host_adapter.clone() as std::sync::Arc<dyn runkon_runtimes::RunTracker>,
            event_sink: host_adapter as std::sync::Arc<dyn runkon_runtimes::RunEventSink>,
        };
        let schema_arc = params
            .extensions
            .get::<crate::schema_config::OutputSchema>();
        let agent_params = runkon_flow_executors::claude_agent::ClaudeAgentParams {
            name: &params.name,
            inputs: &params.inputs,
            snippet_refs: &params.snippets,
            dry_run: params.dry_run,
            retry_error: params.retry_error.as_deref(),
            schema: schema_arc.as_deref(),
        };

        let inner = runkon_flow_executors::claude_agent::ClaudeAgentExecutor::new(
            resolver,
            self.config.anthropic_api_key(),
        );
        let mut output = inner
            .execute(&agent_ctx, &agent_params)
            .map_err(EngineError::Workflow)?;
        output.child_run_id = Some(child_run_id);
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// 4. RkItemProvider adapters
// ---------------------------------------------------------------------------

/// Convert a conductor-core `FanOutItem` to a runkon-flow `FanOutItem`.
fn core_fan_out_item_to_rk(
    item: crate::workflow::item_provider::FanOutItem,
) -> runkon_flow::traits::item_provider::FanOutItem {
    runkon_flow::traits::item_provider::FanOutItem {
        item_type: item.item_type,
        item_id: item.item_id,
        item_ref: item.item_ref,
        context: item.context,
    }
}

/// Shared body for every `RkItemProvider::items()` implementation.
///
/// Locks the connection, delegates to `provider`, and maps the result back into
/// runkon-flow types.  All four adapters differ only in which `ItemProvider`
/// implementation they pass here.
fn delegate_items<P: ItemProvider>(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    config: &crate::config::Config,
    scope: Option<&dyn std::any::Any>,
    filter: &HashMap<String, String>,
    provider: P,
) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
    let guard = conn.lock().map_err(bridge_lock_err)?;
    let core_ctx = crate::workflow::item_provider::ProviderContext {
        conn: &guard,
        config,
    };
    provider
        .items(&core_ctx, scope, filter)
        .map(|items: Vec<crate::workflow::item_provider::FanOutItem>| {
            items.into_iter().map(core_fan_out_item_to_rk).collect()
        })
        .map_err(EngineError::from)
}

// ---------------------------------------------------------------------------
// 4. Rk*ItemProvider — one struct per item source, generated by macro.
//
// Both variants share the same ItemProvider impl body (delegate_items call).
// The only differences are: struct name, name() literal, optional repo_id
// field, and the inner provider constructed inside delegate_items.
// ---------------------------------------------------------------------------

// Shared ItemProvider trait impl — called from both branches of impl_rk_item_provider!.
// Each branch adds a private `fn provider(&self)` method to the struct so the inner
// provider can be obtained via `self.provider()` inside method bodies without needing
// `self` to appear in macro argument tokens (which Rust macro hygiene forbids).
macro_rules! impl_rk_item_provider_trait {
    ($name:ident, $provider_name:literal) => {
        impl runkon_flow::traits::item_provider::ItemProvider for $name {
            fn name(&self) -> &str {
                $provider_name
            }
            fn parse_scope(
                &self,
                raw: Option<&HashMap<String, String>>,
            ) -> Result<Option<Box<dyn std::any::Any>>, String> {
                self.provider().parse_scope(raw).map_err(|e| e.to_string())
            }
            fn scope_warnings(&self, raw: Option<&HashMap<String, String>>) -> Vec<String> {
                self.provider().scope_warnings(raw)
            }
            fn requires_filter(&self) -> bool {
                self.provider().requires_filter()
            }
            fn validate_filter(&self, filter: &HashMap<String, String>) -> Result<(), String> {
                self.provider()
                    .validate_filter(filter)
                    .map_err(|e| e.to_string())
            }
            fn items(
                &self,
                _ctx: &dyn runkon_flow::traits::run_context::RunContext,
                _info: &runkon_flow::traits::item_provider::ProviderInfo,
                scope: Option<&dyn std::any::Any>,
                filter: &HashMap<String, String>,
            ) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
                delegate_items(&self.conn, &self.config, scope, filter, self.provider())
            }
            fn supports_ordered(&self) -> bool {
                self.provider().supports_ordered()
            }
            fn dependencies(&self, step_id: &str) -> Result<Vec<(String, String)>, EngineError> {
                let guard = self.conn.lock().map_err(bridge_lock_err)?;
                self.provider()
                    .dependencies(&guard, &self.config, step_id)
                    .map_err(EngineError::from)
            }
        }
    };
}

macro_rules! impl_rk_item_provider {
    // Variant: no extra field — inner provider needs no self data.
    ($name:ident, $provider_name:literal, $inner:expr) => {
        pub(super) struct $name {
            conn: Arc<Mutex<rusqlite::Connection>>,
            config: crate::config::Config,
        }
        impl $name {
            pub(super) fn new(
                conn: Arc<Mutex<rusqlite::Connection>>,
                config: crate::config::Config,
            ) -> Self {
                Self { conn, config }
            }
            fn provider(&self) -> impl ItemProvider {
                $inner
            }
        }
        impl_rk_item_provider_trait!($name, $provider_name);
    };
    // Variant: with repo_id — `$make_provider` is a closure `|repo_id| <expr>` that
    // receives `self.repo_id` and returns a provider.  Using a closure avoids any
    // direct reference to `self` at the macro call site (which is not a method scope).
    ($name:ident, $provider_name:literal, repo_id, $make_provider:expr) => {
        pub(super) struct $name {
            conn: Arc<Mutex<rusqlite::Connection>>,
            config: crate::config::Config,
            repo_id: Option<String>,
        }
        impl $name {
            pub(super) fn new(
                conn: Arc<Mutex<rusqlite::Connection>>,
                config: crate::config::Config,
                repo_id: Option<String>,
            ) -> Self {
                Self {
                    conn,
                    config,
                    repo_id,
                }
            }
            fn provider(&self) -> impl ItemProvider {
                ($make_provider)(self.repo_id.clone())
            }
        }
        impl_rk_item_provider_trait!($name, $provider_name);
    };
}

impl_rk_item_provider!(
    RkTicketsItemProvider,
    "tickets",
    repo_id,
    crate::workflow::item_provider::tickets::TicketsProvider::new
);

impl_rk_item_provider!(
    RkReposItemProvider,
    "repos",
    crate::workflow::item_provider::repos::ReposProvider
);

impl_rk_item_provider!(
    RkWorkflowRunsItemProvider,
    "workflow_runs",
    crate::workflow::item_provider::workflow_runs::WorkflowRunsProvider
);

// WorktreesProvider requires repo_id; worktree_id is not available in this context.
impl_rk_item_provider!(RkWorktreesItemProvider, "worktrees", repo_id, |repo_id| {
    crate::workflow::item_provider::worktrees::WorktreesProvider::new(repo_id, None)
});

// ---------------------------------------------------------------------------
// 5. ConductorChildWorkflowRunner
// ---------------------------------------------------------------------------

/// Implements `runkon_flow::engine::ChildWorkflowRunner` by delegating to
/// conductor-core's `execute_workflow` / `resume_workflow` functions.
pub(super) struct ConductorChildWorkflowRunner {
    db_path: std::path::PathBuf,
    config: crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    /// Cached from the parent run at construction time to avoid a per-child DB round-trip.
    target_label: Option<String>,
    triggered_by_hook: bool,
}

impl ConductorChildWorkflowRunner {
    pub(super) fn new(
        db_path: std::path::PathBuf,
        config: crate::config::Config,
        conn: Arc<Mutex<rusqlite::Connection>>,
        target_label: Option<String>,
        triggered_by_hook: bool,
    ) -> Self {
        Self {
            db_path,
            config,
            conn,
            target_label,
            triggered_by_hook,
        }
    }

    /// Build the `WorkflowExecStandalone` params for a new child workflow run.
    ///
    /// Extracted for unit-testability: the regression test in
    /// `tests::child_standalone_reads_ticket_repo_from_run_ctx` verifies that
    /// `ticket_id` and `repo_id` are read from `run_ctx` (not `inputs`), so
    /// resumed runs whose stored inputs no longer carry those keys still
    /// propagate the right identity values to child workflows.
    fn build_child_standalone_params(
        &self,
        workflow: runkon_flow::dsl::WorkflowDef,
        parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
        params: runkon_flow::engine::ChildWorkflowInput,
    ) -> crate::workflow::types::WorkflowExecStandalone {
        let exec_config = crate::workflow::WorkflowExecConfig {
            event_sinks: parent_ctx.event_sinks.iter().cloned().collect(),
            ..parent_ctx.exec_config.clone()
        };
        crate::workflow::types::WorkflowExecStandalone {
            config: self.config.clone(),
            workflow,
            worktree_id: parent_ctx
                .run_ctx
                .get(crate::workflow::engine_keys::WORKTREE_ID),
            working_dir: parent_ctx.run_ctx.working_dir_str(),
            repo_path: parent_ctx
                .run_ctx
                .get(crate::workflow::engine_keys::REPO_PATH)
                .unwrap_or_default(),
            ticket_id: parent_ctx
                .run_ctx
                .get(crate::workflow::engine_keys::TICKET_ID),
            repo_id: parent_ctx
                .run_ctx
                .get(crate::workflow::engine_keys::REPO_ID),
            model: parent_ctx.model.clone(),
            exec_config,
            inputs: params.inputs,
            target_label: self.target_label.clone(),
            run_id_notify: None,
            triggered_by_hook: self.triggered_by_hook,
            conductor_bin_dir: None,
            force: false,
            extra_plugin_dirs: parent_ctx.extra_plugin_dirs.clone(),
            db_path: Some(self.db_path.clone()),
            parent_workflow_run_id: Some(parent_ctx.workflow_run_id.clone()),
            depth: params.depth,
            parent_step_id: params.parent_step_id,
            default_bot_name: params.bot_name,
            iteration: params.iteration,
        }
    }

    /// Project a parent's `ChildWorkflowContext` into the `WorkflowResumeInput`
    /// that `super::coordinator::resume_workflow` consumes.
    ///
    /// Extracted so the `event_sinks` propagation is unit-testable without
    /// spinning up a real workflow run — see the regression test in
    /// `tests::resume_input_propagates_event_sinks_from_parent_ctx` which
    /// guards against `event_sinks: vec![]` re-creeping back in.
    fn build_resume_input<'a>(
        &'a self,
        workflow_run_id: &'a str,
        model: Option<&'a str>,
        parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
    ) -> crate::workflow::types::WorkflowResumeInput<'a> {
        crate::workflow::types::WorkflowResumeInput {
            config: &self.config,
            workflow_run_id,
            model,
            from_step: None,
            restart: false,
            conductor_bin_dir: None,
            event_sinks: parent_ctx.event_sinks.iter().cloned().collect(),
            db_path: Some(self.db_path.clone()),
            shutdown: None,
        }
    }
}

impl runkon_flow::engine::ChildWorkflowRunner for ConductorChildWorkflowRunner {
    fn execute_child(
        &self,
        workflow_name: &str,
        parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
        params: runkon_flow::engine::ChildWorkflowInput,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        // Load the real workflow definition from disk. The runner resolves the
        // actual definition by name from the worktree/repo .conductor/workflows/ directory.
        let parent_working_dir = parent_ctx.run_ctx.working_dir_str();
        let parent_repo_path = parent_ctx
            .run_ctx
            .get(crate::workflow::engine_keys::REPO_PATH)
            .unwrap_or_default();
        let wf_dirs = crate::workflow::manager::definitions::workflow_dirs(
            &parent_working_dir,
            &parent_repo_path,
        );
        let wf_dir_refs: Vec<&std::path::Path> = wf_dirs.iter().map(|p| p.as_path()).collect();
        let core_def = runkon_flow::dsl::load_workflow_by_name(&wf_dir_refs, workflow_name)
            .map_err(|e| {
                EngineError::Workflow(format!(
                    "failed to load sub-workflow '{}': {e}",
                    workflow_name
                ))
            })?;

        // Route child workflows through execute_workflow_standalone so they use
        // FlowEngine::run() — keeping event emission and step tracking consistent
        // between parent and child runs.
        let standalone_params = self.build_child_standalone_params(core_def, parent_ctx, params);

        let core_result = super::coordinator::execute_workflow_standalone(&standalone_params)
            .map_err(|e| {
                wrap_child_workflow_err(e, format!("child workflow '{workflow_name}' failed"))
            })?;

        Ok(core_result)
    }

    fn resume_child(
        &self,
        workflow_run_id: &str,
        model: Option<&str>,
        parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        let input = self.build_resume_input(workflow_run_id, model, parent_ctx);

        let core_result = super::coordinator::resume_workflow(&input).map_err(|e| {
            wrap_child_workflow_err(
                e,
                format!("failed to resume child workflow run '{workflow_run_id}'"),
            )
        })?;

        Ok(core_result)
    }

    fn find_resumable_child(
        &self,
        parent_run_id: &str,
        workflow_name: &str,
    ) -> runkon_flow::engine_error::Result<Option<runkon_flow::types::WorkflowRun>> {
        let conn = self.conn.lock().map_err(bridge_lock_err)?;
        let core_run =
            crate::workflow::find_resumable_child_run(&conn, parent_run_id, workflow_name)
                .map_err(|e| {
                    EngineError::Workflow(format!(
                        "failed to find resumable child run for parent='{parent_run_id}' workflow='{workflow_name}': {e}"
                    ))
                })?;

        Ok(core_run)
    }
}

// ---------------------------------------------------------------------------
// 6. Helper builder functions
// ---------------------------------------------------------------------------

/// Build a runkon-flow `ActionRegistry` backed by a `RkActionExecutorAdapter`
/// as the catch-all fallback executor.
pub(super) fn build_rk_action_registry(
    config: &crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    db_path: &std::path::Path,
) -> runkon_flow::traits::action_executor::ActionRegistry {
    let adapter = RkActionExecutorAdapter::new(config.clone(), conn, db_path.to_path_buf());
    runkon_flow::traits::action_executor::ActionRegistry::from_executors(
        HashMap::new(),
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
    Arc::new(
        crate::workflow::script_env_provider::ConductorScriptEnvProvider::new(
            conductor_bin_dir,
            extra_plugin_dirs,
            config,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ---------------------------------------------------------------------------
    // delegate_items — mutex poison propagates as EngineError
    // ---------------------------------------------------------------------------

    #[test]
    fn delegate_items_propagates_mutex_poison() {
        let conn = Arc::new(Mutex::new(
            rusqlite::Connection::open_in_memory().expect("in-memory db"),
        ));
        // Poison the mutex by panicking inside a lock guard in another thread.
        let conn_clone = Arc::clone(&conn);
        let _ = std::thread::spawn(move || {
            let _guard = conn_clone.lock().unwrap();
            panic!("intentional panic to poison mutex");
        })
        .join();

        let config = crate::config::Config::default();
        let result = delegate_items(
            &conn,
            &config,
            None,
            &HashMap::new(),
            crate::workflow::item_provider::repos::ReposProvider,
        );
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("mutex poisoned"),
                    "expected poison error, got: {msg}"
                );
            }
            Ok(_) => panic!("expected mutex-poison error, got Ok"),
        }
    }

    // ---------------------------------------------------------------------------
    // From<ConductorError> for EngineError — both branches
    // ---------------------------------------------------------------------------

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

    // ---------------------------------------------------------------------------
    // Fix 9: dependencies method via a concrete Rk item-provider adapter
    // ---------------------------------------------------------------------------

    #[test]
    fn rk_repos_item_provider_dependencies_returns_empty_for_nonexistent_step() {
        use runkon_flow::traits::item_provider::ItemProvider as RkItemProvider;

        let conn = Arc::new(Mutex::new(crate::test_helpers::setup_db()));
        let config = crate::config::Config::default();
        let provider = RkReposItemProvider::new(Arc::clone(&conn), config);

        let result = RkItemProvider::dependencies(&provider, "nonexistent-step");
        assert!(result.is_ok(), "dependencies should not error: {result:?}");
        assert!(
            result.unwrap().is_empty(),
            "dependencies for nonexistent step should be empty"
        );
    }

    // ---------------------------------------------------------------------------
    // Regression test: resume_child must propagate parent_ctx.event_sinks into
    // WorkflowResumeInput. Prior bug: `event_sinks: vec![]` silently dropped
    // step events on resumed child workflows.
    // ---------------------------------------------------------------------------

    #[test]
    fn resume_input_propagates_event_sinks_from_parent_ctx() {
        use runkon_flow::engine::ChildWorkflowContext;
        use runkon_flow::events::{EngineEventData, EventSink};

        struct CountingSink;
        impl EventSink for CountingSink {
            fn emit(&self, _: &EngineEventData) {}
        }

        let conn = Arc::new(Mutex::new(crate::test_helpers::setup_db()));
        let runner = ConductorChildWorkflowRunner::new(
            std::path::PathBuf::from("/tmp/test.db"),
            crate::config::Config::default(),
            conn,
            None,
            false,
        );

        let sinks: Arc<[Arc<dyn EventSink>]> = Arc::from(vec![
            Arc::new(CountingSink) as Arc<dyn EventSink>,
            Arc::new(CountingSink) as Arc<dyn EventSink>,
        ]);

        let parent_ctx = ChildWorkflowContext {
            run_ctx: std::sync::Arc::new(runkon_flow::traits::run_context::NoopRunContext::default())
                as std::sync::Arc<dyn runkon_flow::traits::run_context::RunContext>,
            extra_plugin_dirs: vec![],
            workflow_run_id: "parent-run".to_string(),
            model: None,
            exec_config: crate::workflow::WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            event_sinks: Arc::clone(&sinks),
        };

        let input = runner.build_resume_input("child-run-1", None, &parent_ctx);

        assert_eq!(
            input.event_sinks.len(),
            2,
            "event_sinks must be propagated from parent_ctx; \
             regression check for prior `event_sinks: vec![]` bug"
        );
        assert_eq!(input.workflow_run_id, "child-run-1");
    }

    // ---------------------------------------------------------------------------
    // Regression test: build_child_standalone_params must read ticket_id and
    // repo_id from run_ctx, NOT from parent_ctx.inputs.
    //
    // Bug: before the fix these two fields still used inputs.get("ticket_id") /
    // inputs.get("repo_id"), so resumed workflows whose stored inputs no longer
    // carried those keys would spawn children with ticket_id/repo_id = None.
    // ---------------------------------------------------------------------------

    #[test]
    fn child_standalone_reads_ticket_repo_from_run_ctx() {
        use runkon_flow::engine::{ChildWorkflowContext, ChildWorkflowInput};
        use runkon_flow::events::EventSink;

        // run_ctx carries the identity values; inputs intentionally empty.
        let mut vars = std::collections::HashMap::new();
        vars.insert(crate::workflow::engine_keys::TICKET_ID, "t-abc".to_string());
        vars.insert(crate::workflow::engine_keys::REPO_ID, "r-def".to_string());
        let run_ctx = runkon_flow::traits::run_context::NoopRunContext::with_vars(vars);

        let conn = Arc::new(Mutex::new(crate::test_helpers::setup_db()));
        let runner = ConductorChildWorkflowRunner::new(
            std::path::PathBuf::from("/tmp/test.db"),
            crate::config::Config::default(),
            conn,
            None,
            false,
        );

        let parent_ctx = ChildWorkflowContext {
            run_ctx: std::sync::Arc::new(run_ctx)
                as std::sync::Arc<dyn runkon_flow::traits::run_context::RunContext>,
            extra_plugin_dirs: vec![],
            workflow_run_id: "parent-run".to_string(),
            model: None,
            exec_config: crate::workflow::WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            event_sinks: Arc::<[Arc<dyn EventSink>]>::from(vec![]),
        };

        let workflow = runkon_flow::test_helpers::make_def("test-child", vec![]);
        let params = ChildWorkflowInput {
            inputs: HashMap::new(),
            iteration: 0,
            bot_name: None,
            depth: 1,
            parent_step_id: None,
            cancellation: runkon_flow::CancellationToken::new(),
        };

        let standalone = runner.build_child_standalone_params(workflow, &parent_ctx, params);

        assert_eq!(
            standalone.ticket_id,
            Some("t-abc".to_string()),
            "ticket_id must come from run_ctx, not inputs"
        );
        assert_eq!(
            standalone.repo_id,
            Some("r-def".to_string()),
            "repo_id must come from run_ctx, not inputs"
        );
    }

    #[test]
    fn resume_input_with_empty_parent_sinks_yields_empty_sinks() {
        use runkon_flow::engine::ChildWorkflowContext;
        use runkon_flow::events::EventSink;

        let conn = Arc::new(Mutex::new(crate::test_helpers::setup_db()));
        let runner = ConductorChildWorkflowRunner::new(
            std::path::PathBuf::from("/tmp/test.db"),
            crate::config::Config::default(),
            conn,
            None,
            false,
        );

        let parent_ctx = ChildWorkflowContext {
            run_ctx: std::sync::Arc::new(runkon_flow::traits::run_context::NoopRunContext::default())
                as std::sync::Arc<dyn runkon_flow::traits::run_context::RunContext>,
            extra_plugin_dirs: vec![],
            workflow_run_id: "parent-run".to_string(),
            model: None,
            exec_config: crate::workflow::WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            event_sinks: Arc::<[Arc<dyn EventSink>]>::from(vec![]),
        };

        let input = runner.build_resume_input("child-run-2", None, &parent_ctx);
        assert!(input.event_sinks.is_empty());
    }
}
