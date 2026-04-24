//! Bridge adapters between `conductor-core` types and `runkon-flow` traits.
//!
//! This module converts between the two type universes so that
//! `execute_workflow_standalone` can delegate to `runkon_flow::FlowEngine::run()`.
//!
//! All items are `pub(super)` — visible to the parent `workflow` module only.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use crate::error::ConductorError;
use crate::workflow::action_executor::ActionExecutor;
use crate::workflow::item_provider::ItemProvider;

// ---------------------------------------------------------------------------
// 1. OutputSchema conversion helpers (runkon-flow → conductor-core)
// ---------------------------------------------------------------------------

/// Convert a `runkon_flow` `OutputSchema` into a `conductor-core` `OutputSchema`.
pub(super) fn rk_schema_to_core(
    rk: runkon_flow::output_schema::OutputSchema,
) -> crate::schema_config::OutputSchema {
    crate::schema_config::OutputSchema {
        name: rk.name,
        fields: rk.fields.into_iter().map(rk_field_def_to_core).collect(),
        markers: rk.markers,
    }
}

fn rk_field_def_to_core(
    rk: runkon_flow::output_schema::FieldDef,
) -> crate::schema_config::FieldDef {
    crate::schema_config::FieldDef {
        name: rk.name,
        required: rk.required,
        field_type: rk_field_type_to_core(rk.field_type),
        desc: rk.desc,
        examples: rk.examples,
    }
}

fn rk_field_type_to_core(
    rk: runkon_flow::output_schema::FieldType,
) -> crate::schema_config::FieldType {
    match rk {
        runkon_flow::output_schema::FieldType::String => crate::schema_config::FieldType::String,
        runkon_flow::output_schema::FieldType::Number => crate::schema_config::FieldType::Number,
        runkon_flow::output_schema::FieldType::Boolean => crate::schema_config::FieldType::Boolean,
        runkon_flow::output_schema::FieldType::Enum(variants) => {
            crate::schema_config::FieldType::Enum(variants)
        }
        runkon_flow::output_schema::FieldType::Array { items } => {
            crate::schema_config::FieldType::Array {
                items: rk_array_items_to_core(items),
            }
        }
        runkon_flow::output_schema::FieldType::Object { fields } => {
            crate::schema_config::FieldType::Object {
                fields: fields.into_iter().map(rk_field_def_to_core).collect(),
            }
        }
    }
}

fn rk_array_items_to_core(
    rk: runkon_flow::output_schema::ArrayItems,
) -> crate::schema_config::ArrayItems {
    match rk {
        runkon_flow::output_schema::ArrayItems::Scalar(ft) => {
            crate::schema_config::ArrayItems::Scalar(Box::new(rk_field_type_to_core(*ft)))
        }
        runkon_flow::output_schema::ArrayItems::Object(fields) => {
            crate::schema_config::ArrayItems::Object(
                fields.into_iter().map(rk_field_def_to_core).collect(),
            )
        }
        runkon_flow::output_schema::ArrayItems::Untyped => {
            crate::schema_config::ArrayItems::Untyped
        }
    }
}

/// Convert a `conductor-core` `OutputSchema` into a `runkon_flow` `OutputSchema`.
///
/// Inverse of `rk_schema_to_core`. Used by the `schema_resolver` closure passed
/// to `ExecutionState` so that `load_schema()` results are usable by the engine.
pub(super) fn core_schema_to_rk(
    core: crate::schema_config::OutputSchema,
) -> runkon_flow::output_schema::OutputSchema {
    runkon_flow::output_schema::OutputSchema {
        name: core.name,
        fields: core.fields.into_iter().map(core_field_def_to_rk).collect(),
        markers: core.markers,
    }
}

fn core_field_def_to_rk(
    core: crate::schema_config::FieldDef,
) -> runkon_flow::output_schema::FieldDef {
    runkon_flow::output_schema::FieldDef {
        name: core.name,
        required: core.required,
        field_type: core_field_type_to_rk(core.field_type),
        desc: core.desc,
        examples: core.examples,
    }
}

