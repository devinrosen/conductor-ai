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
    let gate_type = s.gate_type.as_ref().map(|gt| match gt {
        crate::workflow_dsl::GateType::HumanApproval => runkon_flow::dsl::GateType::HumanApproval,
        crate::workflow_dsl::GateType::HumanReview => runkon_flow::dsl::GateType::HumanReview,
        crate::workflow_dsl::GateType::PrApproval => runkon_flow::dsl::GateType::PrApproval,
        crate::workflow_dsl::GateType::PrChecks => runkon_flow::dsl::GateType::PrChecks,
        crate::workflow_dsl::GateType::QualityGate => runkon_flow::dsl::GateType::QualityGate,
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

pub fn core_workflow_result_to_rk(
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

pub fn rk_workflow_result_to_core(
    r: runkon_flow::types::WorkflowResult,
) -> crate::workflow::types::WorkflowResult {
    crate::workflow::types::WorkflowResult {
        workflow_run_id: r.workflow_run_id,
        worktree_id: r.worktree_id,
        workflow_name: r.workflow_name,
        all_succeeded: r.all_succeeded,
        total_cost: r.total_cost,
        total_turns: r.total_turns,
        total_duration_ms: r.total_duration_ms,
        total_input_tokens: r.total_input_tokens,
        total_output_tokens: r.total_output_tokens,
        total_cache_read_input_tokens: r.total_cache_read_input_tokens,
        total_cache_creation_input_tokens: r.total_cache_creation_input_tokens,
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

    // ---------------------------------------------------------------------------
    // run_status_to_rk / run_status_to_core — all 8 variants in both directions
    // ---------------------------------------------------------------------------

    macro_rules! run_status_roundtrip {
        ($name:ident, $core:expr, $rk:expr) => {
            #[test]
            fn $name() {
                use crate::workflow::status::WorkflowRunStatus as Core;
                use runkon_flow::status::WorkflowRunStatus as Rk;
                assert_eq!(run_status_to_rk($core), $rk, "core→rk");
                assert_eq!(run_status_to_core($rk), $core, "rk→core");
            }
        };
    }

    run_status_roundtrip!(run_status_pending, Core::Pending, Rk::Pending);
    run_status_roundtrip!(run_status_running, Core::Running, Rk::Running);
    run_status_roundtrip!(run_status_completed, Core::Completed, Rk::Completed);
    run_status_roundtrip!(run_status_failed, Core::Failed, Rk::Failed);
    run_status_roundtrip!(run_status_cancelled, Core::Cancelled, Rk::Cancelled);
    run_status_roundtrip!(run_status_waiting, Core::Waiting, Rk::Waiting);
    run_status_roundtrip!(run_status_needs_resume, Core::NeedsResume, Rk::NeedsResume);
    run_status_roundtrip!(run_status_cancelling, Core::Cancelling, Rk::Cancelling);

    // ---------------------------------------------------------------------------
    // step_status_to_rk / step_status_to_core — all 7 variants in both directions
    // ---------------------------------------------------------------------------

    macro_rules! step_status_roundtrip {
        ($name:ident, $core:expr, $rk:expr) => {
            #[test]
            fn $name() {
                use crate::workflow::status::WorkflowStepStatus as Core;
                use runkon_flow::status::WorkflowStepStatus as Rk;
                assert_eq!(step_status_to_rk($core), $rk, "core→rk");
                assert_eq!(step_status_to_core($rk), $core, "rk→core");
            }
        };
    }

    step_status_roundtrip!(step_status_pending, Core::Pending, Rk::Pending);
    step_status_roundtrip!(step_status_running, Core::Running, Rk::Running);
    step_status_roundtrip!(step_status_completed, Core::Completed, Rk::Completed);
    step_status_roundtrip!(step_status_failed, Core::Failed, Rk::Failed);
    step_status_roundtrip!(step_status_skipped, Core::Skipped, Rk::Skipped);
    step_status_roundtrip!(step_status_waiting, Core::Waiting, Rk::Waiting);
    step_status_roundtrip!(step_status_timed_out, Core::TimedOut, Rk::TimedOut);

    // ---------------------------------------------------------------------------
    // fan_out_update_to_core — Running and Terminal arms
    // ---------------------------------------------------------------------------

    #[test]
    fn fan_out_update_running_preserves_child_run_id() {
        let update = RkFanOutItemUpdate::Running {
            child_run_id: "child-123".to_string(),
        };
        match fan_out_update_to_core(update) {
            CoreFanOutItemUpdate::Running { child_run_id } => {
                assert_eq!(child_run_id, "child-123");
            }
            CoreFanOutItemUpdate::Terminal { .. } => panic!("expected Running"),
        }
    }

    #[test]
    fn fan_out_update_terminal_preserves_status() {
        let update = RkFanOutItemUpdate::Terminal {
            status: RkFanOutItemStatus::Completed,
        };
        match fan_out_update_to_core(update) {
            CoreFanOutItemUpdate::Terminal { status } => {
                assert_eq!(status, CoreFanOutItemStatus::Completed);
            }
            CoreFanOutItemUpdate::Running { .. } => panic!("expected Terminal"),
        }
    }

    // ---------------------------------------------------------------------------
    // fan_out_status_to_core — all 5 variants
    // ---------------------------------------------------------------------------

    macro_rules! fan_out_status_roundtrip {
        ($name:ident, $rk:expr, $core:expr) => {
            #[test]
            fn $name() {
                assert_eq!(fan_out_status_to_core($rk), $core);
            }
        };
    }

    fan_out_status_roundtrip!(
        fan_out_status_pending,
        RkFanOutItemStatus::Pending,
        CoreFanOutItemStatus::Pending
    );
    fan_out_status_roundtrip!(
        fan_out_status_running,
        RkFanOutItemStatus::Running,
        CoreFanOutItemStatus::Running
    );
    fan_out_status_roundtrip!(
        fan_out_status_completed,
        RkFanOutItemStatus::Completed,
        CoreFanOutItemStatus::Completed
    );
    fan_out_status_roundtrip!(
        fan_out_status_failed,
        RkFanOutItemStatus::Failed,
        CoreFanOutItemStatus::Failed
    );
    fan_out_status_roundtrip!(
        fan_out_status_skipped,
        RkFanOutItemStatus::Skipped,
        CoreFanOutItemStatus::Skipped
    );

    // ---------------------------------------------------------------------------
    // rk_workflow_result_to_core — field-mapping correctness (guards transposition)
    // ---------------------------------------------------------------------------

    #[test]
    fn rk_workflow_result_to_core_maps_all_fields() {
        use runkon_flow::types::WorkflowResult as RkResult;
        let rk = RkResult {
            workflow_run_id: "run-1".to_string(),
            worktree_id: Some("wt-1".to_string()),
            workflow_name: "my-wf".to_string(),
            all_succeeded: true,
            total_cost: 1.5,
            total_turns: 10,
            total_duration_ms: 5000,
            total_input_tokens: 100,
            total_output_tokens: 200,
            total_cache_read_input_tokens: 50,
            total_cache_creation_input_tokens: 25,
        };
        let core = rk_workflow_result_to_core(rk);
        assert_eq!(core.workflow_run_id, "run-1");
        assert_eq!(core.worktree_id, Some("wt-1".to_string()));
        assert_eq!(core.workflow_name, "my-wf");
        assert!(core.all_succeeded);
        assert_eq!(core.total_cost, 1.5);
        assert_eq!(core.total_turns, 10);
        assert_eq!(core.total_duration_ms, 5000);
        assert_eq!(core.total_input_tokens, 100);
        assert_eq!(core.total_output_tokens, 200);
        assert_eq!(core.total_cache_read_input_tokens, 50);
        assert_eq!(core.total_cache_creation_input_tokens, 25);
    }

    // ---------------------------------------------------------------------------
    // gate_approval_to_rk — all 3 variants with payload verification
    // ---------------------------------------------------------------------------

    #[test]
    fn gate_approval_to_rk_pending() {
        assert!(matches!(
            gate_approval_to_rk(CoreGateApprovalState::Pending),
            RkGateApprovalState::Pending
        ));
    }

    #[test]
    fn gate_approval_to_rk_approved_preserves_payload() {
        let state = CoreGateApprovalState::Approved {
            feedback: Some("lgtm".to_string()),
            selections: Some(vec!["opt-a".to_string()]),
        };
        match gate_approval_to_rk(state) {
            RkGateApprovalState::Approved {
                feedback,
                selections,
            } => {
                assert_eq!(feedback.as_deref(), Some("lgtm"));
                assert_eq!(selections, Some(vec!["opt-a".to_string()]));
            }
            other => panic!("expected Approved, got {other:?}"),
        }
    }

    #[test]
    fn gate_approval_to_rk_rejected_preserves_feedback() {
        let state = CoreGateApprovalState::Rejected {
            feedback: Some("not ready".to_string()),
        };
        match gate_approval_to_rk(state) {
            RkGateApprovalState::Rejected { feedback } => {
                assert_eq!(feedback.as_deref(), Some("not ready"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // run_to_rk — field mapping correctness
    // ---------------------------------------------------------------------------

    fn make_core_run() -> CoreRun {
        CoreRun {
            id: "run-1".to_string(),
            workflow_name: "wf".to_string(),
            worktree_id: Some("wt-1".to_string()),
            parent_run_id: "parent".to_string(),
            status: crate::workflow::status::WorkflowRunStatus::Completed,
            dry_run: false,
            trigger: "manual".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            ended_at: Some("2024-01-01T01:00:00Z".to_string()),
            result_summary: Some("ok".to_string()),
            error: None,
            definition_snapshot: None,
            inputs: std::collections::HashMap::new(),
            ticket_id: None,
            repo_id: Some("repo-1".to_string()),
            parent_workflow_run_id: None,
            target_label: None,
            default_bot_name: None,
            iteration: 0,
            blocked_on: None,
            workflow_title: Some("My Workflow".to_string()),
            total_input_tokens: Some(10),
            total_output_tokens: Some(20),
            total_cache_read_input_tokens: Some(5),
            total_cache_creation_input_tokens: Some(2),
            total_turns: Some(3),
            total_cost_usd: Some(0.5),
            total_duration_ms: Some(1000),
            model: Some("claude-3".to_string()),
            dismissed: false,
        }
    }

    #[test]
    fn run_to_rk_maps_all_fields() {
        let rk = run_to_rk(make_core_run());
        assert_eq!(rk.id, "run-1");
        assert_eq!(rk.workflow_name, "wf");
        assert_eq!(rk.worktree_id, Some("wt-1".to_string()));
        assert_eq!(rk.parent_run_id, "parent");
        assert_eq!(rk.status, runkon_flow::status::WorkflowRunStatus::Completed);
        assert!(!rk.dry_run);
        assert_eq!(rk.trigger, "manual");
        assert_eq!(rk.started_at, "2024-01-01T00:00:00Z");
        assert_eq!(rk.ended_at, Some("2024-01-01T01:00:00Z".to_string()));
        assert_eq!(rk.result_summary, Some("ok".to_string()));
        assert!(rk.error.is_none());
        assert_eq!(rk.repo_id, Some("repo-1".to_string()));
        assert_eq!(rk.workflow_title, Some("My Workflow".to_string()));
        assert_eq!(rk.total_input_tokens, Some(10));
        assert_eq!(rk.total_output_tokens, Some(20));
        assert_eq!(rk.total_cache_read_input_tokens, Some(5));
        assert_eq!(rk.total_cache_creation_input_tokens, Some(2));
        assert_eq!(rk.total_turns, Some(3));
        assert_eq!(rk.total_cost_usd, Some(0.5));
        assert_eq!(rk.total_duration_ms, Some(1000));
        assert_eq!(rk.model, Some("claude-3".to_string()));
        assert!(!rk.dismissed);
    }

    #[test]
    fn run_to_rk_blocked_on_none_stays_none() {
        let rk = run_to_rk(make_core_run());
        assert!(rk.blocked_on.is_none());
    }

    // ---------------------------------------------------------------------------
    // blocked_on_to_rk — all 4 variants
    // ---------------------------------------------------------------------------

    #[test]
    fn blocked_on_human_approval_preserves_fields() {
        let b = CoreBlockedOn::HumanApproval {
            gate_name: "approve-gate".to_string(),
            prompt: Some("approve?".to_string()),
            options: vec![],
        };
        match run_to_rk(CoreRun {
            blocked_on: Some(b),
            ..make_core_run()
        })
        .blocked_on
        .unwrap()
        {
            RkBlockedOn::HumanApproval {
                gate_name,
                prompt,
                options,
            } => {
                assert_eq!(gate_name, "approve-gate");
                assert_eq!(prompt.as_deref(), Some("approve?"));
                assert!(options.is_empty());
            }
            other => panic!("expected HumanApproval, got {other:?}"),
        }
    }

    #[test]
    fn blocked_on_human_review_preserves_fields() {
        let b = CoreBlockedOn::HumanReview {
            gate_name: "review-gate".to_string(),
            prompt: None,
            options: vec!["opt-a".to_string()],
        };
        match run_to_rk(CoreRun {
            blocked_on: Some(b),
            ..make_core_run()
        })
        .blocked_on
        .unwrap()
        {
            RkBlockedOn::HumanReview {
                gate_name,
                prompt,
                options,
            } => {
                assert_eq!(gate_name, "review-gate");
                assert!(prompt.is_none());
                assert_eq!(options, vec!["opt-a".to_string()]);
            }
            other => panic!("expected HumanReview, got {other:?}"),
        }
    }

    #[test]
    fn blocked_on_pr_approval_preserves_fields() {
        let b = CoreBlockedOn::PrApproval {
            gate_name: "pr-gate".to_string(),
            approvals_needed: 2,
        };
        match run_to_rk(CoreRun {
            blocked_on: Some(b),
            ..make_core_run()
        })
        .blocked_on
        .unwrap()
        {
            RkBlockedOn::PrApproval {
                gate_name,
                approvals_needed,
            } => {
                assert_eq!(gate_name, "pr-gate");
                assert_eq!(approvals_needed, 2u32);
            }
            other => panic!("expected PrApproval, got {other:?}"),
        }
    }

    #[test]
    fn blocked_on_pr_checks_preserves_gate_name() {
        let b = CoreBlockedOn::PrChecks {
            gate_name: "checks-gate".to_string(),
        };
        match run_to_rk(CoreRun {
            blocked_on: Some(b),
            ..make_core_run()
        })
        .blocked_on
        .unwrap()
        {
            RkBlockedOn::PrChecks { gate_name } => {
                assert_eq!(gate_name, "checks-gate");
            }
            other => panic!("expected PrChecks, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // new_run_to_core / new_step_to_core — field-mapping correctness
    // ---------------------------------------------------------------------------

    #[test]
    fn new_run_to_core_maps_all_fields() {
        let rk = RkNewRun {
            workflow_name: "my-wf".to_string(),
            worktree_id: Some("wt-1".to_string()),
            ticket_id: Some("ticket-1".to_string()),
            repo_id: Some("repo-1".to_string()),
            parent_run_id: "parent".to_string(),
            dry_run: true,
            trigger: "manual".to_string(),
            definition_snapshot: Some("{}".to_string()),
            parent_workflow_run_id: Some("parent-wf-run".to_string()),
            target_label: Some("label".to_string()),
        };
        let core = new_run_to_core(rk);
        assert_eq!(core.workflow_name, "my-wf");
        assert_eq!(core.worktree_id, Some("wt-1".to_string()));
        assert_eq!(core.ticket_id, Some("ticket-1".to_string()));
        assert_eq!(core.repo_id, Some("repo-1".to_string()));
        assert_eq!(core.parent_run_id, "parent");
        assert!(core.dry_run);
        assert_eq!(core.trigger, "manual");
        assert_eq!(core.definition_snapshot, Some("{}".to_string()));
        assert_eq!(
            core.parent_workflow_run_id,
            Some("parent-wf-run".to_string())
        );
        assert_eq!(core.target_label, Some("label".to_string()));
    }

    #[test]
    fn new_step_to_core_maps_all_fields() {
        let rk = RkNewStep {
            workflow_run_id: "run-1".to_string(),
            step_name: "my-step".to_string(),
            role: "actor".to_string(),
            can_commit: true,
            position: 3,
            iteration: 2,
            retry_count: Some(1),
        };
        let core = new_step_to_core(rk);
        assert_eq!(core.workflow_run_id, "run-1");
        assert_eq!(core.step_name, "my-step");
        assert_eq!(core.role, "actor");
        assert!(core.can_commit);
        assert_eq!(core.position, 3);
        assert_eq!(core.iteration, 2);
        assert_eq!(core.retry_count, Some(1));
    }
}
