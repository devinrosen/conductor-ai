use std::collections::HashSet;

use crate::error::Result;
use crate::workflow_dsl::{Condition, DoNode, DoWhileNode, IfNode, UnlessNode, WhileNode};

use crate::workflow::engine::{
    check_max_iterations, check_stuck, execute_nodes, execute_single_node, ExecutionState,
};
use crate::workflow::helpers::find_max_completed_while_iteration;

pub fn eval_condition(state: &ExecutionState<'_>, condition: &Condition) -> bool {
    match condition {
        Condition::StepMarker { step, marker } => state
            .step_results
            .get(step)
            .map(|r| r.markers.iter().any(|m| m == marker))
            .unwrap_or(false),
        Condition::BoolInput { input } => state
            .inputs
            .get(input)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    }
}

pub fn execute_if(state: &mut ExecutionState<'_>, node: &IfNode) -> Result<()> {
    let condition_met = eval_condition(state, &node.condition);

    if condition_met {
        tracing::info!(condition = ?node.condition, "if — condition met, executing body");
        execute_nodes(state, &node.body, true)?;
    } else {
        tracing::info!(condition = ?node.condition, "if — condition not met, skipping");
    }

    Ok(())
}

pub fn execute_unless(state: &mut ExecutionState<'_>, node: &UnlessNode) -> Result<()> {
    let condition_met = eval_condition(state, &node.condition);

    if !condition_met {
        tracing::info!(condition = ?node.condition, "unless — condition not met, executing body");
        execute_nodes(state, &node.body, true)?;
    } else {
        tracing::info!(condition = ?node.condition, "unless — condition met, skipping");
    }

    Ok(())
}

pub fn execute_while(state: &mut ExecutionState<'_>, node: &WhileNode) -> Result<()> {
    // On resume, determine the last completed iteration so we can fast-forward
    let start_iteration = if state.resume_ctx.is_some() {
        find_max_completed_while_iteration(state, node)
    } else {
        0u32
    };
    let mut iteration = start_iteration;
    let mut prev_marker_sets: Vec<HashSet<String>> = Vec::new();

    loop {
        // Check condition
        let has_marker = state
            .step_results
            .get(&node.step)
            .map(|r| r.markers.iter().any(|m| m == &node.marker))
            .unwrap_or(false);

        if !has_marker {
            tracing::info!(
                "while {}.{} — condition no longer met after {} iterations",
                node.step,
                node.marker,
                iteration
            );
            break;
        }

        if check_max_iterations(
            state,
            iteration,
            node.max_iterations,
            &node.on_max_iter,
            &node.step,
            &node.marker,
            "while",
        )? {
            break;
        }

        tracing::info!(
            "while {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body
        for body_node in &node.body {
            execute_single_node(state, body_node, iteration)?;

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            check_stuck(
                state,
                &mut prev_marker_sets,
                &node.step,
                &node.marker,
                stuck_after,
                "while",
            )?;
        }

        iteration += 1;
    }

    Ok(())
}

pub fn execute_do_while(state: &mut ExecutionState<'_>, node: &DoWhileNode) -> Result<()> {
    let mut iteration = 0u32;
    let mut prev_marker_sets: Vec<HashSet<String>> = Vec::new();

    loop {
        if check_max_iterations(
            state,
            iteration,
            node.max_iterations,
            &node.on_max_iter,
            &node.step,
            &node.marker,
            "do",
        )? {
            break;
        }

        tracing::info!(
            "do {}.{} — iteration {}/{}",
            node.step,
            node.marker,
            iteration + 1,
            node.max_iterations
        );

        // Execute body first (do-while: body always runs before condition check)
        for body_node in &node.body {
            execute_single_node(state, body_node, iteration)?;

            if !state.all_succeeded && state.exec_config.fail_fast {
                return Ok(());
            }
        }

        // Check condition after body
        let has_marker = state
            .step_results
            .get(&node.step)
            .map(|r| r.markers.iter().any(|m| m == &node.marker))
            .unwrap_or(false);

        // Stuck detection
        if let Some(stuck_after) = node.stuck_after {
            check_stuck(
                state,
                &mut prev_marker_sets,
                &node.step,
                &node.marker,
                stuck_after,
                "do",
            )?;
        }

        if !has_marker {
            tracing::info!(
                "do {}.{} — condition no longer met after {} iterations",
                node.step,
                node.marker,
                iteration + 1
            );
            break;
        }

        iteration += 1;
    }

    Ok(())
}

pub fn execute_do(state: &mut ExecutionState<'_>, node: &DoNode) -> Result<()> {
    tracing::info!(
        "do block: executing {} body nodes sequentially",
        node.body.len()
    );

    // Save and apply block-level output/with so nested calls can inherit them
    let saved_output = state.block_output.clone();
    let saved_with = state.block_with.clone();

    if node.output.is_some() {
        state.block_output = node.output.clone();
    }
    if !node.with.is_empty() {
        // Prepend block's with to any outer block_with already in state
        let mut combined = node.with.clone();
        combined.extend(saved_with.iter().cloned());
        state.block_with = combined;
    }

    for body_node in &node.body {
        if let Err(e) = execute_single_node(state, body_node, 0) {
            // Restore block-level context before propagating so that
            // always-blocks and subsequent nodes don't inherit do-block state.
            state.block_output = saved_output;
            state.block_with = saved_with;
            return Err(e);
        }
        if !state.all_succeeded && state.exec_config.fail_fast {
            break;
        }
    }

    // Restore block-level context
    state.block_output = saved_output;
    state.block_with = saved_with;

    Ok(())
}
