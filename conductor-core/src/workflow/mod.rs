//! Workflow engine: execute multi-step workflow definitions with conditional
//! branching, loops, parallel execution, gates, and actor/reviewer agent roles.
//!
//! Builds on top of the existing `AgentManager` and orchestrator infrastructure,
//! adding workflow-level tracking in `workflow_runs` / `workflow_run_steps`.

pub(crate) mod constants;
pub(crate) mod engine;
pub(crate) mod executors;
pub(crate) mod helpers;
pub(crate) mod manager;
pub(crate) mod output;
pub(crate) mod prompt_builder;
pub(crate) mod status;
pub(crate) mod types;

// Re-export DSL types so consumers go through `workflow::` instead of `workflow_dsl::` directly.
pub use crate::workflow_dsl::{
    collect_agent_names, collect_workflow_refs, default_skills_dir, detect_workflow_cycles,
    make_script_resolver, parse_workflow_str, resolve_script_path, validate_script_steps,
    validate_workflow_semantics, AgentRef, AlwaysNode, CallNode, CallWorkflowNode, Condition,
    DoNode, DoWhileNode, GateNode, GateType, IfNode, InputDecl, InputType, ParallelNode,
    UnlessNode, ValidationError, ValidationReport, WhileNode, WorkflowDef, WorkflowNode,
    WorkflowTrigger, WorkflowWarning, MAX_WORKFLOW_DEPTH,
};

// Re-export all public types and functions to preserve existing import paths.
pub use constants::CONDUCTOR_OUTPUT_INSTRUCTION;
pub use engine::ENGINE_INJECTED_KEYS;
pub use engine::{
    apply_workflow_input_defaults, execute_workflow, execute_workflow_standalone, resume_workflow,
    resume_workflow_standalone, validate_resume_preconditions,
};
pub use manager::WorkflowManager;
pub use output::{parse_conductor_output, ConductorOutput};
pub use status::{WorkflowRunStatus, WorkflowStepStatus};
pub use types::{
    ActiveWorkflowCounts, ContextEntry, MetadataEntry, PendingGateRow, RunIdSlot, StepResult,
    WorkflowExecConfig, WorkflowExecInput, WorkflowExecStandalone, WorkflowResult,
    WorkflowResumeInput, WorkflowResumeStandalone, WorkflowRun, WorkflowRunContext,
    WorkflowRunStep, WorkflowStepSummary,
};

use crate::agent_config::AgentSpec;

/// Convert a DSL `AgentRef` to the `agent_config` layer's `AgentSpec`.
///
/// This is the boundary where the workflow DSL concern (`AgentRef`) maps to
/// the resolution concern (`AgentSpec`).
impl From<&AgentRef> for AgentSpec {
    fn from(r: &AgentRef) -> Self {
        match r {
            AgentRef::Name(s) => AgentSpec::Name(s.clone()),
            AgentRef::Path(s) => AgentSpec::Path(s.clone()),
        }
    }
}

#[cfg(test)]
mod tests;