fn core_field_type_to_rk(
    core: crate::schema_config::FieldType,
) -> runkon_flow::output_schema::FieldType {
    match core {
        crate::schema_config::FieldType::String => runkon_flow::output_schema::FieldType::String,
        crate::schema_config::FieldType::Number => runkon_flow::output_schema::FieldType::Number,
        crate::schema_config::FieldType::Boolean => runkon_flow::output_schema::FieldType::Boolean,
        crate::schema_config::FieldType::Enum(variants) => {
            runkon_flow::output_schema::FieldType::Enum(variants)
        }
        crate::schema_config::FieldType::Array { items } => {
            runkon_flow::output_schema::FieldType::Array {
                items: core_array_items_to_rk(items),
            }
        }
        crate::schema_config::FieldType::Object { fields } => {
            runkon_flow::output_schema::FieldType::Object {
                fields: fields.into_iter().map(core_field_def_to_rk).collect(),
            }
        }
    }
}

fn core_array_items_to_rk(
    core: crate::schema_config::ArrayItems,
) -> runkon_flow::output_schema::ArrayItems {
    match core {
        crate::schema_config::ArrayItems::Scalar(ft) => {
            runkon_flow::output_schema::ArrayItems::Scalar(Box::new(core_field_type_to_rk(*ft)))
        }
        crate::schema_config::ArrayItems::Object(fields) => {
            runkon_flow::output_schema::ArrayItems::Object(
                fields.into_iter().map(core_field_def_to_rk).collect(),
            )
        }
        crate::schema_config::ArrayItems::Untyped => {
            runkon_flow::output_schema::ArrayItems::Untyped
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
///
/// The runkon-flow `ExecutionContext` does not carry `db_path`, so we store it
/// in the adapter and inject it when constructing the core `ExecutionContext`.
pub(super) struct RkActionExecutorAdapter {
    inner: crate::workflow::claude_agent_executor::ClaudeAgentExecutor,
    db_path: std::path::PathBuf,
}

impl RkActionExecutorAdapter {
    pub(super) fn new(config: crate::config::Config, db_path: std::path::PathBuf) -> Self {
        let api_executor: Box<dyn crate::workflow::action_executor::ActionExecutor> = Box::new(
            crate::workflow::api_call_executor::ApiCallExecutor::new(config.clone()),
        );
        Self {
            inner: crate::workflow::claude_agent_executor::ClaudeAgentExecutor::new(
                config,
                Some(api_executor),
            ),
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
        // Convert runkon-flow ExecutionContext → conductor-core ExecutionContext,
        // injecting db_path which exists only in the conductor-core variant.
        let core_ectx = crate::workflow::action_executor::ExecutionContext {
            run_id: ectx.run_id.clone(),
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
        let core_schema = params.schema.clone().map(rk_schema_to_core);
        let core_params = crate::workflow::action_executor::ActionParams {
            name: params.name.clone(),
            inputs: params.inputs.clone(),
            retries_remaining: params.retries_remaining,
            retry_error: params.retry_error.clone(),
            snippets: params.snippets.clone(),
            dry_run: params.dry_run,
            gate_feedback: params.gate_feedback.clone(),
            schema: core_schema,
        };

        // Dispatch through the inner executor.
        self.inner
            .execute(&core_ectx, &core_params)
            .map(core_action_output_to_rk)
            .map_err(|e| match e {
                ConductorError::WorkflowCancelled => EngineError::Cancelled(
                    runkon_flow::cancellation_reason::CancellationReason::UserRequested(None),
                ),
                other => EngineError::Workflow(other.to_string()),
            })
    }
}

// ---------------------------------------------------------------------------
// 4. RkItemProvider adapters
// ---------------------------------------------------------------------------

/// Convert a runkon-flow `ForeachScope` to a conductor-core `ForeachScope` via
/// JSON round-trip (both types share the same serialization schema).
fn rk_scope_to_core(
    scope: &runkon_flow::dsl::ForeachScope,
) -> crate::error::Result<crate::workflow_dsl::ForeachScope> {
    let json = serde_json::to_string(scope).map_err(|e| ConductorError::Workflow(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| ConductorError::Workflow(e.to_string()))
}

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

// ---------------------------------------------------------------------------
// 4a. RkTicketsItemProvider
// ---------------------------------------------------------------------------

pub(super) struct RkTicketsItemProvider {
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: crate::config::Config,
    repo_id: Option<String>,
}

impl RkTicketsItemProvider {
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
}

impl runkon_flow::traits::item_provider::ItemProvider for RkTicketsItemProvider {
    fn name(&self) -> &str {
        "tickets"
    }

    fn items(
        &self,
        _ctx: &runkon_flow::traits::item_provider::ProviderContext,
        scope: Option<&runkon_flow::dsl::ForeachScope>,
        filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
        let guard = self
            .conn
            .lock()
            .map_err(|e| EngineError::Workflow(format!("mutex poisoned: {e}")))?;
        let core_ctx = crate::workflow::item_provider::ProviderContext {
            conn: &guard,
            config: &self.config,
        };
        let core_scope = match scope {
            Some(s) => Some(rk_scope_to_core(s).map_err(|e| EngineError::Workflow(e.to_string()))?),
            None => None,
        };
        let provider =
            crate::workflow::item_provider::tickets::TicketsProvider::new(self.repo_id.clone());
        provider
            .items(&core_ctx, core_scope.as_ref(), filter, existing_set)
            .map(|items: Vec<crate::workflow::item_provider::FanOutItem>| {
                items.into_iter().map(core_fan_out_item_to_rk).collect()
            })
            .map_err(|e: crate::error::ConductorError| EngineError::Workflow(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// 4b. RkReposItemProvider
// ---------------------------------------------------------------------------

pub(super) struct RkReposItemProvider {
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: crate::config::Config,
}

impl RkReposItemProvider {
    pub(super) fn new(
        conn: Arc<Mutex<rusqlite::Connection>>,
        config: crate::config::Config,
    ) -> Self {
        Self { conn, config }
    }
}

impl runkon_flow::traits::item_provider::ItemProvider for RkReposItemProvider {
    fn name(&self) -> &str {
        "repos"
    }

    fn items(
        &self,
        _ctx: &runkon_flow::traits::item_provider::ProviderContext,
        scope: Option<&runkon_flow::dsl::ForeachScope>,
        filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
        let guard = self
            .conn
            .lock()
            .map_err(|e| EngineError::Workflow(format!("mutex poisoned: {e}")))?;
        let core_ctx = crate::workflow::item_provider::ProviderContext {
            conn: &guard,
            config: &self.config,
        };
        let core_scope = match scope {
            Some(s) => Some(rk_scope_to_core(s).map_err(|e| EngineError::Workflow(e.to_string()))?),
            None => None,
        };
        let provider = crate::workflow::item_provider::repos::ReposProvider;
        provider
            .items(&core_ctx, core_scope.as_ref(), filter, existing_set)
            .map(|items: Vec<crate::workflow::item_provider::FanOutItem>| {
                items.into_iter().map(core_fan_out_item_to_rk).collect()
            })
            .map_err(|e: crate::error::ConductorError| EngineError::Workflow(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// 4c. RkWorkflowRunsItemProvider
// ---------------------------------------------------------------------------

pub(super) struct RkWorkflowRunsItemProvider {
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: crate::config::Config,
}

impl RkWorkflowRunsItemProvider {
    pub(super) fn new(
        conn: Arc<Mutex<rusqlite::Connection>>,
        config: crate::config::Config,
    ) -> Self {
        Self { conn, config }
    }
}

impl runkon_flow::traits::item_provider::ItemProvider for RkWorkflowRunsItemProvider {
    fn name(&self) -> &str {
        "workflow_runs"
    }

    fn items(
        &self,
        _ctx: &runkon_flow::traits::item_provider::ProviderContext,
        scope: Option<&runkon_flow::dsl::ForeachScope>,
        filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
        let guard = self
            .conn
            .lock()
            .map_err(|e| EngineError::Workflow(format!("mutex poisoned: {e}")))?;
        let core_ctx = crate::workflow::item_provider::ProviderContext {
            conn: &guard,
            config: &self.config,
        };
        let core_scope = match scope {
            Some(s) => Some(rk_scope_to_core(s).map_err(|e| EngineError::Workflow(e.to_string()))?),
            None => None,
        };
        let provider = crate::workflow::item_provider::workflow_runs::WorkflowRunsProvider;
        provider
            .items(&core_ctx, core_scope.as_ref(), filter, existing_set)
            .map(|items: Vec<crate::workflow::item_provider::FanOutItem>| {
                items.into_iter().map(core_fan_out_item_to_rk).collect()
            })
            .map_err(|e: crate::error::ConductorError| EngineError::Workflow(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// 4d. RkWorktreesItemProvider
// ---------------------------------------------------------------------------

pub(super) struct RkWorktreesItemProvider {
    conn: Arc<Mutex<rusqlite::Connection>>,
    config: crate::config::Config,
    repo_id: Option<String>,
}

impl RkWorktreesItemProvider {
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
}

impl runkon_flow::traits::item_provider::ItemProvider for RkWorktreesItemProvider {
    fn name(&self) -> &str {
        "worktrees"
    }

    fn items(
        &self,
        _ctx: &runkon_flow::traits::item_provider::ProviderContext,
        scope: Option<&runkon_flow::dsl::ForeachScope>,
        filter: &HashMap<String, String>,
        existing_set: &HashSet<String>,
    ) -> Result<Vec<runkon_flow::traits::item_provider::FanOutItem>, EngineError> {
        let guard = self
            .conn
            .lock()
            .map_err(|e| EngineError::Workflow(format!("mutex poisoned: {e}")))?;
        let core_ctx = crate::workflow::item_provider::ProviderContext {
            conn: &guard,
            config: &self.config,
        };
        let core_scope = match scope {
            Some(s) => Some(rk_scope_to_core(s).map_err(|e| EngineError::Workflow(e.to_string()))?),
            None => None,
        };
        // WorktreesProvider requires repo_id and worktree_id; pass repo_id from self,
        // worktree_id is not available in this context.
        let provider = crate::workflow::item_provider::worktrees::WorktreesProvider::new(
            self.repo_id.clone(),
            None,
        );
        provider
            .items(&core_ctx, core_scope.as_ref(), filter, existing_set)
            .map(|items: Vec<crate::workflow::item_provider::FanOutItem>| {
                items.into_iter().map(core_fan_out_item_to_rk).collect()
            })
            .map_err(|e: crate::error::ConductorError| EngineError::Workflow(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// 5. ConductorChildWorkflowRunner
// ---------------------------------------------------------------------------

/// Implements `runkon_flow::engine::ChildWorkflowRunner` by delegating to
/// conductor-core's `execute_workflow` / `resume_workflow` functions.
pub(super) struct ConductorChildWorkflowRunner {
    db_path: std::path::PathBuf,
    config: crate::config::Config,
}

impl ConductorChildWorkflowRunner {
    pub(super) fn new(db_path: std::path::PathBuf, config: crate::config::Config) -> Self {
        Self { db_path, config }
    }
}

impl runkon_flow::engine::ChildWorkflowRunner for ConductorChildWorkflowRunner {
    fn execute_child(
        &self,
        child_def: &runkon_flow::dsl::WorkflowDef,
        parent_state: &runkon_flow::engine::ExecutionState,
        params: runkon_flow::engine::ChildWorkflowInput,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        // Convert runkon-flow WorkflowDef → conductor-core WorkflowDef via JSON.
        let def_json =
            serde_json::to_string(child_def).map_err(|e| EngineError::Workflow(e.to_string()))?;
        let core_def: crate::workflow_dsl::WorkflowDef =
            serde_json::from_str(&def_json).map_err(|e| EngineError::Workflow(e.to_string()))?;

        // Open a fresh connection for this child run.
        let conn = crate::db::open_database(&self.db_path)
            .map_err(|e| EngineError::Workflow(e.to_string()))?;

        // Build exec config from parent state — no event_sinks propagated here
        // (the child run will fire its own events independently).
        let exec_config = crate::workflow::types::WorkflowExecConfig {
            poll_interval: parent_state.exec_config.poll_interval,
            step_timeout: parent_state.exec_config.step_timeout,
            fail_fast: parent_state.exec_config.fail_fast,
            dry_run: parent_state.exec_config.dry_run,
            shutdown: parent_state.exec_config.shutdown.clone(),
            event_sinks: parent_state.event_sinks.iter().cloned().collect(),
        };

        let input = crate::workflow::types::WorkflowExecInput {
            conn: &conn,
            config: &self.config,
            workflow: &core_def,
            worktree_id: parent_state.worktree_ctx.worktree_id.as_deref(),
            working_dir: &parent_state.worktree_ctx.working_dir,
            repo_path: &parent_state.worktree_ctx.repo_path,
            model: parent_state.model.as_deref(),
            exec_config: &exec_config,
            inputs: params.inputs,
            ticket_id: parent_state.inputs.get("ticket_id").map(String::as_str),
            repo_id: parent_state.inputs.get("repo_id").map(String::as_str),
            depth: params.depth,
            parent_workflow_run_id: Some(&parent_state.workflow_run_id),
            target_label: parent_state.target_label.as_deref(),
            default_bot_name: params.bot_name,
            iteration: params.iteration,
            run_id_notify: None,
            triggered_by_hook: parent_state.triggered_by_hook,
            conductor_bin_dir: None,
            force: false,
            extra_plugin_dirs: parent_state.worktree_ctx.extra_plugin_dirs.clone(),
            parent_step_id: params.parent_step_id,
        };

        let core_result = crate::workflow::engine::execute_workflow(&input)
            .map_err(|e| EngineError::Workflow(e.to_string()))?;

        Ok(core_workflow_result_to_rk(core_result))
    }

    fn resume_child(
        &self,
        workflow_run_id: &str,
        model: Option<&str>,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        let conn = crate::db::open_database(&self.db_path)
            .map_err(|e| EngineError::Workflow(e.to_string()))?;

        let input = crate::workflow::types::WorkflowResumeInput {
            conn: &conn,
            config: &self.config,
            workflow_run_id,
            model,
            from_step: None,
            restart: false,
            conductor_bin_dir: None,
            event_sinks: vec![],
        };

        let core_result = crate::workflow::engine::resume_workflow(&input)
            .map_err(|e| EngineError::Workflow(e.to_string()))?;

        Ok(core_workflow_result_to_rk(core_result))
    }

    fn find_resumable_child(
        &self,
        parent_run_id: &str,
        workflow_name: &str,
    ) -> runkon_flow::engine_error::Result<Option<runkon_flow::types::WorkflowRun>> {
        let conn = crate::db::open_database(&self.db_path)
            .map_err(|e| EngineError::Workflow(e.to_string()))?;

        let mgr = crate::workflow::manager::WorkflowManager::new(&conn);
        let core_run = mgr
            .find_resumable_child_run(parent_run_id, workflow_name)
            .map_err(|e| EngineError::Workflow(e.to_string()))?;

        Ok(core_run.map(core_workflow_run_to_rk))
    }
}

/// Convert a conductor-core `WorkflowResult` to a runkon-flow `WorkflowResult`.
fn core_workflow_result_to_rk(
    core: crate::workflow::types::WorkflowResult,
) -> runkon_flow::types::WorkflowResult {
    runkon_flow::types::WorkflowResult {
        workflow_run_id: core.workflow_run_id,
        worktree_id: core.worktree_id,
        workflow_name: core.workflow_name,
        all_succeeded: core.all_succeeded,
        total_cost: core.total_cost,
        total_turns: core.total_turns,
        total_duration_ms: core.total_duration_ms,
        total_input_tokens: core.total_input_tokens,
        total_output_tokens: core.total_output_tokens,
        total_cache_read_input_tokens: core.total_cache_read_input_tokens,
        total_cache_creation_input_tokens: core.total_cache_creation_input_tokens,
    }
}

/// Convert a conductor-core `WorkflowRun` to a runkon-flow `WorkflowRun`.
///
/// `status` is converted via `to_string()` / `parse()` since both types share
/// the same string representations.  `blocked_on` is converted via JSON round-trip.
fn core_workflow_run_to_rk(
    run: crate::workflow::types::WorkflowRun,
) -> runkon_flow::types::WorkflowRun {
    let rk_status = run
        .status
        .to_string()
        .parse::<runkon_flow::status::WorkflowRunStatus>()
        .unwrap_or(runkon_flow::status::WorkflowRunStatus::Failed);

    let rk_blocked_on = run.blocked_on.and_then(|bo| {
        serde_json::to_string(&bo)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
    });

    runkon_flow::types::WorkflowRun {
        id: run.id,
        workflow_name: run.workflow_name,
        worktree_id: run.worktree_id,
        parent_run_id: run.parent_run_id,
        status: rk_status,
        dry_run: run.dry_run,
        trigger: run.trigger,
        started_at: run.started_at,
        ended_at: run.ended_at,
        result_summary: run.result_summary,
        error: run.error,
        definition_snapshot: run.definition_snapshot,
        inputs: run.inputs,
        ticket_id: run.ticket_id,
        repo_id: run.repo_id,
        parent_workflow_run_id: run.parent_workflow_run_id,
        target_label: run.target_label,
        default_bot_name: run.default_bot_name,
        iteration: run.iteration,
        blocked_on: rk_blocked_on,
        workflow_title: run.workflow_title,
        total_input_tokens: run.total_input_tokens,
        total_output_tokens: run.total_output_tokens,
        total_cache_read_input_tokens: run.total_cache_read_input_tokens,
        total_cache_creation_input_tokens: run.total_cache_creation_input_tokens,
        total_turns: run.total_turns,
        total_cost_usd: run.total_cost_usd,
        total_duration_ms: run.total_duration_ms,
        model: run.model,
        dismissed: run.dismissed,
    }
}

// ---------------------------------------------------------------------------
// 6. Helper builder functions
// ---------------------------------------------------------------------------

/// Build a runkon-flow `ActionRegistry` backed by a `RkActionExecutorAdapter`
/// as the catch-all fallback executor.
pub(super) fn build_rk_action_registry(
    config: &crate::config::Config,
    db_path: &std::path::Path,
) -> runkon_flow::traits::action_executor::ActionRegistry {
    let adapter = RkActionExecutorAdapter::new(config.clone(), db_path.to_path_buf());
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
    config: &crate::config::Config,
    params: &crate::workflow::types::WorkflowExecStandalone,
) -> Arc<dyn runkon_flow::traits::script_env_provider::ScriptEnvProvider> {
    let _ = config;
    Arc::new(
        crate::workflow::script_env_provider::ConductorScriptEnvProvider::new(
            params.conductor_bin_dir.clone(),
            params.extra_plugin_dirs.clone(),
        ),
    )
}
