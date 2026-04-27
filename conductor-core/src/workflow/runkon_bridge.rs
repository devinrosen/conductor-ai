//! Bridge adapters between `conductor-core` types and `runkon-flow` traits.
//!
//! This module converts between the two type universes so that
//! `execute_workflow_standalone` can delegate to `runkon_flow::FlowEngine::run()`.
//!
//! All items are `pub(super)` — visible to the parent `workflow` module only.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use crate::error::ConductorError;
use crate::workflow::action_executor::ActionExecutor;
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
// 2. ActionOutput conversion helper (conductor-core → runkon-flow)
// ---------------------------------------------------------------------------

/// Convert a conductor-core `ActionOutput` into a `runkon_flow` `ActionOutput`.
///
/// The `child_run_id` field exists only in the runkon-flow type; it is set to
/// `None` because the conductor-core executor does not produce it directly.
pub(super) fn core_action_output_to_rk(
    core: crate::workflow::action_executor::ActionOutput,
) -> runkon_flow::traits::action_executor::ActionOutput {
    runkon_flow::traits::action_executor::ActionOutput {
        markers: core.markers,
        context: core.context,
        result_text: core.result_text,
        structured_output: core.structured_output,
        cost_usd: core.cost_usd,
        num_turns: core.num_turns,
        duration_ms: core.duration_ms,
        input_tokens: core.input_tokens,
        output_tokens: core.output_tokens,
        cache_read_input_tokens: core.cache_read_input_tokens,
        cache_creation_input_tokens: core.cache_creation_input_tokens,
        child_run_id: None,
    }
}

// ---------------------------------------------------------------------------
// 3. RkActionExecutorAdapter
// ---------------------------------------------------------------------------

/// Wraps conductor-core's `ClaudeAgentExecutor` behind the runkon-flow
/// `ActionExecutor` trait.
fn bridge_lock_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Workflow(format!("db mutex poisoned: {e}"))
}

///
/// The runkon-flow `ExecutionContext` does not carry `db_path`, so we store it
/// in the adapter and inject it when constructing the core `ExecutionContext`.
pub(super) struct RkActionExecutorAdapter {
    inner: crate::workflow::claude_agent_executor::ClaudeAgentExecutor,
    conn: Arc<Mutex<rusqlite::Connection>>,
    db_path: std::path::PathBuf,
}

impl RkActionExecutorAdapter {
    pub(super) fn new(
        config: crate::config::Config,
        conn: Arc<Mutex<rusqlite::Connection>>,
        db_path: std::path::PathBuf,
    ) -> Self {
        let api_executor: Box<dyn crate::workflow::action_executor::ActionExecutor> = Box::new(
            crate::workflow::api_call_executor::ApiCallExecutor::new(config.clone()),
        );
        Self {
            inner: crate::workflow::claude_agent_executor::ClaudeAgentExecutor::new(
                config,
                Some(api_executor),
            ),
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
        ectx: &runkon_flow::traits::action_executor::ExecutionContext,
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
                    ectx.worktree_id.as_deref(),
                    &format!("Workflow step: {}", params.name),
                    ectx.model.as_deref(),
                    &ectx.parent_run_id,
                    ectx.bot_name.as_deref(),
                )
                .map_err(|e| {
                    EngineError::Workflow(format!(
                        "step '{}': failed to create child agent run: {e}",
                        params.name
                    ))
                })?;

            if !ectx.step_id.is_empty() {
                let wf_mgr = crate::workflow::manager::WorkflowManager::new(&conn);
                // Best-effort pre-execution link so the TUI can show live agent output
                // while the step is running. The ActionOutput written by the executor
                // after execution completes is the authoritative source of child_run_id.
                if let Err(e) = wf_mgr.update_step_child_run_id(&ectx.step_id, &child_run.id) {
                    tracing::warn!(
                        "step '{}' (step_id={}): failed to link child_run_id {}: {e}",
                        params.name,
                        ectx.step_id,
                        child_run.id,
                    );
                }
            }

            child_run.id
        };

