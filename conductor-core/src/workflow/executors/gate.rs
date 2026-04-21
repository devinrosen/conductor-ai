use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::error::{ConductorError, Result};
use crate::workflow_dsl::{GateNode, GateOptions, GateType, OnFailAction, OnTimeout};

use crate::workflow::engine::{restore_step, should_skip, ExecutionState};
use crate::workflow::run_context::RunContext;
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};

use super::gate_resolver::{
    build_default_gate_resolvers, GateContext, GateParams, GatePoll, GitHubTokenCache,
};

pub fn execute_gate(state: &mut ExecutionState<'_>, node: &GateNode, iteration: u32) -> Result<()> {
    let working_dir: String = {
        let ctx = crate::workflow::run_context::WorktreeRunContext::new(state);
        ctx.working_dir().to_string_lossy().into_owned()
    };
    let pos = state.position;
    state.position += 1;

    // Skip completed gates on resume — restore feedback for downstream steps
    if should_skip(state, &node.name, iteration) {
        tracing::info!("Skipping completed gate '{}'", node.name);
        restore_step(state, &node.name, iteration);
        return Ok(());
    }

    // Quality gates evaluate immediately — no blocking/waiting.
    if node.gate_type == GateType::QualityGate {
        return execute_quality_gate(state, node, pos, iteration);
    }

    // Dry-run: auto-approve all gates
    if state.exec_config.dry_run {
        tracing::info!("gate '{}': dry-run auto-approved", node.name);
        let step_id = state.wf_mgr.insert_step(
            &state.workflow_run_id,
            &node.name,
            "reviewer",
            false,
            pos,
            iteration as i64,
        )?;
        state.wf_mgr.update_step_status(
            &step_id,
            WorkflowStepStatus::Completed,
            None,
            Some("dry-run: auto-approved"),
            None,
            None,
            None,
        )?;
        return Ok(());
    }

    let step_id = state.wf_mgr.insert_step(
        &state.workflow_run_id,
        &node.name,
        "gate",
        false,
        pos,
        iteration as i64,
    )?;

    state.wf_mgr.set_step_gate_info(
        &step_id,
        node.gate_type.clone(),
        node.prompt.as_deref(),
        &format!("{}s", node.timeout_secs),
    )?;

    state.wf_mgr.update_step_status(
        &step_id,
        WorkflowStepStatus::Waiting,
        None,
        None,
        None,
        None,
        None,
    )?;

    // Resolve gate options (if any) and persist them.
    let resolved_options: Vec<String> = if let Some(ref gate_opts) = node.options {
        match gate_opts {
            GateOptions::Static(items) => items.clone(),
            GateOptions::StepRef(dotted) => {
                // Split "step.field" — everything before the first dot is the step key.
                let dot = dotted.find('.').ok_or_else(|| {
                    ConductorError::Workflow(format!(
                        "Gate '{}': options StepRef '{dotted}' must be in 'step.field' format",
                        node.name
                    ))
                })?;
                let step_key = &dotted[..dot];
                let field_key = &dotted[dot + 1..];
                let result = state.step_results.get(step_key).ok_or_else(|| {
                    ConductorError::Workflow(format!(
                        "Gate '{}': options StepRef references step '{step_key}' which has no result yet",
                        node.name
                    ))
                })?;
                let json_str = result.structured_output.as_deref().ok_or_else(|| {
                    ConductorError::Workflow(format!(
                        "Gate '{}': step '{step_key}' has no structured_output to extract field '{field_key}' from",
                        node.name
                    ))
                })?;
                let val: serde_json::Value = serde_json::from_str(json_str).map_err(|e| {
                    ConductorError::Workflow(format!(
                        "Gate '{}': failed to parse structured_output of step '{step_key}': {e}",
                        node.name
                    ))
                })?;
                let arr = val.get(field_key).and_then(|v| v.as_array()).ok_or_else(|| {
                    ConductorError::Workflow(format!(
                        "Gate '{}': field '{field_key}' in step '{step_key}' structured_output is not a JSON array",
                        node.name
                    ))
                })?;
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            }
        }
    } else {
        Vec::new()
    };

    if !resolved_options.is_empty() {
        // Persist as [{value, label}] JSON for downstream consumers.
        let opts_json: Vec<serde_json::Value> = resolved_options
            .iter()
            .map(|s| serde_json::json!({"value": s, "label": s}))
            .collect();
        let opts_str = serde_json::to_string(&opts_json).map_err(|e| {
            ConductorError::Workflow(format!("Failed to serialize gate options: {}", e))
        })?;
        state.wf_mgr.set_step_gate_options(&step_id, &opts_str)?;
    }

    // Atomically set status=Waiting and blocked_on in a single DB statement so
    // there is no observable window where status=Waiting but blocked_on=NULL.
    let gate_name = node.name.clone();
    let blocked_on = match node.gate_type {
        GateType::HumanApproval => crate::workflow::types::BlockedOn::HumanApproval {
            gate_name,
            prompt: node.prompt.clone(),
            options: resolved_options.clone(),
        },
        GateType::HumanReview => crate::workflow::types::BlockedOn::HumanReview {
            gate_name,
            prompt: node.prompt.clone(),
            options: resolved_options.clone(),
        },
        GateType::PrApproval => crate::workflow::types::BlockedOn::PrApproval {
            gate_name,
            approvals_needed: node.min_approvals,
        },
        GateType::PrChecks => crate::workflow::types::BlockedOn::PrChecks { gate_name },
        GateType::QualityGate => unreachable!("quality gates are handled above"),
    };
    state
        .wf_mgr
        .set_waiting_blocked_on(&state.workflow_run_id, &blocked_on)?;

    // Log human gate instructions before entering the poll loop.
    if matches!(
        node.gate_type,
        GateType::HumanApproval | GateType::HumanReview
    ) {
        tracing::info!("Gate '{}' waiting for human action:", node.name);
        if let Some(ref p) = node.prompt {
            tracing::info!("  Prompt: {p}");
        }
        tracing::info!(
            "  Approve:  conductor workflow gate-approve {}",
            state.workflow_run_id
        );
        tracing::info!(
            "  Reject:   conductor workflow gate-reject {}",
            state.workflow_run_id
        );
        if node.gate_type == GateType::HumanReview {
            tracing::info!(
                "  Feedback: conductor workflow gate-feedback {} \"<text>\"",
                state.workflow_run_id
            );
        }
    } else if node.gate_type == GateType::PrApproval {
        tracing::info!("Gate '{}' polling for PR approvals...", node.name);
    } else if node.gate_type == GateType::PrChecks {
        tracing::info!("Gate '{}' polling for PR checks...", node.name);
    }

    // Resolve the gate type string for registry dispatch.
    let gate_type_str = match node.gate_type {
        GateType::HumanApproval => "human_approval",
        GateType::HumanReview => "human_review",
        GateType::PrApproval => "pr_approval",
        GateType::PrChecks => "pr_checks",
        GateType::QualityGate => unreachable!(),
    };

    // Resolve the actual DB file path from the connection so that the
    // HumanApprovalGateResolver opens the right DB (important in tests that
    // use named temp files instead of the default conductor.db location).
    let db_path: std::path::PathBuf = state
        .conn
        .query_row("PRAGMA database_list", [], |row| row.get::<_, String>(2))
        .ok()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::config::db_path);

    // Build the token cache and resolver registry.
    let token_cache = Arc::new(GitHubTokenCache::new(None));
    let resolvers = build_default_gate_resolvers(db_path.clone());

    let resolver = resolvers.get(gate_type_str).ok_or_else(|| {
        ConductorError::Workflow(format!("no registered GateResolver for '{gate_type_str}'"))
    })?;

    let params = GateParams {
        gate_name: node.name.clone(),
        prompt: node.prompt.clone(),
        min_approvals: node.min_approvals,
        approval_mode: node.approval_mode.clone(),
        options: resolved_options,
        timeout_secs: node.timeout_secs,
        bot_name: node.bot_name.clone(),
        step_id: step_id.clone(),
    };

    let ctx = GateContext {
        working_dir: &working_dir,
        config: state.config,
        default_bot_name: state.default_bot_name.as_deref(),
        token_cache,
        db_path: &db_path,
    };

    // Poll/timeout loop — dispatcher owns this; resolvers are pure "poll once".
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(node.timeout_secs) {
            return handle_gate_timeout(state, &step_id, node);
        }

        match resolver.poll(&state.workflow_run_id, &params, &ctx)? {
            GatePoll::Approved(feedback) => {
                tracing::info!("Gate '{}' approved", node.name);
                state.wf_mgr.approve_gate(&step_id, "gh", None, None)?;
                if let Some(ref fb) = feedback {
                    state.last_gate_feedback = Some(fb.clone());
                }
                state.wf_mgr.update_workflow_status(
                    &state.workflow_run_id,
                    WorkflowRunStatus::Running,
                    None,
                    None,
                )?;
                return Ok(());
            }
            GatePoll::Rejected(reason) => {
                tracing::warn!("Gate '{}' rejected", node.name);
                state.all_succeeded = false;
                state.wf_mgr.update_workflow_status(
                    &state.workflow_run_id,
                    WorkflowRunStatus::Running,
                    None,
                    None,
                )?;
                return Err(ConductorError::Workflow(reason));
            }
            GatePoll::Pending => {
                thread::sleep(state.exec_config.poll_interval);
            }
        }
    }
}

