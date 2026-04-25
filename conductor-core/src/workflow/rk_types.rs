//! Type converters between runkon-flow and conductor-core persistence types.
//!
//! Lives here (not in `persistence_sqlite`) so that `runkon_bridge` and `engine` can
//! import converters without depending on the persistence module.
//!
//! This module, along with `runkon_bridge` and `runkon_gate_bridge`, is migration
//! scaffolding for Phase 3.x of the FlowEngine migration. These bridge modules are
//! temporary glue that will be removed once the legacy conductor-core execution stack
//! is deleted in Phase 3.3.

use crate::workflow::manager::FanOutItemRow as CoreFanOutItemRow;
use crate::workflow::persistence::{
    FanOutItemStatus as CoreFanOutItemStatus, FanOutItemUpdate as CoreFanOutItemUpdate,
    GateApprovalState as CoreGateApprovalState, NewRun as CoreNewRun, NewStep as CoreNewStep,
    StepUpdate as CoreStepUpdate,
};
use crate::workflow::types::{
    BlockedOn as CoreBlockedOn, WorkflowRun as CoreRun, WorkflowRunStep as CoreStep,
};
use runkon_flow::traits::persistence::{
    FanOutItemStatus as RkFanOutItemStatus, FanOutItemUpdate as RkFanOutItemUpdate,
    GateApprovalState as RkGateApprovalState, NewRun as RkNewRun, NewStep as RkNewStep,
    StepUpdate as RkStepUpdate,
};
use runkon_flow::types::{
    BlockedOn as RkBlockedOn, FanOutItemRow as RkFanOutItemRow, WorkflowRun as RkRun,
    WorkflowRunStep as RkStep,
};

pub fn run_status_to_core(
    s: runkon_flow::status::WorkflowRunStatus,
) -> crate::workflow::status::WorkflowRunStatus {
    use crate::workflow::status::WorkflowRunStatus as Core;
    use runkon_flow::status::WorkflowRunStatus as Rk;
    match s {
        Rk::Pending => Core::Pending,
        Rk::Running => Core::Running,
        Rk::Completed => Core::Completed,
        Rk::Failed => Core::Failed,
        Rk::Cancelled => Core::Cancelled,
        Rk::Waiting => Core::Waiting,
        Rk::NeedsResume => Core::NeedsResume,
        Rk::Cancelling => Core::Cancelling,
    }
}

pub fn run_status_to_rk(
    s: crate::workflow::status::WorkflowRunStatus,
) -> runkon_flow::status::WorkflowRunStatus {
    use crate::workflow::status::WorkflowRunStatus as Core;
    use runkon_flow::status::WorkflowRunStatus as Rk;
    match s {
        Core::Pending => Rk::Pending,
        Core::Running => Rk::Running,
        Core::Completed => Rk::Completed,
        Core::Failed => Rk::Failed,
        Core::Cancelled => Rk::Cancelled,
        Core::Waiting => Rk::Waiting,
        Core::NeedsResume => Rk::NeedsResume,
        Core::Cancelling => Rk::Cancelling,
    }
}

pub fn step_status_to_core(
    s: runkon_flow::status::WorkflowStepStatus,
) -> crate::workflow::status::WorkflowStepStatus {
    use crate::workflow::status::WorkflowStepStatus as Core;
    use runkon_flow::status::WorkflowStepStatus as Rk;
    match s {
        Rk::Pending => Core::Pending,
        Rk::Running => Core::Running,
        Rk::Completed => Core::Completed,
        Rk::Failed => Core::Failed,
        Rk::Skipped => Core::Skipped,
        Rk::Waiting => Core::Waiting,
        Rk::TimedOut => Core::TimedOut,
    }
}

pub fn step_status_to_rk(
    s: crate::workflow::status::WorkflowStepStatus,
) -> runkon_flow::status::WorkflowStepStatus {
    use crate::workflow::status::WorkflowStepStatus as Core;
    use runkon_flow::status::WorkflowStepStatus as Rk;
    match s {
        Core::Pending => Rk::Pending,
        Core::Running => Rk::Running,
        Core::Completed => Rk::Completed,
        Core::Failed => Rk::Failed,
        Core::Skipped => Rk::Skipped,
        Core::Waiting => Rk::Waiting,
        Core::TimedOut => Rk::TimedOut,
    }
}