        // Convert runkon-flow ExecutionContext → conductor-core ExecutionContext,
        // injecting db_path which exists only in the conductor-core variant.
        // run_id is the freshly-created agent_run ID (not the workflow step ID).
        let core_ectx = crate::workflow::action_executor::ExecutionContext {
            run_id: child_run_id.clone(),
            working_dir: ectx.working_dir.clone(),
            repo_path: ectx.repo_path.clone(),
            db_path: self.db_path.clone(),
            step_timeout: ectx.step_timeout,
            shutdown: ectx.shutdown.clone(),
            model: ectx.model.clone(),
            bot_name: ectx.bot_name.clone(),
            plugin_dirs: ectx.plugin_dirs.clone(),
            workflow_name: ectx.workflow_name.clone(),
            worktree_id: ectx.worktree_id.clone(),
            parent_run_id: ectx.parent_run_id.clone(),
            step_id: ectx.step_id.clone(),
        };

        // Convert runkon-flow ActionParams → conductor-core ActionParams.
        let core_schema = params.schema.clone();
        let core_params = crate::workflow::action_executor::ActionParams {
            name: params.name.clone(),
            inputs: (*params.inputs).clone(),
            retries_remaining: params.retries_remaining,
            retry_error: params.retry_error.clone(),
            snippets: params.snippets.clone(),
            dry_run: params.dry_run,
            gate_feedback: params.gate_feedback.clone(),
            schema: core_schema,
        };

        // Dispatch through the inner executor, then surface child_run_id so the
        // engine writes the step↔run link via update_step() post-execution.
        let mut output = self
            .inner
            .execute(&core_ectx, &core_params)
            .map(core_action_output_to_rk)
            .map_err(EngineError::from)?;
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
    }
}

/// Shared body for every `RkItemProvider::items()` implementation.
///
/// Locks the connection, converts the scope, delegates to `provider`, and maps
/// the result back into runkon-flow types.  All four adapters differ only in
/// which `ItemProvider` implementation they pass here.
fn delegate_items<P: ItemProvider>(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    config: &crate::config::Config,
    scope: Option<&runkon_flow::dsl::ForeachScope>,
    filter: &HashMap<String, String>,
    existing_set: &HashSet<String>,
    provider: P,
) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
    let guard = conn.lock().map_err(bridge_lock_err)?;
    let core_ctx = crate::workflow::item_provider::ProviderContext {
        conn: &guard,
        config,
    };
    provider
        .items(&core_ctx, scope, filter, existing_set)
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
            fn items(
                &self,
                _ctx: &runkon_flow::traits::item_provider::ProviderContext,
                scope: Option<&runkon_flow::dsl::ForeachScope>,
                filter: &HashMap<String, String>,
                existing_set: &HashSet<String>,
            ) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
                delegate_items(
                    &self.conn,
                    &self.config,
                    scope,
                    filter,
                    existing_set,
                    self.provider(),
                )
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
}

impl ConductorChildWorkflowRunner {
    pub(super) fn new(
        db_path: std::path::PathBuf,
        config: crate::config::Config,
        conn: Arc<Mutex<rusqlite::Connection>>,
    ) -> Self {
        Self {
            db_path,
            config,
            conn,
        }
    }
}

impl runkon_flow::engine::ChildWorkflowRunner for ConductorChildWorkflowRunner {
    fn execute_child(
        &self,
        workflow_name: &str,
        parent_state: &runkon_flow::engine::ExecutionState,
        params: runkon_flow::engine::ChildWorkflowInput,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        // Load the real workflow definition from disk. The runner resolves the
        // actual definition by name from the worktree/repo .conductor/workflows/ directory.
        let core_def = runkon_flow::dsl::load_workflow_by_name(
            &parent_state.worktree_ctx.working_dir,
            &parent_state.worktree_ctx.repo_path,
            workflow_name,
        )
        .map_err(|e| {
            EngineError::Workflow(format!(
                "failed to load sub-workflow '{}': {e}",
                workflow_name
            ))
        })?;

        let exec_config = crate::workflow::types::WorkflowExecConfig {
            poll_interval: parent_state.exec_config.poll_interval,
            step_timeout: parent_state.exec_config.step_timeout,
            fail_fast: parent_state.exec_config.fail_fast,
            dry_run: parent_state.exec_config.dry_run,
            shutdown: parent_state.exec_config.shutdown.clone(),
            event_sinks: parent_state.event_sinks.iter().cloned().collect(),
        };

        // Route child workflows through execute_workflow_standalone so they use
        // FlowEngine::run() — keeping event emission and step tracking consistent
        // between parent and child runs.
        let standalone_params = crate::workflow::types::WorkflowExecStandalone {
            config: self.config.clone(),
            workflow: core_def,
            worktree_id: parent_state.worktree_ctx.worktree_id.clone(),
            working_dir: parent_state.worktree_ctx.working_dir.clone(),
            repo_path: parent_state.worktree_ctx.repo_path.clone(),
            ticket_id: parent_state.inputs.get("ticket_id").cloned(),
            repo_id: parent_state.inputs.get("repo_id").cloned(),
            model: parent_state.model.clone(),
            exec_config,
            inputs: params.inputs,
            target_label: parent_state.target_label.clone(),
            run_id_notify: None,
            triggered_by_hook: parent_state.triggered_by_hook,
            conductor_bin_dir: None,
            force: false,
            extra_plugin_dirs: parent_state.worktree_ctx.extra_plugin_dirs.clone(),
            db_path: Some(self.db_path.clone()),
            parent_workflow_run_id: Some(parent_state.workflow_run_id.clone()),
            depth: params.depth,
            parent_step_id: params.parent_step_id,
            default_bot_name: params.bot_name,
            iteration: params.iteration,
        };

        let core_result = super::coordinator::execute_workflow_standalone(&standalone_params)
            .map_err(|e| match e {
                ConductorError::WorkflowCancelled => EngineError::from(e),
                other => EngineError::Workflow(format!(
                    "child workflow '{}' failed: {other}",
                    workflow_name
                )),
            })?;

        Ok(core_result.into())
    }

