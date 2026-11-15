use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use super::{bridge_lock_err, wrap_child_workflow_err};

/// Child workflows that must start a new run on resume instead of resuming
/// their earlier run (set when the parent is resumed from a step or restarted).
#[derive(Debug, Clone, Default)]
pub(crate) enum FreshChildren {
    #[default]
    None,
    All,
    Named(HashSet<String>),
}

impl FreshChildren {
    pub(crate) fn contains(&self, name: &str) -> bool {
        match self {
            FreshChildren::None => false,
            FreshChildren::All => true,
            FreshChildren::Named(names) => names.contains(name),
        }
    }
}

/// The ticket a child run is about: its `ticket_id` input when non-empty,
/// else the parent run's ticket.
fn child_ticket_id(
    inputs: &std::collections::HashMap<String, String>,
    parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
) -> Option<String> {
    match non_empty_ticket_input(inputs) {
        Some(t) => Some(t.to_string()),
        None => parent_ctx
            .run_ctx
            .get(crate::workflow::engine_keys::TICKET_ID),
    }
}

/// The `ticket_id` input, trimmed, when it is non-empty.
fn non_empty_ticket_input(inputs: &std::collections::HashMap<String, String>) -> Option<&str> {
    inputs
        .get("ticket_id")
        .map(|s| s.trim())
        .filter(|t| !t.is_empty())
}

/// An unknown ticket id would otherwise surface as a foreign-key error after
/// the child run row is created. Only a not-found result reads as "not a known
/// ticket"; any other lookup error is reported with its cause.
fn validate_child_ticket_exists(
    conn: &rusqlite::Connection,
    workflow_name: &str,
    inputs: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let Some(tid) = non_empty_ticket_input(inputs) else {
        return Ok(());
    };
    match crate::tickets::TicketSyncer::new(conn).get_by_id(tid) {
        Ok(_) => Ok(()),
        Err(crate::error::ConductorError::TicketNotFound { .. }) => Err(format!(
            "child workflow '{workflow_name}': input ticket_id '{tid}' is not a known ticket"
        )),
        Err(e) => Err(format!(
            "child workflow '{workflow_name}': failed to look up ticket '{tid}': {e}"
        )),
    }
}

/// Fail clearly when a child's `ticket_id` input is an unfilled `{{...}}`
/// placeholder, e.g. `{{item.ticket_id}}` for a worktree with no linked ticket.
fn validate_child_ticket_input(
    workflow_name: &str,
    inputs: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    match inputs.get("ticket_id") {
        Some(v) if v.contains("{{") => Err(format!(
            "child workflow '{workflow_name}': input ticket_id is the unresolved placeholder '{v}'; \
             the foreach item has no such key (a worktree with no linked ticket? \
             use scope = {{ has_ticket = true }})"
        )),
        _ => Ok(()),
    }
}

/// Where a child run executes: the item's worktree when the `worktree_id`
/// input names one, else the parent's worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildTarget {
    worktree_id: Option<String>,
    working_dir: String,
    repo_path: String,
    repo_id: Option<String>,
    /// True when the target worktree is not the parent's.
    retargeted: bool,
}

/// The `worktree_id` input, trimmed, when it is non-empty.
fn non_empty_worktree_input(inputs: &std::collections::HashMap<String, String>) -> Option<&str> {
    inputs
        .get("worktree_id")
        .map(|s| s.trim())
        .filter(|t| !t.is_empty())
}

/// Fail clearly when a child's `worktree_id` input is an unfilled `{{...}}` placeholder.
fn validate_child_worktree_input(
    workflow_name: &str,
    inputs: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    match inputs.get("worktree_id") {
        Some(v) if v.contains("{{") => Err(format!(
            "child workflow '{workflow_name}': input worktree_id is the unresolved placeholder '{v}'; \
             the foreach item has no such key"
        )),
        _ => Ok(()),
    }
}

