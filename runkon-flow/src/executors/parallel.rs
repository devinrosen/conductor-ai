use std::sync::Arc;

use crate::cancellation_reason::CancellationReason;
use crate::dsl::ParallelNode;
use crate::engine::{resolve_schema, ExecutionState};
use crate::engine_error::{EngineError, Result};
use crate::status::WorkflowStepStatus;
use crate::traits::action_executor::{ActionOutput, ActionParams, ExecutionContext};
use crate::traits::persistence::{NewStep, StepUpdate};
use crate::types::{StepResult, StepSuccess};

use super::p_err;

pub fn execute_parallel(
    state: &mut ExecutionState,
    node: &ParallelNode,
    iteration: u32,
) -> Result<()> {
    let group_id = ulid::Ulid::new().to_string();
    let pos_base = state.position;

    tracing::info!(
        "parallel: spawning {} agents (fail_fast={}, min_success={:?})",
        node.calls.len(),
        node.fail_fast,
        node.min_success,
    );

    // Load block-level schema (if any)
    let block_schema = node
        .output
        .as_deref()
        .map(|name| resolve_schema(state, name))
        .transpose()?;

    struct ParallelCallResult {
        agent_name: String,
        step_id: String,
        result: std::result::Result<ActionOutput, EngineError>,
        attempt: u32,
    }

    struct DispatchInput {
        step_id: String,
        agent_name: String,
        ectx: ExecutionContext,
        params: ActionParams,
    }

    let mut skipped_count = 0u32;
    let mut call_inputs: Vec<(
        usize,
        String,
        Option<crate::output_schema::OutputSchema>,
        Vec<String>,
    )> = Vec::new();

    // First pass: skip any already-completed agents on resume
    for (i, agent_ref) in node.calls.iter().enumerate() {
        let pos = pos_base + i as i64;
        state.position = pos + 1;
        let agent_label = agent_ref.label();
        let agent_step_key = agent_ref.step_key();

        if super::skip_if_already_completed(state, &agent_step_key, iteration, agent_label) {
            skipped_count += 1;
            continue;
        }

        // Determine schema for this call: per-call override > block-level
        let call_schema = node
            .call_outputs
            .get(&i.to_string())
            .map(|name| resolve_schema(state, name))
            .transpose()?;
        let effective_schema = call_schema.as_ref().or(block_schema.as_ref()).cloned();

        // Combine block-level `with` + per-call `with` additions. Only clone when
        // there are per-call extras to avoid N × snippet_total allocations.
        let effective_with = if let Some(extra) = node.call_with.get(&i.to_string()) {
            let mut w = node.with.clone();
            w.extend(extra.iter().cloned());
            w
        } else {
            node.with.clone()
        };

        call_inputs.push((i, agent_step_key.clone(), effective_schema, effective_with));
    }

    // Pre-dispatch pass: evaluate per-call `if` conditions, create step records, and build
    // the dispatch queue. All records are created before any thread is spawned so the DB
    // reflects the full parallel block immediately (important for UI and resume).
    let mut dispatch_queue: Vec<DispatchInput> = Vec::new();

    // Build the variable map once — state doesn't change between branches so there is
    // no need to re-serialize state.contexts for every parallel branch.
    let shared_inputs = super::build_inputs_map(state);

    // Parallel-scope token: child of the run root. Cancelling it signals running branches
    // to exit early when fail_fast fires.
    let scope_token = state.cancellation.child();

    for (i, _agent_step_key, call_schema, effective_with) in call_inputs {
        let pos = pos_base + i as i64;
        let agent_ref = &node.calls[i];
        let agent_label = agent_ref.label();

        // Check per-call `if` condition
        if let Some((cond_step, cond_marker)) = node.call_if.get(&i.to_string()) {
            let has_marker = state
                .step_results
                .get(cond_step)
                .map(|r| r.markers.iter().any(|m| m == cond_marker))
                .unwrap_or(false);
            if !has_marker {
                tracing::info!(
                    "parallel: skipping '{}' (if={}.{} not satisfied)",
                    agent_label,
                    cond_step,
                    cond_marker
                );
                let step_id = state
                    .persistence
                    .insert_step(NewStep {
                        workflow_run_id: state.workflow_run_id.clone(),
                        step_name: agent_label.to_string(),
                        role: "actor".to_string(),
                        can_commit: false,
                        position: pos,
                        iteration: iteration as i64,
                        retry_count: None,
                    })
                    .map_err(p_err)?;
                state
                    .persistence
                    .update_step(
                        &step_id,
                        StepUpdate {
                            status: WorkflowStepStatus::Skipped,
                            child_run_id: None,
                            result_text: Some(format!(
                                "skipped: {cond_step}.{cond_marker} not emitted"
                            )),
                            context_out: None,
                            markers_out: None,
                            retry_count: None,
                            structured_output: None,
                            step_error: None,
                        },
                    )
                    .map_err(p_err)?;
                skipped_count += 1;
                continue;
            }
        }

        let step_id = state
            .persistence
            .insert_step(NewStep {
                workflow_run_id: state.workflow_run_id.clone(),
                step_name: agent_label.to_string(),
                role: "actor".to_string(),
                can_commit: false,
                position: pos,
                iteration: iteration as i64,
                retry_count: Some(0),
            })
            .map_err(p_err)?;

        let inputs = Arc::clone(&shared_inputs);

        let ectx = super::build_execution_context(
            state,
            &step_id,
            state.default_bot_name.clone(),
            state.worktree_ctx.extra_plugin_dirs.clone(),
        );

        let params = super::build_action_params(
            agent_label,
            inputs,
            effective_with,
            state.exec_config.dry_run,
            state.last_gate_feedback.clone(),
            call_schema,
            0,
            None,
        );

        dispatch_queue.push(DispatchInput {
            step_id,
            agent_name: agent_label.to_string(),
            ectx,
            params,
        });
    }

    // Spawn all agents concurrently. Each thread checks the scope token before dispatching
    // so that a fail_fast cancellation from a result that arrives while threads are still
    // starting will prevent those threads from doing any work.
    let (completion_tx, completion_rx) = std::sync::mpsc::channel::<(
        String,
        String,
        std::result::Result<ActionOutput, EngineError>,
    )>();

    for dispatch_input in dispatch_queue {
        let tx = completion_tx.clone();
        let registry = Arc::clone(&state.action_registry);
        let scope = scope_token.clone();
        std::thread::spawn(move || {
            let result = if scope.is_cancelled() {
                Err(EngineError::Cancelled(CancellationReason::FailFast))
            } else {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    registry.dispatch(
                        &dispatch_input.params.name,
                        &dispatch_input.ectx,
                        &dispatch_input.params,
                    )
                }))
                .unwrap_or_else(|payload| {
                    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                        format!("executor '{}' panicked: {s}", dispatch_input.params.name)
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        format!("executor '{}' panicked: {s}", dispatch_input.params.name)
                    } else {
                        format!("executor '{}' panicked", dispatch_input.params.name)
                    };
                    Err(EngineError::Workflow(msg))
                })
            };
            let _ = tx.send((dispatch_input.step_id, dispatch_input.agent_name, result));
        });
    }
    // Drop the sender so the receiver knows when all threads have completed.
    drop(completion_tx);

    // Collect results as threads complete, triggering fail_fast cancellation as needed.
    let mut results: Vec<ParallelCallResult> = Vec::new();
    for (step_id, agent_name, result) in completion_rx {
        let failed = result.is_err();
        results.push(ParallelCallResult {
            agent_name,
            step_id,
            result,
            attempt: 0,
        });
        if failed && node.fail_fast {
            scope_token.cancel(CancellationReason::FailFast);
        }
    }

    // Process results
    let mut merged_markers: Vec<String> = Vec::new();
    let mut successes = 0u32;
    let mut failures = 0u32;

    for pr in &results {
        match &pr.result {
            Ok(output) => {
                let markers_json = crate::helpers::serialize_or_empty_array(
                    &output.markers,
                    &format!("parallel: '{}'", pr.agent_name),
                );
                let context = output.context.clone().unwrap_or_default();

                super::persist_completed_step(
                    state,
                    &pr.step_id,
                    output.child_run_id.clone(),
                    output.result_text.clone(),
                    Some(context.clone()),
                    Some(markers_json),
                    pr.attempt,
                    output.structured_output.clone(),
                )?;

                tracing::info!(
                    "parallel: '{}' completed (cost=${:.4})",
                    pr.agent_name,
                    output.cost_usd.unwrap_or(0.0),
                );

                successes += 1;
                merged_markers.extend(output.markers.iter().cloned());

                // Push parallel agent context
                state.contexts.push(StepSuccess {
                    step_name: pr.agent_name.clone(),
                    result_text: output.result_text.clone(),
                    cost_usd: output.cost_usd,
                    num_turns: output.num_turns,
                    duration_ms: output.duration_ms,
                    input_tokens: output.input_tokens,
                    output_tokens: output.output_tokens,
                    cache_read_input_tokens: output.cache_read_input_tokens,
                    cache_creation_input_tokens: output.cache_creation_input_tokens,
                    markers: output.markers.clone(),
                    context: context.clone(),
                    child_run_id: output.child_run_id.clone(),
                    structured_output: output.structured_output.clone(),
                    output_file: None,
                    iteration,
                }.into());

                state.accumulate_metrics(
                    output.cost_usd,
                    output.num_turns,
                    output.duration_ms,
                    output.input_tokens,
                    output.output_tokens,
                    output.cache_read_input_tokens,
                    output.cache_creation_input_tokens,
                );

                if let Err(e) = state.flush_metrics() {
                    tracing::warn!("Failed to flush mid-run metrics after parallel agent: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("parallel: '{}' failed: {e}", pr.agent_name);
                state
                    .persistence
                    .update_step(&pr.step_id, StepUpdate::failed(e.to_string(), pr.attempt))
                    .map_err(p_err)?;
                failures += 1;
            }
        }
    }

    // Apply min_success policy (skipped-on-resume agents count as successes)
    let effective_successes = successes + skipped_count;
    let total_agents = results.len() as u32 + skipped_count;
    let min_required = node.min_success.unwrap_or(total_agents);
    tracing::info!(
        "parallel: {successes} succeeded, {failures} failed, {skipped_count} skipped out of {total_agents} agents",
    );
    if effective_successes < min_required {
        tracing::warn!(
            "parallel: only {}/{} succeeded (min_success={})",
            effective_successes,
            total_agents,
            min_required
        );
        state.all_succeeded = false;
    }

    // Store merged markers as a synthetic result
    let synthetic_result = StepResult {
        step_name: format!("parallel:{}", group_id),
        status: if effective_successes >= min_required {
            WorkflowStepStatus::Completed
        } else {
            WorkflowStepStatus::Failed
        },
        result_text: None,
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: merged_markers,
        context: String::new(),
        child_run_id: None,
        structured_output: None,
        output_file: None,
    };
    state
        .step_results
        .insert(format!("parallel:{}", group_id), synthetic_result);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{AgentRef, ParallelNode};
    use crate::engine::{ExecutionState, WorktreeContext};
    use crate::engine_error::EngineError;
    use crate::persistence_memory::InMemoryWorkflowPersistence;
    use crate::status::WorkflowStepStatus;
    use crate::traits::action_executor::{ActionExecutor, ActionOutput, ActionParams};
    use crate::traits::item_provider::ItemProviderRegistry;
    use crate::traits::persistence::WorkflowPersistence;
    use crate::traits::script_env_provider::NoOpScriptEnvProvider;
    use crate::types::WorkflowExecConfig;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MarkersExecutor {
        markers: Vec<String>,
        context: String,
    }

    impl ActionExecutor for MarkersExecutor {
        fn name(&self) -> &str {
            "markers_exec"
        }
        fn execute(
            &self,
            _ectx: &crate::traits::action_executor::ExecutionContext,
            _params: &ActionParams,
        ) -> Result<ActionOutput, EngineError> {
            Ok(ActionOutput {
                markers: self.markers.clone(),
                context: Some(self.context.clone()),
                cost_usd: Some(0.01),
                num_turns: Some(2),
                ..Default::default()
            })
        }
    }

    fn make_persistence_with_run() -> (Arc<InMemoryWorkflowPersistence>, String) {
        let p = Arc::new(InMemoryWorkflowPersistence::new());
        let run = p
            .create_run(crate::traits::persistence::NewRun {
                workflow_name: "wf".to_string(),
                worktree_id: None,
                ticket_id: None,
                repo_id: None,
                parent_run_id: String::new(),
                dry_run: false,
                trigger: "manual".to_string(),
                definition_snapshot: None,
                parent_workflow_run_id: None,
                target_label: None,
            })
            .unwrap();
        (p, run.id)
    }

    fn make_state(
        persistence: Arc<InMemoryWorkflowPersistence>,
        run_id: String,
        registry: crate::traits::action_executor::ActionRegistry,
    ) -> ExecutionState {
        ExecutionState {
            persistence,
            action_registry: Arc::new(registry),
            script_env_provider: Arc::new(NoOpScriptEnvProvider),
            workflow_run_id: run_id,
            workflow_name: "wf".to_string(),
            worktree_ctx: WorktreeContext {
                worktree_id: None,
                working_dir: String::new(),
                repo_path: String::new(),
                ticket_id: None,
                repo_id: None,
                extra_plugin_dirs: vec![],
            },
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs: HashMap::new(),
            parent_run_id: String::new(),
            depth: 0,
            target_label: None,
            step_results: HashMap::new(),
            contexts: vec![],
            position: 0,
            all_succeeded: true,
            total_cost: 0.0,
            total_turns: 0,
            total_duration_ms: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            last_gate_feedback: None,
            block_output: None,
            block_with: vec![],
            resume_ctx: None,
            default_bot_name: None,
            triggered_by_hook: false,
            schema_resolver: None,
            child_runner: None,
            last_heartbeat_at: ExecutionState::new_heartbeat(),
            registry: Arc::new(ItemProviderRegistry::new()),
            event_sinks: Arc::from(vec![]),
            cancellation: crate::cancellation::CancellationToken::new(),
            current_execution_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Verifies the ActionOutput dispatch path: markers, context, and metrics from
    /// the executor are correctly extracted and stored in the step record and state.
    #[test]
    fn parallel_actionoutput_dispatch_path_records_markers_and_context() {
        let mut named = HashMap::new();
        named.insert(
            "markers_exec".to_string(),
            Box::new(MarkersExecutor {
                markers: vec!["m1".to_string(), "m2".to_string()],
                context: "step context".to_string(),
            }) as Box<dyn ActionExecutor>,
        );
        let registry = crate::traits::action_executor::ActionRegistry::new(named, None);

        let (persistence, run_id) = make_persistence_with_run();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone(), registry);

        let node = ParallelNode {
            fail_fast: false,
            min_success: None,
            calls: vec![AgentRef::Name("markers_exec".to_string())],
            output: None,
            call_outputs: HashMap::new(),
            with: vec![],
            call_with: HashMap::new(),
            call_if: HashMap::new(),
        };

        execute_parallel(&mut state, &node, 0).unwrap();

        // The step record in the DB should be Completed with correct markers.
        let steps = persistence.get_steps(&run_id).unwrap();
        assert_eq!(steps.len(), 1, "expected one step record");
        let step = &steps[0];
        assert_eq!(
            step.status,
            WorkflowStepStatus::Completed,
            "step should be Completed; got {:?}",
            step.status
        );
        let markers: Vec<String> = step
            .markers_out
            .as_deref()
            .and_then(|m| serde_json::from_str(m).ok())
            .unwrap_or_default();
        assert_eq!(
            markers,
            vec!["m1", "m2"],
            "markers should match executor output"
        );
        assert_eq!(
            step.context_out.as_deref(),
            Some("step context"),
            "context should match executor output"
        );

        // The context entry should be pushed to state.contexts.
        assert!(
            state.contexts.iter().any(|c| c.context == "step context"),
            "executor context should be in state.contexts"
        );

        // Metrics should be accumulated.
        assert!(
            state.total_cost > 0.0,
            "cost should be accumulated from ActionOutput"
        );
    }

    /// Verifies that a panicking executor is caught by `catch_unwind` and recorded as a
    /// Failed step rather than crashing the whole process.
    #[test]
    fn parallel_panicking_executor_is_caught_and_step_is_failed() {
        struct PanicExec;
        impl ActionExecutor for PanicExec {
            fn name(&self) -> &str {
                "panic_exec"
            }
            fn execute(
                &self,
                _ectx: &crate::traits::action_executor::ExecutionContext,
                _params: &ActionParams,
            ) -> Result<ActionOutput, EngineError> {
                panic!("deliberate panic in test executor");
            }
        }

        let mut named = HashMap::new();
        named.insert(
            "panic_exec".to_string(),
            Box::new(PanicExec) as Box<dyn ActionExecutor>,
        );
        let registry = crate::traits::action_executor::ActionRegistry::new(named, None);

        let (persistence, run_id) = make_persistence_with_run();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone(), registry);

        let node = ParallelNode {
            fail_fast: false,
            min_success: None,
            calls: vec![AgentRef::Name("panic_exec".to_string())],
            output: None,
            call_outputs: HashMap::new(),
            with: vec![],
            call_with: HashMap::new(),
            call_if: HashMap::new(),
        };

        // execute_parallel should succeed (the panic is caught internally).
        execute_parallel(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        assert_eq!(steps.len(), 1, "expected one step record");
        let step = &steps[0];
        assert_eq!(
            step.status,
            WorkflowStepStatus::Failed,
            "panicking executor should produce a Failed step; got {:?}",
            step.status
        );
        let error_msg = step.step_error.as_deref().unwrap_or("");
        assert!(
            error_msg.contains("panic_exec"),
            "step_error should name the executor; got: {error_msg:?}"
        );
        assert!(
            error_msg.contains("deliberate panic in test executor"),
            "step_error should include the panic payload; got: {error_msg:?}"
        );
    }

    /// Verifies that a panicking executor with a `String` payload is caught and the
    /// message is surfaced in the step error.
    #[test]
    fn parallel_panicking_executor_string_payload_is_surfaced() {
        struct PanicStringExec;
        impl ActionExecutor for PanicStringExec {
            fn name(&self) -> &str {
                "panic_string_exec"
            }
            fn execute(
                &self,
                _ectx: &crate::traits::action_executor::ExecutionContext,
                _params: &ActionParams,
            ) -> Result<ActionOutput, EngineError> {
                panic!("{}", "string payload panic".to_string())
            }
        }

        let mut named = HashMap::new();
        named.insert(
            "panic_string_exec".to_string(),
            Box::new(PanicStringExec) as Box<dyn ActionExecutor>,
        );
        let registry = crate::traits::action_executor::ActionRegistry::new(named, None);

        let (persistence, run_id) = make_persistence_with_run();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone(), registry);

        let node = ParallelNode {
            fail_fast: false,
            min_success: None,
            calls: vec![AgentRef::Name("panic_string_exec".to_string())],
            output: None,
            call_outputs: HashMap::new(),
            with: vec![],
            call_with: HashMap::new(),
            call_if: HashMap::new(),
        };

        execute_parallel(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        assert_eq!(steps.len(), 1);
        let error_msg = steps[0].step_error.as_deref().unwrap_or("");
        assert!(
            error_msg.contains("panic_string_exec"),
            "step_error should name the executor; got: {error_msg:?}"
        );
        assert!(
            error_msg.contains("string payload panic"),
            "step_error should include the String panic payload; got: {error_msg:?}"
        );
    }

    /// Verifies that a panicking executor with an unknown payload type (neither `&str`
    /// nor `String`) falls back to a generic panic message.
    #[test]
    fn parallel_panicking_executor_unknown_payload_fallback() {
        struct PanicUnknownExec;
        impl ActionExecutor for PanicUnknownExec {
            fn name(&self) -> &str {
                "panic_unknown_exec"
            }
            fn execute(
                &self,
                _ectx: &crate::traits::action_executor::ExecutionContext,
                _params: &ActionParams,
            ) -> Result<ActionOutput, EngineError> {
                std::panic::panic_any(42i32)
            }
        }

        let mut named = HashMap::new();
        named.insert(
            "panic_unknown_exec".to_string(),
            Box::new(PanicUnknownExec) as Box<dyn ActionExecutor>,
        );
        let registry = crate::traits::action_executor::ActionRegistry::new(named, None);

        let (persistence, run_id) = make_persistence_with_run();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone(), registry);

        let node = ParallelNode {
            fail_fast: false,
            min_success: None,
            calls: vec![AgentRef::Name("panic_unknown_exec".to_string())],
            output: None,
            call_outputs: HashMap::new(),
            with: vec![],
            call_with: HashMap::new(),
            call_if: HashMap::new(),
        };

        execute_parallel(&mut state, &node, 0).unwrap();

        let steps = persistence.get_steps(&run_id).unwrap();
        assert_eq!(steps.len(), 1);
        let error_msg = steps[0].step_error.as_deref().unwrap_or("");
        assert!(
            error_msg.contains("panic_unknown_exec"),
            "step_error should name the executor; got: {error_msg:?}"
        );
        // Unknown payload should produce the fallback message without a payload description.
        assert!(
            !error_msg.contains("42"),
            "step_error should NOT contain the unknown payload value; got: {error_msg:?}"
        );
    }

    /// Verifies that fail_fast marks the workflow as not-all-succeeded after the first failure.
    ///
    /// With true parallel execution all branches are spawned before any result is processed.
    /// The scope token is cancelled only when the first failure result is dequeued by the
    /// main thread; branches that already called `dispatch()` before the cancel fires will
    /// complete normally (Ok or Err depending on the executor). The exact count of Failed
    /// steps is therefore non-deterministic: it is at least 1 (the failing branch) but may
    /// be higher if racing branches also see the cancellation check. The meaningful invariant
    /// is that `all_succeeded` becomes false and at least one step is recorded as Failed.
    #[test]
    fn parallel_fail_fast_stops_after_first_failure() {
        struct FailExec;
        impl ActionExecutor for FailExec {
            fn name(&self) -> &str {
                "fail_exec"
            }
            fn execute(
                &self,
                _ectx: &crate::traits::action_executor::ExecutionContext,
                _params: &ActionParams,
            ) -> Result<ActionOutput, EngineError> {
                Err(EngineError::Workflow("intentional failure".to_string()))
            }
        }

        let mut named = HashMap::new();
        named.insert(
            "fail_exec".to_string(),
            Box::new(FailExec) as Box<dyn ActionExecutor>,
        );
        named.insert(
            "markers_exec".to_string(),
            Box::new(MarkersExecutor {
                markers: vec!["ok".to_string()],
                context: String::new(),
            }) as Box<dyn ActionExecutor>,
        );
        let registry = crate::traits::action_executor::ActionRegistry::new(named, None);

        let (persistence, run_id) = make_persistence_with_run();
        let mut state = make_state(Arc::clone(&persistence), run_id.clone(), registry);

        let node = ParallelNode {
            fail_fast: true,
            min_success: None,
            calls: vec![
                AgentRef::Name("fail_exec".to_string()),
                AgentRef::Name("markers_exec".to_string()),
                AgentRef::Name("markers_exec".to_string()),
            ],
            output: None,
            call_outputs: HashMap::new(),
            with: vec![],
            call_with: HashMap::new(),
            call_if: HashMap::new(),
        };

        execute_parallel(&mut state, &node, 0).ok();

        let steps = persistence.get_steps(&run_id).unwrap();
        let failed = steps
            .iter()
            .filter(|s| s.status == WorkflowStepStatus::Failed)
            .count();
        assert!(
            failed >= 1,
            "at least one branch should be Failed; steps: {:?}",
            steps
        );
        // The overall workflow should be marked as not all-succeeded
        assert!(
            !state.all_succeeded,
            "all_succeeded should be false when fail_fast fires"
        );
    }
}