    fn resume_child(
        &self,
        workflow_run_id: &str,
        model: Option<&str>,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        let input = crate::workflow::types::WorkflowResumeInput {
            config: &self.config,
            workflow_run_id,
            model,
            from_step: None,
            restart: false,
            conductor_bin_dir: None,
            event_sinks: vec![],
            db_path: Some(self.db_path.clone()),
            shutdown: None,
        };

        let core_result = super::coordinator::resume_workflow(&input).map_err(|e| match e {
            ConductorError::WorkflowCancelled => EngineError::from(e),
            other => EngineError::Workflow(format!(
                "failed to resume child workflow run '{workflow_run_id}': {other}"
            )),
        })?;

        Ok(core_result.into())
    }

    fn find_resumable_child(
        &self,
        parent_run_id: &str,
        workflow_name: &str,
    ) -> runkon_flow::engine_error::Result<Option<runkon_flow::types::WorkflowRun>> {
        let conn = self.conn.lock().map_err(bridge_lock_err)?;

        let mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let core_run = mgr
            .find_resumable_child_run(parent_run_id, workflow_name)
            .map_err(|e| EngineError::Workflow(format!(
                "failed to find resumable child run for parent='{parent_run_id}' workflow='{workflow_name}': {e}"
            )))?;

        Ok(core_run.map(crate::workflow::rk_types::run_to_rk))
    }
}

// ---------------------------------------------------------------------------
// 6. PersistenceAdapter
// ---------------------------------------------------------------------------

/// Bridges the conductor-core `WorkflowPersistence` trait to the runkon-flow
/// `WorkflowPersistence` trait, performing type conversion at the boundary.
///
/// This is the single place in conductor-core that implements
/// `runkon_flow::traits::persistence::WorkflowPersistence`; all other modules
/// interact with the core trait only.
pub(super) struct PersistenceAdapter(
    pub(super) Arc<dyn crate::workflow::persistence::WorkflowPersistence>,
);

impl runkon_flow::traits::persistence::WorkflowPersistence for PersistenceAdapter {
    fn create_run(
        &self,
        new_run: runkon_flow::traits::persistence::NewRun,
    ) -> Result<runkon_flow::types::WorkflowRun, EngineError> {
        let core_run = self.0.create_run(new_run.into())?;
        Ok(crate::workflow::rk_types::run_to_rk(core_run))
    }

    fn get_run(
        &self,
        run_id: &str,
    ) -> Result<Option<runkon_flow::types::WorkflowRun>, EngineError> {
        let result = self.0.get_run(run_id)?;
        Ok(result.map(crate::workflow::rk_types::run_to_rk))
    }

    fn list_active_runs(
        &self,
        statuses: &[runkon_flow::status::WorkflowRunStatus],
    ) -> Result<Vec<runkon_flow::types::WorkflowRun>, EngineError> {
        let core_statuses: Vec<crate::workflow::status::WorkflowRunStatus> =
            statuses.iter().map(|s| s.clone().into()).collect();
        let result = self.0.list_active_runs(&core_statuses)?;
        Ok(result
            .into_iter()
            .map(crate::workflow::rk_types::run_to_rk)
            .collect())
    }

