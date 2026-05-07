use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use super::{bridge_lock_err, wrap_child_workflow_err};

/// Implements `runkon_flow::engine::ChildWorkflowRunner` by delegating to
/// conductor-core's `execute_workflow` / `resume_workflow` functions.
pub(crate) struct ConductorChildWorkflowRunner {
    db_path: std::path::PathBuf,
    config: crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    /// Cached from the parent run at construction time to avoid a per-child DB round-trip.
    target_label: Option<String>,
    triggered_by_hook: bool,
}

impl ConductorChildWorkflowRunner {
    pub(crate) fn new(
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
            // Child workflows inherit no explicit runtime override from the
            // parent — the adapter's derive-from-model fallback in
            // `RkActionExecutorAdapter` routes the inherited `model` to the
            // owning runtime. Propagating an explicit override across child
            // boundaries would require a parallel field on
            // `runkon_flow::engine::ChildWorkflowContext`, which is intentionally
            // host-neutral.
            runtime: None,
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
            // See `build_child_standalone_params` for the runtime-propagation
            // rationale: child resumes also rely on derive-from-model.
            runtime: None,
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

        let core_result =
            super::super::coordinator::execute_workflow_standalone(&standalone_params).map_err(
                |e| wrap_child_workflow_err(e, format!("child workflow '{workflow_name}' failed")),
            )?;

        Ok(core_result)
    }

    fn resume_child(
        &self,
        workflow_run_id: &str,
        model: Option<&str>,
        parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
    ) -> runkon_flow::engine_error::Result<runkon_flow::types::WorkflowResult> {
        let input = self.build_resume_input(workflow_run_id, model, parent_ctx);

        let core_result = super::super::coordinator::resume_workflow(&input).map_err(|e| {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

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
