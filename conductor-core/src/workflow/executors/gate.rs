use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::error::{ConductorError, Result};
use crate::workflow_dsl::{ApprovalMode, GateNode, GateOptions, GateType, OnFailAction, OnTimeout};

use crate::workflow::engine::{restore_step, should_skip, ExecutionState};
use crate::workflow::status::{WorkflowRunStatus, WorkflowStepStatus};

pub fn execute_gate(state: &mut ExecutionState<'_>, node: &GateNode, iteration: u32) -> Result<()> {
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
            options: resolved_options,
        },
        GateType::HumanReview => crate::workflow::types::BlockedOn::HumanReview {
            gate_name,
            prompt: node.prompt.clone(),
            options: resolved_options,
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

    // Capture the bot name used for this gate (resolved fresh on each poll to avoid
    // using an expired installation token in long-running gate loops).
    let gate_effective_bot: Option<String> = node
        .bot_name
        .clone()
        .or_else(|| state.default_bot_name.clone());
    let gate_config = state.config;
    // Cache the installation token so we don't make a live HTTPS call on every
    // poll iteration.  Installation tokens are valid for 1 hour; we refresh
    // after 55 minutes to stay well inside that window.
    // Cache entry: (token_or_none, fetched_at).  `None` token means the last
    // fetch failed; those entries use a short 30-second TTL so we don't
    // hammer a misconfigured GitHub App key on every 5-second poll tick.
    let gate_token_cache: std::cell::RefCell<Option<(Option<String>, std::time::Instant)>> =
        std::cell::RefCell::new(None);
    let resolve_gate_token = || -> Option<String> {
        if gate_effective_bot.is_none() && gate_config.github.app.is_none() {
            return None;
        }
        let mut cache = gate_token_cache.borrow_mut();
        let needs_refresh = cache
            .as_ref()
            .map(|(cached_token, fetched_at)| {
                let ttl = if cached_token.is_some() {
                    Duration::from_secs(55 * 60)
                } else {
                    // Short retry TTL for failed fetches.
                    Duration::from_secs(30)
                };
                fetched_at.elapsed() > ttl
            })
            .unwrap_or(true);
        if needs_refresh {
            let token = crate::github_app::resolve_named_app_token(
                gate_config,
                gate_effective_bot.as_deref(),
                "gate",
            )
            .token()
            .map(String::from);
            // Always write to cache — on failure we store None with a short
            // TTL so repeated poll ticks don't retrigger the subprocess call.
            *cache = Some((token.clone(), std::time::Instant::now()));
            token
        } else {
            cache.as_ref().and_then(|(t, _)| t.clone())
        }
    };

    match node.gate_type {
        GateType::HumanApproval | GateType::HumanReview => {
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

            // Poll DB for approval
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                // Check if gate has been approved/rejected.
                // Use find_waiting_gate as a fast path, fall back to reading the
                // step directly when our gate is no longer the active waiting gate.
                let resolved_step =
                    if let Some(step) = state.wf_mgr.find_waiting_gate(&state.workflow_run_id)? {
                        if step.id == step_id {
                            Some(step)
                        } else {
                            // Another gate is now waiting — ours must have been resolved
                            state.wf_mgr.get_step_by_id(&step_id)?
                        }
                    } else {
                        // No waiting gate — ours must have been resolved
                        state.wf_mgr.get_step_by_id(&step_id)?
                    };

                if let Some(ref step) = resolved_step {
                    if step.gate_approved_at.is_some()
                        || step.status == WorkflowStepStatus::Completed
                    {
                        tracing::info!("Gate '{}' approved", node.name);
                        if let Some(ref feedback) = step.gate_feedback {
                            state.last_gate_feedback = Some(feedback.clone());
                        }
                        state.wf_mgr.update_workflow_status(
                            &state.workflow_run_id,
                            WorkflowRunStatus::Running,
                            None,
                        )?;
                        return Ok(());
                    }
                    if step.status == WorkflowStepStatus::Failed {
                        tracing::warn!("Gate '{}' rejected", node.name);
                        state.all_succeeded = false;
                        state.wf_mgr.update_workflow_status(
                            &state.workflow_run_id,
                            WorkflowRunStatus::Running,
                            None,
                        )?;
                        return Err(ConductorError::Workflow(format!(
                            "Gate '{}' rejected",
                            node.name
                        )));
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::PrApproval => {
            tracing::info!("Gate '{}' polling for PR approvals...", node.name);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                let gate_bot_token = resolve_gate_token();
                match node.approval_mode {
                    ApprovalMode::MinApprovals => {
                        // Poll gh pr view for raw approval count
                        let mut cmd = Command::new("gh");
                        cmd.args(["pr", "view", "--json", "reviews,author"])
                            .current_dir(&state.working_dir);
                        if let Some(ref token) = gate_bot_token {
                            cmd.env("GH_TOKEN", token);
                        }
                        let output = cmd.output();

                        if let Ok(out) = output {
                            if out.status.success() {
                                let json_str = String::from_utf8_lossy(&out.stdout);
                                if let Ok(val) =
                                    serde_json::from_str::<serde_json::Value>(&json_str)
                                {
                                    let pr_author =
                                        val["author"]["login"].as_str().unwrap_or("").to_string();
                                    let approvals = val["reviews"]
                                        .as_array()
                                        .map(|reviews| {
                                            reviews
                                                .iter()
                                                .filter(|r| {
                                                    r["state"].as_str() == Some("APPROVED")
                                                        && r["author"]["login"]
                                                            .as_str()
                                                            .unwrap_or("")
                                                            != pr_author
                                                })
                                                .count()
                                                as u32
                                        })
                                        .unwrap_or(0);
                                    if approvals >= node.min_approvals {
                                        tracing::info!(
                                            "Gate '{}': {} approvals (required {})",
                                            node.name,
                                            approvals,
                                            node.min_approvals
                                        );
                                        state.wf_mgr.approve_gate(&step_id, "gh", None, None)?;
                                        state.wf_mgr.update_workflow_status(
                                            &state.workflow_run_id,
                                            WorkflowRunStatus::Running,
                                            None,
                                        )?;
                                        return Ok(());
                                    }
                                }
                            }
                        }
                    }
                    ApprovalMode::ReviewDecision => {
                        // Poll gh pr view for GitHub's branch-protection-aware reviewDecision
                        let mut cmd = Command::new("gh");
                        cmd.args(["pr", "view", "--json", "reviewDecision"])
                            .current_dir(&state.working_dir);
                        if let Some(ref token) = gate_bot_token {
                            cmd.env("GH_TOKEN", token);
                        }
                        let output = cmd.output();

                        if let Ok(out) = output {
                            if out.status.success() {
                                let json_str = String::from_utf8_lossy(&out.stdout);
                                if let Ok(val) =
                                    serde_json::from_str::<serde_json::Value>(&json_str)
                                {
                                    let decision = val["reviewDecision"].as_str().unwrap_or("");
                                    tracing::info!(
                                        "Gate '{}': reviewDecision = {}",
                                        node.name,
                                        decision
                                    );
                                    if decision == "APPROVED" {
                                        state.wf_mgr.approve_gate(&step_id, "gh", None, None)?;
                                        state.wf_mgr.update_workflow_status(
                                            &state.workflow_run_id,
                                            WorkflowRunStatus::Running,
                                            None,
                                        )?;
                                        return Ok(());
                                    }
                                    // CHANGES_REQUESTED or REVIEW_REQUIRED: keep polling
                                }
                            }
                        }
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::PrChecks => {
            tracing::info!("Gate '{}' polling for PR checks...", node.name);
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(node.timeout_secs) {
                    return handle_gate_timeout(state, &step_id, node);
                }

                let gate_bot_token = resolve_gate_token();
                let mut cmd = Command::new("gh");
                cmd.args(["pr", "checks", "--json", "state"])
                    .current_dir(&state.working_dir);
                if let Some(ref token) = gate_bot_token {
                    cmd.env("GH_TOKEN", token);
                }
                let output = cmd.output();

                if let Ok(out) = output {
                    if out.status.success() {
                        let json_str = String::from_utf8_lossy(&out.stdout);
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            if let Some(checks) = val.as_array() {
                                let all_pass = !checks.is_empty()
                                    && checks.iter().all(|c| {
                                        c["state"].as_str() == Some("SUCCESS")
                                            || c["state"].as_str() == Some("SKIPPED")
                                    });
                                if all_pass {
                                    tracing::info!("Gate '{}': all checks passing", node.name);
                                    state.wf_mgr.approve_gate(&step_id, "gh", None, None)?;
                                    state.wf_mgr.update_workflow_status(
                                        &state.workflow_run_id,
                                        WorkflowRunStatus::Running,
                                        None,
                                    )?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }

                thread::sleep(state.exec_config.poll_interval);
            }
        }
        GateType::QualityGate => {
            // Quality gates are handled earlier in execute_gate via execute_quality_gate.
            unreachable!("quality gates should not reach the blocking gate poll loop");
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
            )?;
            Ok(())
        }
    }
}