    fn update_run_status(
        &self,
        run_id: &str,
        status: runkon_flow::status::WorkflowRunStatus,
        result_summary: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), EngineError> {
        self.0
            .update_run_status(run_id, status.into(), result_summary, error)
    }

    fn insert_step(
        &self,
        new_step: runkon_flow::traits::persistence::NewStep,
    ) -> Result<String, EngineError> {
        self.0.insert_step(new_step.into())
    }

    fn update_step(
        &self,
        step_id: &str,
        update: runkon_flow::traits::persistence::StepUpdate,
    ) -> Result<(), EngineError> {
        self.0.update_step(step_id, update.into())
    }

    fn get_steps(
        &self,
        run_id: &str,
    ) -> Result<Vec<runkon_flow::types::WorkflowRunStep>, EngineError> {
        let result = self.0.get_steps(run_id)?;
        Ok(result
            .into_iter()
            .map(crate::workflow::rk_types::step_to_rk)
            .collect())
    }

    fn insert_fan_out_item(
        &self,
        step_run_id: &str,
        item_type: &str,
        item_id: &str,
        item_ref: &str,
    ) -> Result<String, EngineError> {
        self.0
            .insert_fan_out_item(step_run_id, item_type, item_id, item_ref)
    }

    fn update_fan_out_item(
        &self,
        item_id: &str,
        update: runkon_flow::traits::persistence::FanOutItemUpdate,
    ) -> Result<(), EngineError> {
        self.0.update_fan_out_item(item_id, update.into())
    }

    fn get_fan_out_items(
        &self,
        step_run_id: &str,
        status_filter: Option<runkon_flow::traits::persistence::FanOutItemStatus>,
    ) -> Result<Vec<runkon_flow::types::FanOutItemRow>, EngineError> {
        let core_filter = status_filter.map(Into::into);
        let result = self.0.get_fan_out_items(step_run_id, core_filter)?;
        Ok(result
            .into_iter()
            .map(crate::workflow::rk_types::fan_out_item_to_rk)
            .collect())
    }

    fn get_gate_approval(
        &self,
        step_id: &str,
    ) -> Result<runkon_flow::traits::persistence::GateApprovalState, EngineError> {
        let result = self.0.get_gate_approval(step_id)?;
        Ok(crate::workflow::rk_types::gate_approval_to_rk(result))
    }

    fn approve_gate(
        &self,
        step_id: &str,
        approved_by: &str,
        feedback: Option<&str>,
        selections: Option<&[String]>,
    ) -> Result<(), EngineError> {
        self.0
            .approve_gate(step_id, approved_by, feedback, selections)
    }

    fn reject_gate(
        &self,
        step_id: &str,
        rejected_by: &str,
        feedback: Option<&str>,
    ) -> Result<(), EngineError> {
        self.0.reject_gate(step_id, rejected_by, feedback)
    }

    fn is_run_cancelled(&self, run_id: &str) -> Result<bool, EngineError> {
        self.0.is_run_cancelled(run_id)
    }