/// Evaluate a quality gate by checking a prior step's structured output against a threshold.
///
/// Quality gates are non-blocking: they evaluate immediately by reading the
/// `structured_output` from `step_results` for the configured `source` step,
/// parsing the JSON, and comparing the `confidence` field against `threshold`.
pub fn execute_quality_gate(
    state: &mut ExecutionState<'_>,
    node: &GateNode,
    pos: i64,
    iteration: u32,
) -> Result<()> {
    let qg = node.quality_gate.as_ref().ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Quality gate '{}' is missing required quality_gate configuration (source, threshold)",
            node.name
        ))
    })?;
    let source = qg.source.as_str();
    let threshold = qg.threshold;
    let on_fail_action = qg.on_fail_action.clone();

    let step_id = state.wf_mgr.insert_step(
        &state.workflow_run_id,
        &node.name,
        "gate",
        false,
        pos,
        iteration as i64,
    )?;

    // Helper: update_step_status with no run_id, cost, duration, or attempt fields.
    let set_step_status = |status: WorkflowStepStatus, context: &str| -> Result<()> {
        state
            .wf_mgr
            .update_step_status(&step_id, status, None, Some(context), None, None, None)
    };

    // Look up the source step's structured output
    let (confidence, degradation_reason): (u32, Option<String>) = match state
        .step_results
        .get(source)
    {
        Some(result) => {
            if let Some(ref json_str) = result.structured_output {
                // Parse JSON and extract confidence field
                match serde_json::from_str::<serde_json::Value>(json_str) {
                    Ok(val) => {
                        // Try integer first, then fall back to float.
                        // Clamp to 100 to prevent u64→u32 truncation from wrapping
                        // large values into the passing range.
                        if let Some(c) = val.get("confidence").and_then(|v| v.as_u64()) {
                            (c.min(100) as u32, None)
                        } else if let Some(f) = val.get("confidence").and_then(|v| v.as_f64()) {
                            ((f as u64).min(100) as u32, None)
                        } else {
                            let reason = format!(
                                    "'confidence' key missing or not a number in structured output from '{}'",
                                    source
                                );
                            tracing::warn!("quality_gate '{}': {}", node.name, reason);
                            (0, Some(reason))
                        }
                    }
                    Err(e) => {
                        let reason =
                            format!("failed to parse structured output from '{}': {}", source, e);
                        tracing::warn!("quality_gate '{}': {}", node.name, reason);
                        (0, Some(reason))
                    }
                }
            } else {
                let reason = format!("source step '{}' has no structured output", source);
                tracing::warn!("quality_gate '{}': {}", node.name, reason);
                (0, Some(reason))
            }
        }
        None => {
            let msg = format!(
                "Quality gate '{}': source step '{}' not found in step results",
                node.name, source
            );
            set_step_status(WorkflowStepStatus::Failed, &msg)?;
            return Err(ConductorError::Workflow(msg));
        }
    };

    let passed = confidence >= threshold;
    let mut context = format!(
        "quality_gate: confidence={}, threshold={}, result={}",
        confidence,
        threshold,
        if passed { "pass" } else { "fail" }
    );
    if let Some(ref reason) = degradation_reason {
        context.push_str(&format!(" (confidence defaulted to 0: {})", reason));
    }

    if passed {
        tracing::info!(
            "quality_gate '{}': passed (confidence {} >= threshold {})",
            node.name,
            confidence,
            threshold
        );
        set_step_status(WorkflowStepStatus::Completed, &context)?;
    } else {
        tracing::warn!(
            "quality_gate '{}': failed (confidence {} < threshold {})",
            node.name,
            confidence,
            threshold
        );
        match on_fail_action {
            OnFailAction::Fail => {
                set_step_status(WorkflowStepStatus::Failed, &context)?;
                return Err(ConductorError::Workflow(format!(
                    "Quality gate '{}' failed: confidence {} is below threshold {}",
                    node.name, confidence, threshold
                )));
            }
            OnFailAction::Continue => {
                set_step_status(
                    WorkflowStepStatus::Completed,
                    &format!("{} (on_fail=continue, proceeding)", context),
                )?;
            }
        }
    }

    Ok(())
}

pub fn handle_gate_timeout(
    state: &mut ExecutionState<'_>,
    step_id: &str,
    node: &GateNode,
) -> Result<()> {
    tracing::warn!("Gate '{}' timed out", node.name);
    match node.on_timeout {
        OnTimeout::Fail => {
            state.wf_mgr.update_step_status(
                step_id,
                WorkflowStepStatus::Failed,
                None,
                Some("gate timed out"),
                None,
                None,
                None,
            )?;
            state.all_succeeded = false;
            state.wf_mgr.update_workflow_status(
                &state.workflow_run_id,
                WorkflowRunStatus::Running,
                None,
                None,
            )?;
            Err(ConductorError::Workflow(format!(
                "Gate '{}' timed out",
                node.name
            )))
        }
        OnTimeout::Continue => {
            state.wf_mgr.update_step_status(
                step_id,
                WorkflowStepStatus::TimedOut,
                None,
                Some("gate timed out (continuing)"),
                None,
                None,
                None,
            )?;
            state.wf_mgr.update_workflow_status(
                &state.workflow_run_id,
                WorkflowRunStatus::Running,
                None,
                None,
            )?;
            Ok(())
        }
    }
}