/// Resolve where a child runs. With no (or an empty) `worktree_id` input the
/// child inherits the parent's worktree, working dir, repo and repo path. With
/// one, all four come from that worktree.
fn resolve_child_target(
    conn: &rusqlite::Connection,
    config: &crate::config::Config,
    workflow_name: &str,
    inputs: &std::collections::HashMap<String, String>,
    parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
) -> Result<ChildTarget, String> {
    use crate::workflow::engine_keys;
    let parent_worktree = parent_ctx.run_ctx.get(engine_keys::WORKTREE_ID);
    let inherit = || ChildTarget {
        worktree_id: parent_worktree.clone(),
        working_dir: parent_ctx.run_ctx.working_dir_str(),
        repo_path: parent_ctx
            .run_ctx
            .get(engine_keys::REPO_PATH)
            .unwrap_or_default(),
        repo_id: parent_ctx.run_ctx.get(engine_keys::REPO_ID),
        retargeted: false,
    };
    let Some(wt_id) = non_empty_worktree_input(inputs) else {
        return Ok(inherit());
    };
    if parent_worktree.as_deref() == Some(wt_id) {
        return Ok(inherit());
    }
    let wt = match crate::worktree::WorktreeManager::new(conn, config).get_by_id(wt_id) {
        Ok(wt) => wt,
        Err(crate::error::ConductorError::WorktreeNotFound { .. }) => {
            return Err(format!(
            "child workflow '{workflow_name}': input worktree_id '{wt_id}' is not a known worktree"
        ))
        }
        Err(e) => {
            return Err(format!(
                "child workflow '{workflow_name}': failed to look up worktree '{wt_id}': {e}"
            ))
        }
    };
    if !wt.is_active() {
        return Err(format!(
            "child workflow '{workflow_name}': worktree '{wt_id}' ({}) is not active",
            wt.slug
        ));
    }
    let repo = crate::repo::RepoManager::new(conn, config)
        .get_by_id(&wt.repo_id)
        .map_err(|e| {
            format!(
                "child workflow '{workflow_name}': failed to look up repo '{}' of worktree '{wt_id}': {e}",
                wt.repo_id
            )
        })?;
    Ok(ChildTarget {
        worktree_id: Some(wt.id),
        working_dir: wt.path,
        repo_path: repo.local_path,
        repo_id: Some(wt.repo_id),
        retargeted: true,
    })
}