pub fn new_run_to_core(r: RkNewRun) -> CoreNewRun {
    CoreNewRun {
        workflow_name: r.workflow_name,
        worktree_id: r.worktree_id,
        ticket_id: r.ticket_id,
        repo_id: r.repo_id,
        parent_run_id: r.parent_run_id,
        dry_run: r.dry_run,
        trigger: r.trigger,
        definition_snapshot: r.definition_snapshot,
        parent_workflow_run_id: r.parent_workflow_run_id,
        target_label: r.target_label,
    }
}

pub fn new_step_to_core(s: RkNewStep) -> CoreNewStep {
    CoreNewStep {
        workflow_run_id: s.workflow_run_id,
        step_name: s.step_name,
        role: s.role,
        can_commit: s.can_commit,
        position: s.position,
        iteration: s.iteration,
        retry_count: s.retry_count,
    }
}

pub fn step_update_to_core(u: RkStepUpdate) -> CoreStepUpdate {
    CoreStepUpdate {
        status: step_status_to_core(u.status),
        child_run_id: u.child_run_id,
        result_text: u.result_text,
        context_out: u.context_out,
        markers_out: u.markers_out,
        retry_count: u.retry_count,
        structured_output: u.structured_output,
        step_error: u.step_error,
    }
}

pub fn fan_out_update_to_core(u: RkFanOutItemUpdate) -> CoreFanOutItemUpdate {
    match u {
        RkFanOutItemUpdate::Running { child_run_id } => {
            CoreFanOutItemUpdate::Running { child_run_id }
        }
        RkFanOutItemUpdate::Terminal { status } => CoreFanOutItemUpdate::Terminal {
            status: fan_out_status_to_core(status),
        },
    }
}

pub fn fan_out_status_to_core(s: RkFanOutItemStatus) -> CoreFanOutItemStatus {
    match s {
        RkFanOutItemStatus::Pending => CoreFanOutItemStatus::Pending,
        RkFanOutItemStatus::Running => CoreFanOutItemStatus::Running,
        RkFanOutItemStatus::Completed => CoreFanOutItemStatus::Completed,
        RkFanOutItemStatus::Failed => CoreFanOutItemStatus::Failed,
        RkFanOutItemStatus::Skipped => CoreFanOutItemStatus::Skipped,
    }
}

pub fn run_to_rk(r: CoreRun) -> RkRun {
    RkRun {
        id: r.id,
        workflow_name: r.workflow_name,
        worktree_id: r.worktree_id,
        parent_run_id: r.parent_run_id,
        status: run_status_to_rk(r.status),
        dry_run: r.dry_run,
        trigger: r.trigger,
        started_at: r.started_at,
        ended_at: r.ended_at,
        result_summary: r.result_summary,
        error: r.error,
        definition_snapshot: r.definition_snapshot,
        inputs: r.inputs,
        ticket_id: r.ticket_id,
        repo_id: r.repo_id,
        parent_workflow_run_id: r.parent_workflow_run_id,
        target_label: r.target_label,
        default_bot_name: r.default_bot_name,
        iteration: r.iteration,
        blocked_on: r.blocked_on.map(blocked_on_to_rk),
        workflow_title: r.workflow_title,
        total_input_tokens: r.total_input_tokens,
        total_output_tokens: r.total_output_tokens,
        total_cache_read_input_tokens: r.total_cache_read_input_tokens,
        total_cache_creation_input_tokens: r.total_cache_creation_input_tokens,
        total_turns: r.total_turns,
        total_cost_usd: r.total_cost_usd,
        total_duration_ms: r.total_duration_ms,
        model: r.model,
        dismissed: r.dismissed,
    }
}

fn blocked_on_to_rk(b: CoreBlockedOn) -> RkBlockedOn {
    match b {
        CoreBlockedOn::HumanApproval {
            gate_name,
            prompt,
            options,
        } => RkBlockedOn::HumanApproval {
            gate_name,
            prompt,
            options,
        },
        CoreBlockedOn::HumanReview {
            gate_name,
            prompt,
            options,
        } => RkBlockedOn::HumanReview {
            gate_name,
            prompt,
            options,
        },
        CoreBlockedOn::PrApproval {
            gate_name,
            approvals_needed,
        } => RkBlockedOn::PrApproval {
            gate_name,
            approvals_needed,
        },
        CoreBlockedOn::PrChecks { gate_name } => RkBlockedOn::PrChecks { gate_name },
    }
}

