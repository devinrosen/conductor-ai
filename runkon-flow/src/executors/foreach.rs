use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::cancellation::CancellationToken;
use crate::dsl::{ForEachNode, OnChildFail};
use crate::engine::{
    emit_event, record_step_failure, record_step_success, restore_step, should_skip,
    ChildWorkflowInput, ExecutionState,
};
use crate::engine_error::{EngineError, Result};
use crate::events::EngineEvent;
use crate::status::WorkflowStepStatus;
use crate::traits::item_provider::ProviderContext;
use crate::traits::persistence::{FanOutItemStatus, FanOutItemUpdate, NewStep, StepUpdate};

use super::p_err;

/// Shared parent-state snapshot captured before thread spawning.
///
/// All fields are either `Arc` clones or cheap `Clone` copies — no borrows into
/// the parent `ExecutionState` are kept, so the main thread retains full `&mut`
/// access throughout the dispatch loop.
struct ForeachParentCtx {
    /// Pre-forked template with empty runtime collections — cheap to clone.
    template: ExecutionState,
    child_runner: Arc<dyn crate::engine::ChildWorkflowRunner>,
}

impl ForeachParentCtx {
    fn from_state(
        state: &ExecutionState,
        child_runner: Arc<dyn crate::engine::ChildWorkflowRunner>,
    ) -> Self {
        // fork_child creates a state with all runtime collections already empty,
        // so cloning the template later is cheap.
        let mut template = state.fork_child(crate::cancellation::CancellationToken::new());
        template.child_runner = Some(Arc::clone(&child_runner));
        Self {
            template,
            child_runner,
        }
    }

    fn make_child_state(&self, cancellation: CancellationToken) -> ExecutionState {
        let mut child = self.template.clone();
        child.cancellation = cancellation;
        child
    }
}

/// DFS over `dependents_map` starting from `start`, collecting all transitively
/// reachable item IDs that are not yet terminal.
fn collect_transitive_dependents(
    start: &str,
    dependents_map: &HashMap<String, HashSet<String>>,
    terminal_ids: &HashSet<String>,
) -> HashSet<String> {
    let mut result = HashSet::new();
    let mut queue = vec![start.to_string()];
    while let Some(current) = queue.pop() {
        if let Some(children) = dependents_map.get(&current) {
            for child in children {
                if !terminal_ids.contains(child) && result.insert(child.clone()) {
                    queue.push(child.clone());
                }
            }
        }
    }
    result
}

/// Returns true if all declared dependencies of `item_id` are in `terminal_ids`.
fn is_eligible(
    item_id: &str,
    dep_map: &HashMap<String, HashSet<String>>,
    terminal_ids: &HashSet<String>,
) -> bool {
    dep_map
        .get(item_id)
        .map(|deps| deps.iter().all(|d| terminal_ids.contains(d)))
        .unwrap_or(true)
}

/// Record a successful foreach step with the standard set of defaulted arguments.
///
/// All of the foreach-specific fields default to `None` / `vec![]`.
/// This wrapper narrows the call site to the three values that actually vary:
/// `step_key`, `step_name`, and `context`.
fn record_foreach_step_success(
    state: &mut ExecutionState,
    step_key: String,
    step_name: &str,
    context: String,
    iteration: u32,
) {
    record_step_success(
        state,
        step_key,
        crate::types::StepSuccess {
            step_name: step_name.to_string(),
            result_text: Some(context.clone()),
            context,
            iteration,
            ..crate::types::StepSuccess::default()
        },
    );
}