/// Implements `runkon_flow::engine::ChildWorkflowRunner` by delegating to
/// conductor-core's `execute_workflow` / `resume_workflow` functions.
pub(crate) struct ConductorChildWorkflowRunner {
    db_path: std::path::PathBuf,
    config: crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    /// Cached from the parent run at construction time to avoid a per-child DB round-trip.
    target_label: Option<String>,
    triggered_by_hook: bool,
    fresh_children: FreshChildren,
    /// Smoke mode of the parent run, copied into every child's params so a
    /// child stops at its own first agent or gate step.
    smoke: Option<crate::workflow::SmokeStop>,
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
            fresh_children: FreshChildren::None,
            smoke: None,
        }
    }

    /// Run children in smoke mode, reporting stops into `smoke`.
    pub(crate) fn with_smoke(mut self, smoke: Option<crate::workflow::SmokeStop>) -> Self {
        self.smoke = smoke;
        self
    }

    /// Make `find_resumable_child` answer `None` for the given children, so the
    /// engine starts them fresh.
    pub(crate) fn with_fresh_children(mut self, fresh_children: FreshChildren) -> Self {
        self.fresh_children = fresh_children;
        self
    }

    /// Build the `WorkflowExecStandalone` params for a new child workflow run.
    ///
    /// Extracted for unit-testability. The child's ticket is its `ticket_id`
    /// input when given and non-empty (a fan-out child works on its own item's
    /// ticket), else the parent run's ticket; see `child_ticket_id`. The
    /// worktree, working dir, repo path and `repo_id` come from `target`
    /// (see `resolve_child_target`): the parent's, read from `run_ctx` so
    /// resumed runs whose stored inputs no longer carry the keys still
    /// propagate them, unless a `worktree_id` input retargets the child.
    fn build_child_standalone_params(
        &self,
        workflow: runkon_flow::dsl::WorkflowDef,
        parent_ctx: &runkon_flow::engine::ChildWorkflowContext,
        params: runkon_flow::engine::ChildWorkflowInput,
        target: ChildTarget,
    ) -> crate::workflow::types::WorkflowExecStandalone {
        let exec_config = crate::workflow::WorkflowExecConfig {
            event_sinks: parent_ctx.event_sinks.iter().cloned().collect(),
            ..parent_ctx.exec_config.clone()
        };
        crate::workflow::types::WorkflowExecStandalone {
            config: self.config.clone(),
            workflow,
            worktree_id: target.worktree_id,
            working_dir: target.working_dir,
            repo_path: target.repo_path,
            ticket_id: child_ticket_id(&params.inputs, parent_ctx),
            repo_id: target.repo_id,
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
            default_bot_name: params.as_identity,
            iteration: params.iteration,
            smoke: self.smoke.clone(),
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

        validate_child_ticket_input(workflow_name, &params.inputs)
            .map_err(EngineError::Workflow)?;

        validate_child_worktree_input(workflow_name, &params.inputs)
            .map_err(EngineError::Workflow)?;

        let target = {
            let conn = self.conn.lock().map_err(bridge_lock_err)?;
            validate_child_ticket_exists(&conn, workflow_name, &params.inputs)
                .map_err(EngineError::Workflow)?;
            let target = resolve_child_target(
                &conn,
                &self.config,
                workflow_name,
                &params.inputs,
                parent_ctx,
            )
            .map_err(EngineError::Workflow)?;
            // A child normally shares the parent's worktree, so the active-run
            // guard skips depth > 0. A retargeted child runs in another
            // worktree and must not start beside its active run.
            if target.retargeted && self.smoke.is_none() {
                if let Some(wt_id) = &target.worktree_id {
                    super::super::coordinator::guard_active_run(&conn, wt_id, false).map_err(
                        |e| {
                            EngineError::Workflow(format!(
                                "child workflow '{workflow_name}': worktree '{wt_id}': {e}"
                            ))
                        },
                    )?;
                }
            }
            target
        };

        // Route child workflows through execute_workflow_standalone so they use
        // FlowEngine::run() — keeping event emission and step tracking consistent
        // between parent and child runs.
        let standalone_params =
            self.build_child_standalone_params(core_def, parent_ctx, params, target);

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
        if self.fresh_children.contains(workflow_name) {
            tracing::info!(
                parent_run_id,
                workflow_name,
                "resume requested a fresh child run; not resuming the earlier one"
            );
            return Ok(None);
        }
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

        let parent_ctx = ChildWorkflowContext::new(
            std::sync::Arc::new(runkon_flow::traits::run_context::NoopRunContext::default())
                as std::sync::Arc<dyn runkon_flow::traits::run_context::RunContext>,
            vec![],
            "parent-run".to_string(),
            None,
            crate::workflow::WorkflowExecConfig::default(),
            HashMap::new(),
            Arc::clone(&sinks),
        );

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

        let parent_ctx = ChildWorkflowContext::new(
            std::sync::Arc::new(run_ctx)
                as std::sync::Arc<dyn runkon_flow::traits::run_context::RunContext>,
            vec![],
            "parent-run".to_string(),
            None,
            crate::workflow::WorkflowExecConfig::default(),
            HashMap::new(),
            Arc::<[Arc<dyn EventSink>]>::from(vec![]),
        );

        let workflow = runkon_flow::test_helpers::make_def("test-child", vec![]);
        let params = ChildWorkflowInput {
            inputs: HashMap::new(),
            iteration: 0,
            as_identity: None,
            depth: 1,
            parent_step_id: None,
            cancellation: runkon_flow::CancellationToken::new(),
        };

        let target = {
            let conn = runner.conn.lock().unwrap();
            resolve_child_target(
                &conn,
                &runner.config,
                "test-child",
                &params.inputs,
                &parent_ctx,
            )
            .unwrap()
        };
        let standalone =
            runner.build_child_standalone_params(workflow, &parent_ctx, params, target);

        assert_eq!(
            standalone.ticket_id,
            Some("t-abc".to_string()),
            "ticket_id falls back to run_ctx when inputs lack it"
        );
        assert_eq!(
            standalone.repo_id,
            Some("r-def".to_string()),
            "repo_id must come from run_ctx, not inputs"
        );
    }

    fn parent_ctx_with_ticket(ticket: &str) -> runkon_flow::engine::ChildWorkflowContext {
        use runkon_flow::events::EventSink;
        let mut vars = std::collections::HashMap::new();
        vars.insert(crate::workflow::engine_keys::TICKET_ID, ticket.to_string());
        runkon_flow::engine::ChildWorkflowContext::new(
            Arc::new(runkon_flow::traits::run_context::NoopRunContext::with_vars(
                vars,
            )) as Arc<dyn runkon_flow::traits::run_context::RunContext>,
            vec![],
            "parent-run".to_string(),
            None,
            crate::workflow::WorkflowExecConfig::default(),
            HashMap::new(),
            Arc::<[Arc<dyn EventSink>]>::from(vec![]),
        )
    }

    #[test]
    fn child_ticket_id_input_wins_over_parent() {
        let ctx = parent_ctx_with_ticket("t-parent");
        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "t-child".to_string());
        assert_eq!(child_ticket_id(&inputs, &ctx), Some("t-child".to_string()));
    }

    #[test]
    fn child_ticket_id_falls_back_to_parent_when_missing_or_empty() {
        let ctx = parent_ctx_with_ticket("t-parent");
        assert_eq!(
            child_ticket_id(&HashMap::new(), &ctx),
            Some("t-parent".to_string())
        );
        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "  ".to_string());
        assert_eq!(child_ticket_id(&inputs, &ctx), Some("t-parent".to_string()));
    }

    #[test]
    fn unresolved_ticket_placeholder_is_rejected() {
        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "{{item.ticket_id}}".to_string());
        let err = validate_child_ticket_input("ticket-to-pr", &inputs).unwrap_err();
        assert!(err.contains("{{item.ticket_id}}"), "{err}");
        assert!(err.contains("ticket-to-pr"), "{err}");

        inputs.insert("ticket_id".to_string(), "01ABC".to_string());
        assert!(validate_child_ticket_input("ticket-to-pr", &inputs).is_ok());
        assert!(validate_child_ticket_input("ticket-to-pr", &HashMap::new()).is_ok());
    }

    #[test]
    fn child_ticket_exists_check_covers_unknown_known_and_empty() {
        let conn = crate::test_helpers::setup_db();
        crate::tickets::TicketSyncer::new(&conn)
            .upsert_tickets("r1", &[crate::test_helpers::make_ticket("1", "Issue 1")])
            .unwrap();
        let known: String = conn
            .query_row("SELECT id FROM tickets LIMIT 1", [], |r| r.get(0))
            .unwrap();

        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "no-such-ticket".to_string());
        let err = validate_child_ticket_exists(&conn, "ticket-to-pr", &inputs).unwrap_err();
        assert!(err.contains("no-such-ticket"), "{err}");
        assert!(err.contains("not a known ticket"), "{err}");

        inputs.insert("ticket_id".to_string(), format!(" {known} "));
        assert!(validate_child_ticket_exists(&conn, "ticket-to-pr", &inputs).is_ok());

        inputs.insert("ticket_id".to_string(), "  ".to_string());
        assert!(validate_child_ticket_exists(&conn, "ticket-to-pr", &inputs).is_ok());
        assert!(validate_child_ticket_exists(&conn, "ticket-to-pr", &HashMap::new()).is_ok());
    }

    #[test]
    fn child_ticket_lookup_db_error_is_not_reported_as_unknown() {
        let conn = crate::test_helpers::setup_db();
        conn.execute_batch("DROP TABLE tickets;").unwrap();
        let mut inputs = HashMap::new();
        inputs.insert("ticket_id".to_string(), "t1".to_string());
        let err = validate_child_ticket_exists(&conn, "ticket-to-pr", &inputs).unwrap_err();
        assert!(err.contains("failed to look up ticket 't1'"), "{err}");
        assert!(!err.contains("not a known ticket"), "{err}");
    }

    fn parent_ctx_in_worktree(wt: &str) -> runkon_flow::engine::ChildWorkflowContext {
        use runkon_flow::events::EventSink;
        let mut vars = std::collections::HashMap::new();
        vars.insert(crate::workflow::engine_keys::WORKTREE_ID, wt.to_string());
        vars.insert(
            crate::workflow::engine_keys::REPO_ID,
            "r-parent".to_string(),
        );
        vars.insert(
            crate::workflow::engine_keys::REPO_PATH,
            "/parent/repo".to_string(),
        );
        runkon_flow::engine::ChildWorkflowContext::new(
            Arc::new(runkon_flow::traits::run_context::NoopRunContext::with_vars(
                vars,
            )) as Arc<dyn runkon_flow::traits::run_context::RunContext>,
            vec![],
            "parent-run".to_string(),
            None,
            crate::workflow::WorkflowExecConfig::default(),
            HashMap::new(),
            Arc::<[Arc<dyn EventSink>]>::from(vec![]),
        )
    }

    fn wt_inputs(v: &str) -> HashMap<String, String> {
        HashMap::from([("worktree_id".to_string(), v.to_string())])
    }

    #[test]
    fn child_worktree_input_retargets_to_item_worktree() {
        let conn = crate::test_helpers::setup_db();
        crate::test_helpers::insert_test_repo(&conn, "r-item", "item-repo", "/item/repo");
        crate::test_helpers::insert_test_worktree(&conn, "w-item", "r-item", "item-wt", "/item/wt");
        let ctx = parent_ctx_in_worktree("w-parent");
        let config = crate::config::Config::default();

        let t = resolve_child_target(&conn, &config, "ticket-to-pr", &wt_inputs(" w-item "), &ctx)
            .unwrap();
        assert!(t.retargeted);
        assert_eq!(t.worktree_id.as_deref(), Some("w-item"));
        assert_eq!(t.working_dir, "/item/wt");
        assert_eq!(t.repo_id.as_deref(), Some("r-item"));
        assert_eq!(t.repo_path, "/item/repo");
    }

    #[test]
    fn child_without_worktree_input_inherits_parent_target() {
        let conn = crate::test_helpers::setup_db();
        let ctx = parent_ctx_in_worktree("w-parent");
        let config = crate::config::Config::default();
        for inputs in [HashMap::new(), wt_inputs("  "), wt_inputs("w-parent")] {
            let t = resolve_child_target(&conn, &config, "qualify-ticket", &inputs, &ctx).unwrap();
            assert!(!t.retargeted);
            assert_eq!(t.worktree_id.as_deref(), Some("w-parent"));
            assert_eq!(t.repo_id.as_deref(), Some("r-parent"));
            assert_eq!(t.repo_path, "/parent/repo");
        }
    }

    #[test]
    fn child_worktree_input_rejects_unknown_and_inactive_and_placeholder() {
        let conn = crate::test_helpers::setup_db();
        crate::test_helpers::insert_test_repo(&conn, "r-item", "item-repo", "/item/repo");
        crate::test_helpers::insert_test_worktree(&conn, "w-done", "r-item", "done-wt", "/done");
        conn.execute(
            "UPDATE worktrees SET status = 'merged' WHERE id = 'w-done'",
            [],
        )
        .unwrap();
        let ctx = parent_ctx_in_worktree("w-parent");
        let config = crate::config::Config::default();

        let err =
            resolve_child_target(&conn, &config, "wf", &wt_inputs("w-nope"), &ctx).unwrap_err();
        assert!(
            err.contains("w-nope") && err.contains("not a known worktree"),
            "{err}"
        );

        let err =
            resolve_child_target(&conn, &config, "wf", &wt_inputs("w-done"), &ctx).unwrap_err();
        assert!(
            err.contains("w-done") && err.contains("not active"),
            "{err}"
        );

        let err = validate_child_worktree_input("wf", &wt_inputs("{{item.id}}")).unwrap_err();
        assert!(err.contains("{{item.id}}"), "{err}");
        assert!(validate_child_worktree_input("wf", &wt_inputs("w1")).is_ok());
        assert!(validate_child_worktree_input("wf", &HashMap::new()).is_ok());
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

        let parent_ctx = ChildWorkflowContext::new(
            std::sync::Arc::new(runkon_flow::traits::run_context::NoopRunContext::default())
                as std::sync::Arc<dyn runkon_flow::traits::run_context::RunContext>,
            vec![],
            "parent-run".to_string(),
            None,
            crate::workflow::WorkflowExecConfig::default(),
            HashMap::new(),
            Arc::<[Arc<dyn EventSink>]>::from(vec![]),
        );

        let input = runner.build_resume_input("child-run-2", None, &parent_ctx);
        assert!(input.event_sinks.is_empty());
    }

    #[test]
    fn find_resumable_child_honours_fresh_children() {
        use runkon_flow::engine::ChildWorkflowRunner;

        let conn = crate::test_helpers::setup_db();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF; \
             INSERT INTO workflow_runs (id, workflow_name, status, dry_run, trigger, started_at, parent_workflow_run_id, parent_run_id) \
             VALUES ('child-1', 'child-wf', 'failed', 0, 'manual', '2026-01-01T00:00:00Z', 'parent-1', '');",
        )
        .unwrap();
        let conn = Arc::new(Mutex::new(conn));

        let make = |fresh: FreshChildren| {
            ConductorChildWorkflowRunner::new(
                std::path::PathBuf::from("/tmp/test.db"),
                crate::config::Config::default(),
                Arc::clone(&conn),
                None,
                false,
            )
            .with_fresh_children(fresh)
        };

        let found = |r: &ConductorChildWorkflowRunner| {
            r.find_resumable_child("parent-1", "child-wf").unwrap()
        };
        assert!(found(&make(FreshChildren::None)).is_some());
        assert!(found(&make(FreshChildren::All)).is_none());
        let named = FreshChildren::Named(["child-wf".to_string()].into_iter().collect());
        assert!(found(&make(named)).is_none());
        let other = FreshChildren::Named(["other".to_string()].into_iter().collect());
        assert!(found(&make(other)).is_some());
    }
}