    fn tick_heartbeat(&self, run_id: &str) -> Result<(), EngineError> {
        self.0.tick_heartbeat(run_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_metrics(
        &self,
        run_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
        cost_usd: f64,
        num_turns: i64,
        duration_ms: i64,
    ) -> Result<(), EngineError> {
        self.0.persist_metrics(
            run_id,
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cost_usd,
            num_turns,
            duration_ms,
        )
    }
}

// ---------------------------------------------------------------------------
// 7. Helper builder functions
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
/// Uses `ConductorScriptEnvProvider` so that script steps inherit the
/// conductor binary directory and any extra plugin directories on `PATH`.
pub(super) fn build_rk_script_env_provider(
    conductor_bin_dir: Option<std::path::PathBuf>,
    extra_plugin_dirs: Vec<String>,
) -> Arc<dyn runkon_flow::traits::script_env_provider::ScriptEnvProvider> {
    Arc::new(
        crate::workflow::script_env_provider::ConductorScriptEnvProvider::new(
            conductor_bin_dir,
            extra_plugin_dirs,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::status::WorkflowRunStatus as CoreStatus;
    use std::collections::HashMap;

    fn make_core_run(id: &str, status: CoreStatus) -> crate::workflow::types::WorkflowRun {
        crate::workflow::types::WorkflowRun {
            id: id.to_string(),
            workflow_name: "test-workflow".to_string(),
            worktree_id: None,
            parent_run_id: "parent-run".to_string(),
            status,
            dry_run: false,
            trigger: "manual".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: None,
            result_summary: None,
            error: None,
            definition_snapshot: None,
            inputs: HashMap::new(),
            ticket_id: None,
            repo_id: None,
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            workflow_title: None,
            total_input_tokens: None,
            total_output_tokens: None,
            total_cache_read_input_tokens: None,
            total_cache_creation_input_tokens: None,
            total_turns: None,
            total_cost_usd: None,
            total_duration_ms: None,
            model: None,
            dismissed: false,
        }
    }

    // ---------------------------------------------------------------------------
    // rk_conv::run_to_rk — status conversion
    // ---------------------------------------------------------------------------

    #[test]
    fn status_completed_maps_correctly() {
        let run = make_core_run("r1", CoreStatus::Completed);
        let rk = crate::workflow::rk_types::run_to_rk(run);
        assert_eq!(rk.status, runkon_flow::status::WorkflowRunStatus::Completed);
    }

    #[test]
    fn status_failed_maps_correctly() {
        let run = make_core_run("r1", CoreStatus::Failed);
        let rk = crate::workflow::rk_types::run_to_rk(run);
        assert_eq!(rk.status, runkon_flow::status::WorkflowRunStatus::Failed);
    }

    #[test]
    fn status_running_maps_correctly() {
        let run = make_core_run("r1", CoreStatus::Running);
        let rk = crate::workflow::rk_types::run_to_rk(run);
        assert_eq!(rk.status, runkon_flow::status::WorkflowRunStatus::Running);
    }

    #[test]
    fn blocked_on_none_maps_to_none() {
        let run = make_core_run("r1", CoreStatus::Completed);
        let rk = crate::workflow::rk_types::run_to_rk(run);
        assert!(rk.blocked_on.is_none());
    }

    // ---------------------------------------------------------------------------
    // delegate_items — mutex poison propagates as EngineError
    // ---------------------------------------------------------------------------

    #[test]
    fn delegate_items_propagates_mutex_poison() {
        use std::collections::HashSet;
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
            &HashSet::new(),
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
    // Fix 9: PersistenceAdapter::get_gate_approval via the adapter
    // ---------------------------------------------------------------------------

    #[test]
    fn persistence_adapter_get_gate_approval_returns_approved_state() {
        use crate::workflow::persistence::{NewRun, NewStep, WorkflowPersistence};
        use crate::workflow::persistence_memory::InMemoryWorkflowPersistence;
        use runkon_flow::traits::persistence::GateApprovalState as RkGateApprovalState;
        use runkon_flow::traits::persistence::WorkflowPersistence as RkPersistence;

        // 1. Create the in-memory persistence.
        let persistence = InMemoryWorkflowPersistence::new();

        // 2. Create a run and insert a step.
        let run = persistence
            .create_run(NewRun {
                workflow_name: "test-wf".to_string(),
                worktree_id: None,
                ticket_id: None,
                repo_id: None,
                parent_run_id: "parent".to_string(),
                dry_run: false,
                trigger: "test".to_string(),
                definition_snapshot: None,
                parent_workflow_run_id: None,
                target_label: None,
            })
            .unwrap();
        let step_id = persistence
            .insert_step(NewStep {
                workflow_run_id: run.id.clone(),
                step_name: "approval-gate".to_string(),
                role: "gate".to_string(),
                can_commit: false,
                position: 0,
                iteration: 0,
                retry_count: None,
            })
            .unwrap();

        // 3. Approve the gate via the core persistence.
        persistence
            .approve_gate(&step_id, "tester", Some("lgtm"), None)
            .unwrap();

        // 4. Wrap in PersistenceAdapter.
        let adapter = PersistenceAdapter(Arc::new(persistence));

        // 5. Call get_gate_approval via the runkon-flow trait and verify the result.
        let state = RkPersistence::get_gate_approval(&adapter, &step_id).unwrap();
        match state {
            RkGateApprovalState::Approved { feedback, .. } => {
                assert_eq!(
                    feedback.as_deref(),
                    Some("lgtm"),
                    "feedback must survive the approve_gate/get_gate_approval roundtrip via PersistenceAdapter"
                );
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // Fix 10: dependencies method via a concrete Rk item-provider adapter
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
}