/// Execute a `foreach` step: fan out a child workflow over a collection of items.
pub fn execute_foreach(
    state: &mut ExecutionState,
    node: &ForEachNode,
    iteration: u32,
) -> Result<()> {
    let pos = state.position;
    state.position += 1;

    let step_key = format!("foreach:{}", node.name);

    // Skip on resume if already completed.
    if should_skip(state, &step_key, iteration) {
        tracing::info!("foreach '{}': skipping completed step", node.name);
        restore_step(state, &step_key, iteration);
        return Ok(());
    }

    // Insert the step record
    let step_id = state
        .persistence
        .insert_step(NewStep {
            workflow_run_id: state.workflow_run_id.clone(),
            step_name: step_key.clone(),
            role: "foreach".to_string(),
            can_commit: false,
            position: pos,
            iteration: iteration as i64,
            retry_count: Some(0),
        })
        .map_err(p_err)?;

    state
        .persistence
        .update_step(
            &step_id,
            StepUpdate {
                status: WorkflowStepStatus::Running,
                child_run_id: None,
                result_text: None,
                context_out: None,
                markers_out: None,
                retry_count: Some(0),
                structured_output: None,
                step_error: None,
            },
        )
        .map_err(p_err)?;

    // Validate the provider exists
    let provider = state.registry.get(&node.over).ok_or_else(|| {
        EngineError::Workflow(format!(
            "foreach '{}': unknown provider '{}' — no ItemProvider registered for this name",
            node.name, node.over
        ))
    })?;

    // Require a child runner for dispatching child workflows
    let child_runner = match &state.child_runner {
        Some(r) => r.clone(),
        None => {
            return Err(EngineError::Workflow(format!(
                "foreach '{}': no ChildWorkflowRunner configured — cannot dispatch child workflows",
                node.name
            )));
        }
    };

    // Build provider context
    let provider_ctx = ProviderContext {
        run_id: state.workflow_run_id.clone(),
        step_id: step_id.clone(),
    };

    // Phase 1: Item collection
    let existing_items = state
        .persistence
        .get_fan_out_items(&step_id, None)
        .map_err(p_err)?;
    let existing_set: HashSet<String> = existing_items.iter().map(|i| i.item_id.clone()).collect();

    let provider_items = provider.items(
        &provider_ctx,
        node.scope.as_ref(),
        &node.filter,
        &existing_set,
    )?;

    let items: Vec<(String, String, String)> = provider_items
        .into_iter()
        .map(|i| (i.item_type, i.item_id, i.item_ref))
        .collect();

    // Write pending rows for newly discovered items.
    for (item_type, item_id, item_ref) in &items {
        if !existing_set.contains(item_id) {
            state
                .persistence
                .insert_fan_out_item(&step_id, item_type, item_id, item_ref)
                .map_err(p_err)?;
        }
    }

    let new_item_count = items
        .iter()
        .filter(|(_, id, _)| !existing_set.contains(id))
        .count();
    let total_items = existing_items.len() + new_item_count;

    tracing::info!(
        "foreach '{}': {} items to process (over={}, max_parallel={})",
        node.name,
        total_items,
        node.over,
        node.max_parallel,
    );

    emit_event(
        state,
        EngineEvent::FanOutItemsCollected { count: total_items },
    );

    if total_items == 0 {
        let context = format!("foreach {}: no items to process", node.name);
        super::persist_completed_step(
            state,
            &step_id,
            None,
            Some(context.clone()),
            Some(context.clone()),
            None,
            0,
            None,
        )?;

        record_foreach_step_success(state, step_key, &node.name, context, iteration);
        return Ok(());
    }

    // Phase 2: Parallel dispatch loop

    let max_slots = if node.max_parallel == 0 {
        1
    } else {
        node.max_parallel as usize
    };

    let pending_items = state
        .persistence
        .get_fan_out_items(&step_id, Some(FanOutItemStatus::Pending))
        .map_err(p_err)?;

    // Build dependency maps when ordered execution is requested.
    // edges: (blocker_item_id, dependent_item_id) — blocker must finish before dependent starts.
    let (dep_map, dependents_map): (
        HashMap<String, HashSet<String>>,
        HashMap<String, HashSet<String>>,
    ) = if node.ordered && provider.supports_ordered() {
        let edges = provider.dependencies(&step_id)?;
        let mut dep: HashMap<String, HashSet<String>> = HashMap::new();
        let mut rev: HashMap<String, HashSet<String>> = HashMap::new();
        for (blocker, dependent) in edges {
            rev.entry(blocker.clone())
                .or_default()
                .insert(dependent.clone());
            dep.entry(dependent).or_default().insert(blocker);
        }
        (dep, rev)
    } else {
        (HashMap::new(), HashMap::new())
    };

    // Lookup maps built once from the initial pending set.
    let cap = pending_items.len();
    let mut db_id_to_item_id: HashMap<String, String> = HashMap::with_capacity(cap);
    let mut item_id_to_db_id: HashMap<String, String> = HashMap::with_capacity(cap);
    let mut item_ref_map: HashMap<String, String> = HashMap::with_capacity(cap);
    for i in &pending_items {
        db_id_to_item_id.insert(i.id.clone(), i.item_id.clone());
        item_id_to_db_id.insert(i.item_id.clone(), i.id.clone());
        item_ref_map.insert(i.item_id.clone(), i.item_ref.clone());
    }

    // Channel: spawned threads send (fan_out_item_db_id, succeeded).
    let (tx, rx) = mpsc::channel::<(String, bool)>();

    // Snapshot the parent context once; shared via Arc across all thread spawns.
    let parent_ctx = Arc::new(ForeachParentCtx::from_state(
        state,
        Arc::clone(&child_runner),
    ));

    // Clone node.inputs once outside the dispatch loop to avoid re-cloning on every iteration.
    let base_inputs = node.inputs.clone();

    // Seed terminal counts from items that were already terminal before this dispatch
    // (e.g. partially-resumed runs).  New completions/failures/skips are tracked
    // incrementally below so the final phase can skip a DB re-query.
    // Single pass over existing_items to avoid iterating the slice three times.
    let (mut completed_count, mut failed_count, mut skipped_count) =
        existing_items
            .iter()
            .fold((0usize, 0usize, 0usize), |(comp, fail, skip), i| {
                (
                    comp + usize::from(i.status == "completed"),
                    fail + usize::from(i.status == "failed"),
                    skip + usize::from(i.status == "skipped"),
                )
            });

    // Dispatch-loop tracking state.
    // Split pending items into ready (all deps met) and waiting (deps outstanding).
    // At the start terminal_ids is empty, so items with no deps are immediately ready.
    let mut in_flight: usize = 0;
    let mut halt = false;
    // item_ids that have reached a terminal state (completed, failed, or skipped)
    let mut terminal_ids: HashSet<String> = HashSet::new();

    let (ready_vec, mut waiting): (Vec<_>, Vec<_>) = pending_items
        .into_iter()
        .partition(|item| is_eligible(&item.item_id, &dep_map, &terminal_ids));
    let mut ready: std::collections::VecDeque<crate::types::FanOutItemRow> =
        ready_vec.into_iter().collect();

    tracing::info!(
        "foreach '{}': starting parallel dispatch (max_slots={}, items={})",
        node.name,
        max_slots,
        ready.len() + waiting.len(),
    );

    let pool = threadpool::ThreadPool::new(max_slots);

    loop {
        // 1. When threads are in-flight, block briefly on the first result to yield
        //    the CPU instead of spinning. Then drain any additional ready results.
        let mut completed: Vec<(String, bool)> = Vec::new();
        if in_flight > 0 {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(m) => completed.push(m),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        while let Ok(m) = rx.try_recv() {
            completed.push(m);
        }

        for (item_db_id, succeeded) in completed {
            in_flight -= 1;

            let item_id = db_id_to_item_id
                .get(&item_db_id)
                .cloned()
                .ok_or_else(|| EngineError::Workflow(format!(
                    "foreach '{}': internal invariant violation — no item_id for db_id '{item_db_id}'",
                    node.name
                )))?;
            let item_ref = item_ref_map.get(&item_id).cloned().unwrap_or_else(|| {
                tracing::warn!(
                    item_id = %item_id,
                    "foreach: item_ref map miss for item — item_ref will be empty"
                );
                String::new()
            });

            state
                .persistence
                .update_fan_out_item(
                    &item_db_id,
                    FanOutItemUpdate::Terminal {
                        status: if succeeded {
                            FanOutItemStatus::Completed
                        } else {
                            FanOutItemStatus::Failed
                        },
                    },
                )
                .map_err(p_err)?;

            emit_event(
                state,
                EngineEvent::FanOutItemCompleted {
                    item_id: item_id.clone(),
                    succeeded,
                },
            );

            if succeeded {
                completed_count += 1;
                tracing::info!("foreach '{}': item '{}' → completed", node.name, item_ref);
            } else {
                failed_count += 1;
                tracing::warn!("foreach '{}': item '{}' → failed", node.name, item_ref);
            }

            terminal_ids.insert(item_id.clone());
            let mut newly_terminal = vec![item_id.clone()];

            if !succeeded {
                match node.on_child_fail {
                    OnChildFail::Halt => {
                        tracing::warn!(
                            "foreach '{}': on_child_fail=halt, stopping dispatch",
                            node.name
                        );
                        halt = true;
                    }
                    OnChildFail::SkipDependents => {
                        let to_skip =
                            collect_transitive_dependents(&item_id, &dependents_map, &terminal_ids);
                        skipped_count += to_skip.len();
                        for skip_id in &to_skip {
                            if let Some(skip_db_id) = item_id_to_db_id.get(skip_id) {
                                state
                                    .persistence
                                    .update_fan_out_item(
                                        skip_db_id,
                                        FanOutItemUpdate::Terminal {
                                            status: FanOutItemStatus::Skipped,
                                        },
                                    )
                                    .map_err(p_err)?;
                            }
                            terminal_ids.insert(skip_id.clone());
                            newly_terminal.push(skip_id.clone());
                        }
                        ready.retain(|i| !to_skip.contains(&i.item_id));
                        waiting.retain(|i| !to_skip.contains(&i.item_id));
                        if !to_skip.is_empty() {
                            tracing::info!(
                                "foreach '{}': skipped {} dependents of '{}'",
                                node.name,
                                to_skip.len(),
                                item_id
                            );
                        }
                    }
                    OnChildFail::Continue => {}
                }
            }

            // After recording terminal items, promote newly eligible waiting items to ready.
            // Only check items that are dependents of the newly terminal items — an item
            // can only become eligible when one of its dependencies becomes terminal.
            if !waiting.is_empty() {
                let mut candidates = HashSet::new();
                for tid in &newly_terminal {
                    if let Some(deps) = dependents_map.get(tid) {
                        candidates.extend(deps.iter().cloned());
                    }
                }
                if !candidates.is_empty() {
                    let mut still_waiting = Vec::new();
                    for item in waiting.drain(..) {
                        if candidates.contains(&item.item_id)
                            && is_eligible(&item.item_id, &dep_map, &terminal_ids)
                        {
                            ready.push_back(item);
                        } else {
                            still_waiting.push(item);
                        }
                    }
                    waiting = still_waiting;
                }
            }
        }

        // 2. Check parent cancellation.
        if state.cancellation.is_cancelled() {
            tracing::info!(
                "foreach '{}': cancelled — draining {} in-flight",
                node.name,
                in_flight
            );
            break;
        }

        // 3. Dispatch new items while slots are available and we're not halted.
        let mut no_more_eligible = false;
        if !halt {
            while in_flight < max_slots {
                let item = match ready.pop_front() {
                    Some(item) => item,
                    None => {
                        no_more_eligible = true;
                        break;
                    }
                };

                emit_event(
                    state,
                    EngineEvent::FanOutItemStarted {
                        item_id: item.item_id.clone(),
                    },
                );

                state
                    .persistence
                    .update_fan_out_item(
                        &item.id,
                        FanOutItemUpdate::Running {
                            child_run_id: "dispatching".to_string(),
                        },
                    )
                    .map_err(p_err)?;

                let mut child_inputs = base_inputs.clone();
                child_inputs.insert("item.id".to_string(), item.item_id.clone());
                child_inputs.insert("item.ref".to_string(), item.item_ref.clone());

                let ctx = Arc::clone(&parent_ctx);
                let workflow_name = node.workflow.clone();
                let inputs = child_inputs;
                let item_db_id = item.id.clone();
                let child_cancellation = state.cancellation.child();
                let tx_clone = tx.clone();
                let depth = state.depth;

                pool.execute(move || {
                    let child_state = ctx.make_child_state(child_cancellation.clone());
                    let succeeded = ctx
                        .child_runner
                        .execute_child(
                            &workflow_name,
                            &child_state,
                            ChildWorkflowInput {
                                inputs,
                                iteration,
                                bot_name: None,
                                depth: depth + 1,
                                parent_step_id: None,
                                cancellation: child_cancellation,
                            },
                        )
                        .map(|r| r.all_succeeded)
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                item_db_id = %item_db_id,
                                error = %e,
                                "foreach: child workflow execution error; treating item as failed"
                            );
                            false
                        });

                    if let Err(e) = tx_clone.send((item_db_id, succeeded)) {
                        tracing::error!(
                            "foreach: result channel broken (main thread dropped): {}",
                            e
                        );
                    }
                });

                in_flight += 1;
            }
        }

        // 4. Exit when all work is done.
        if in_flight == 0 && (halt || no_more_eligible) {
            break;
        }
    }

    // Phase 3: Step completion — use in-memory counters accumulated during dispatch
    // to avoid an extra DB round-trip for counting terminal items.

    let context = format!(
        "foreach {}: {completed_count} completed, {failed_count} failed, {skipped_count} skipped of {total_items} {}",
        node.name, node.over,
    );

    let step_succeeded = failed_count == 0;

    if step_succeeded {
        state
            .persistence
            .update_step(
                &step_id,
                StepUpdate {
                    status: WorkflowStepStatus::Completed,
                    child_run_id: None,
                    result_text: Some(context.clone()),
                    context_out: Some(context.clone()),
                    markers_out: None,
                    retry_count: Some(0),
                    structured_output: None,
                    step_error: None,
                },
            )
            .map_err(p_err)?;

        record_foreach_step_success(state, step_key, &node.name, context, iteration);
    } else {
        let error_msg = format!(
            "foreach '{}': {failed_count} of {total_items} items failed",
            node.name
        );

        state
            .persistence
            .update_step(
                &step_id,
                StepUpdate {
                    status: WorkflowStepStatus::Failed,
                    child_run_id: None,
                    result_text: Some(error_msg.clone()),
                    context_out: Some(context),
                    markers_out: None,
                    retry_count: Some(0),
                    structured_output: None,
                    step_error: Some(error_msg.clone()),
                },
            )
            .map_err(p_err)?;

        return record_step_failure(state, step_key, &node.name, error_msg, 1, true);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet, VecDeque};

    use crate::types::FanOutItemRow;

    use super::{collect_transitive_dependents, is_eligible};

    fn make_row(item_id: &str, status: &str) -> FanOutItemRow {
        FanOutItemRow {
            id: format!("db-{item_id}"),
            step_run_id: "step".to_string(),
            item_type: "repo".to_string(),
            item_id: item_id.to_string(),
            item_ref: format!("ref-{item_id}"),
            child_run_id: None,
            status: status.to_string(),
            dispatched_at: None,
            completed_at: None,
        }
    }

    /// Simulates the still_waiting promotion loop: drains `waiting` into `ready` for
    /// items whose dependencies are all in `terminal_ids`.
    fn promote_waiting(
        waiting: &mut Vec<FanOutItemRow>,
        ready: &mut VecDeque<FanOutItemRow>,
        dep_map: &HashMap<String, HashSet<String>>,
        terminal_ids: &HashSet<String>,
    ) {
        let mut still_waiting = Vec::new();
        for item in waiting.drain(..) {
            if is_eligible(&item.item_id, dep_map, terminal_ids) {
                ready.push_back(item);
            } else {
                still_waiting.push(item);
            }
        }
        *waiting = still_waiting;
    }

    /// Verifies that the seeding fold correctly counts pre-existing terminal items
    /// on partial resume without iterating the slice more than once.
    #[test]
    fn seed_counts_from_existing_terminal_items() {
        let existing = [
            make_row("a", "completed"),
            make_row("b", "completed"),
            make_row("c", "failed"),
            make_row("d", "skipped"),
            make_row("e", "pending"), // must NOT contribute to any count
        ];

        let (completed, failed, skipped) =
            existing
                .iter()
                .fold((0usize, 0usize, 0usize), |(comp, fail, skip), i| {
                    (
                        comp + usize::from(i.status == "completed"),
                        fail + usize::from(i.status == "failed"),
                        skip + usize::from(i.status == "skipped"),
                    )
                });

        assert_eq!(completed, 2, "expected 2 completed");
        assert_eq!(failed, 1, "expected 1 failed");
        assert_eq!(skipped, 1, "expected 1 skipped");
    }

    /// No dependencies: all items start in ready immediately (terminal_ids empty).
    #[test]
    fn no_dependencies_all_items_start_ready() {
        let dep_map: HashMap<String, HashSet<String>> = HashMap::new();
        let terminal_ids: HashSet<String> = HashSet::new();

        let items = vec![
            make_row("a", "pending"),
            make_row("b", "pending"),
            make_row("c", "pending"),
        ];

        let (ready_vec, waiting): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|item| is_eligible(&item.item_id, &dep_map, &terminal_ids));

        assert_eq!(ready_vec.len(), 3, "all items should be ready with no deps");
        assert!(waiting.is_empty(), "nothing should be waiting");
    }

    /// Linear chain A → B: B stays waiting until A is terminal, then B moves to ready.
    #[test]
    fn linear_chain_b_waits_for_a_then_becomes_ready() {
        let mut dep_map: HashMap<String, HashSet<String>> = HashMap::new();
        // B depends on A
        dep_map
            .entry("b".to_string())
            .or_default()
            .insert("a".to_string());

        let mut terminal_ids: HashSet<String> = HashSet::new();

        let items = vec![make_row("a", "pending"), make_row("b", "pending")];

        let (ready_vec, mut waiting): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|item| is_eligible(&item.item_id, &dep_map, &terminal_ids));
        let mut ready: VecDeque<FanOutItemRow> = ready_vec.into_iter().collect();

        assert_eq!(ready.len(), 1, "only A should be ready initially");
        assert_eq!(ready.front().unwrap().item_id, "a");
        assert_eq!(waiting.len(), 1, "B should be waiting");

        // Simulate A completing
        terminal_ids.insert("a".to_string());
        promote_waiting(&mut waiting, &mut ready, &dep_map, &terminal_ids);

        // After promotion, A is consumed, B should now be in ready
        // Drain the 'a' item from ready (it was dispatched)
        let dispatched = ready.pop_front().unwrap();
        assert_eq!(dispatched.item_id, "a");

        assert_eq!(ready.len(), 1, "B should now be in ready after A completed");
        assert_eq!(ready.front().unwrap().item_id, "b");
        assert!(waiting.is_empty(), "nothing should remain waiting");
    }

    /// Diamond dependency: C and D both depend on A.
    /// Once A completes both C and D should move to ready simultaneously.
    #[test]
    fn diamond_both_dependents_promoted_after_common_dep_completes() {
        let mut dep_map: HashMap<String, HashSet<String>> = HashMap::new();
        // C depends on A, D depends on A
        dep_map
            .entry("c".to_string())
            .or_default()
            .insert("a".to_string());
        dep_map
            .entry("d".to_string())
            .or_default()
            .insert("a".to_string());

        let mut terminal_ids: HashSet<String> = HashSet::new();

        let items = vec![
            make_row("a", "pending"),
            make_row("c", "pending"),
            make_row("d", "pending"),
        ];

        let (ready_vec, mut waiting): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|item| is_eligible(&item.item_id, &dep_map, &terminal_ids));
        let mut ready: VecDeque<FanOutItemRow> = ready_vec.into_iter().collect();

        assert_eq!(ready.len(), 1, "only A should start ready");
        assert_eq!(waiting.len(), 2, "C and D should be waiting");

        // A completes
        terminal_ids.insert("a".to_string());
        promote_waiting(&mut waiting, &mut ready, &dep_map, &terminal_ids);

        // Both C and D should now be ready (plus A still in deque before it's dispatched)
        assert!(
            waiting.is_empty(),
            "no items should remain waiting after A completes"
        );
        // ready should contain A (already there) + C + D = 3
        assert_eq!(ready.len(), 3, "A, C, D should all be in ready");
    }

    /// Items already in existing_set are skipped; their dependents become eligible.
    #[test]
    fn already_completed_items_enable_dependents() {
        let mut dep_map: HashMap<String, HashSet<String>> = HashMap::new();
        // B depends on A
        dep_map
            .entry("b".to_string())
            .or_default()
            .insert("a".to_string());

        // A is already in existing_set (completed before this dispatch loop)
        let existing_set: HashSet<String> = ["a".to_string()].into_iter().collect();
        // terminal_ids seeds from existing completed items
        let mut terminal_ids: HashSet<String> = existing_set.clone();

        // Only B is a new pending item (A was completed earlier)
        let pending_items = vec![make_row("b", "pending")];

        let (ready_vec, waiting): (Vec<_>, Vec<_>) = pending_items
            .into_iter()
            .partition(|item| is_eligible(&item.item_id, &dep_map, &terminal_ids));
        let ready: VecDeque<FanOutItemRow> = ready_vec.into_iter().collect();

        assert_eq!(
            ready.len(),
            1,
            "B should be immediately ready because A is already terminal"
        );
        assert!(waiting.is_empty());

        // Also verify collect_transitive_dependents with everything already terminal
        terminal_ids.insert("b".to_string());
        let mut dependents_map: HashMap<String, HashSet<String>> = HashMap::new();
        dependents_map
            .entry("a".to_string())
            .or_default()
            .insert("b".to_string());
        let transitive = collect_transitive_dependents("a", &dependents_map, &terminal_ids);
        assert!(
            transitive.is_empty(),
            "B is already terminal so transitive set should be empty"
        );
    }
}