pub fn step_to_rk(s: CoreStep) -> RkStep {
    let gate_type = s.gate_type.as_ref().and_then(|gt| {
        let gt_str = gt.to_string();
        gt_str
            .parse::<runkon_flow::dsl::GateType>()
            .map_err(|_| {
                tracing::warn!(
                    step_id = %s.id,
                    gate_type = %gt_str,
                    "Unrecognised gate type in step; treating as None",
                );
            })
            .ok()
    });
    RkStep {
        id: s.id,
        workflow_run_id: s.workflow_run_id,
        step_name: s.step_name,
        role: s.role,
        can_commit: s.can_commit,
        condition_expr: s.condition_expr,
        status: step_status_to_rk(s.status),
        child_run_id: s.child_run_id,
        position: s.position,
        started_at: s.started_at,
        ended_at: s.ended_at,
        result_text: s.result_text,
        condition_met: s.condition_met,
        iteration: s.iteration,
        parallel_group_id: s.parallel_group_id,
        context_out: s.context_out,
        markers_out: s.markers_out,
        retry_count: s.retry_count,
        gate_type,
        gate_prompt: s.gate_prompt,
        gate_timeout: s.gate_timeout,
        gate_approved_by: s.gate_approved_by,
        gate_approved_at: s.gate_approved_at,
        gate_feedback: s.gate_feedback,
        structured_output: s.structured_output,
        output_file: s.output_file,
        gate_options: s.gate_options,
        gate_selections: s.gate_selections,
        input_tokens: s.input_tokens,
        output_tokens: s.output_tokens,
        cache_read_input_tokens: s.cache_read_input_tokens,
        cache_creation_input_tokens: s.cache_creation_input_tokens,
        fan_out_total: s.fan_out_total,
        fan_out_completed: s.fan_out_completed,
        fan_out_failed: s.fan_out_failed,
        fan_out_skipped: s.fan_out_skipped,
        step_error: s.step_error,
    }
}

pub fn fan_out_item_to_rk(r: CoreFanOutItemRow) -> RkFanOutItemRow {
    RkFanOutItemRow {
        id: r.id,
        step_run_id: r.step_run_id,
        item_type: r.item_type,
        item_id: r.item_id,
        item_ref: r.item_ref,
        child_run_id: r.child_run_id,
        status: r.status,
        dispatched_at: r.dispatched_at,
        completed_at: r.completed_at,
    }
}

pub fn gate_approval_to_rk(s: CoreGateApprovalState) -> RkGateApprovalState {
    match s {
        CoreGateApprovalState::Pending => RkGateApprovalState::Pending,
        CoreGateApprovalState::Approved {
            feedback,
            selections,
        } => RkGateApprovalState::Approved {
            feedback,
            selections,
        },
        CoreGateApprovalState::Rejected { feedback } => RkGateApprovalState::Rejected { feedback },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::status::WorkflowStepStatus as CoreStepStatus;

    fn make_core_step(gate_type: Option<crate::workflow_dsl::GateType>) -> CoreStep {
        CoreStep {
            id: "step-1".to_string(),
            workflow_run_id: "run-1".to_string(),
            step_name: "test".to_string(),
            role: "actor".to_string(),
            can_commit: false,
            condition_expr: None,
            status: CoreStepStatus::Completed,
            child_run_id: None,
            position: 0,
            started_at: None,
            ended_at: None,
            result_text: None,
            condition_met: None,
            iteration: 0,
            parallel_group_id: None,
            context_out: None,
            markers_out: None,
            retry_count: 0,
            gate_type,
            gate_prompt: None,
            gate_timeout: None,
            gate_approved_by: None,
            gate_approved_at: None,
            gate_feedback: None,
            structured_output: None,
            output_file: None,
            gate_options: None,
            gate_selections: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            fan_out_total: None,
            fan_out_completed: 0,
            fan_out_failed: 0,
            fan_out_skipped: 0,
            step_error: None,
        }
    }

    #[test]
    fn step_to_rk_with_recognised_gate_type_preserves_gate() {
        let step = make_core_step(Some(crate::workflow_dsl::GateType::HumanApproval));
        let rk = step_to_rk(step);
        assert_eq!(
            rk.gate_type,
            Some(runkon_flow::dsl::GateType::HumanApproval),
            "recognised gate type should round-trip"
        );
    }

    #[test]
    fn step_to_rk_with_no_gate_type_is_none() {
        let step = make_core_step(None);
        let rk = step_to_rk(step);
        assert!(rk.gate_type.is_none(), "missing gate type should stay None");
    }

    #[test]
    fn step_to_rk_status_roundtrip() {
        let step = make_core_step(None);
        let rk = step_to_rk(step);
        assert_eq!(
            rk.status,
            runkon_flow::status::WorkflowStepStatus::Completed
        );
    }
}
