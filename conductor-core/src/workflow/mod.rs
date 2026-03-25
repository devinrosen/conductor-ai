//! Workflow engine: execute multi-step workflow definitions with conditional
//! branching, loops, parallel execution, gates, and actor/reviewer agent roles.
//!
//! Builds on top of the existing `AgentManager` and orchestrator infrastructure,
//! adding workflow-level tracking in `workflow_runs` / `workflow_run_steps`.

mod batch_validate;
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
    WorkflowSource, WorkflowTrigger, WorkflowWarning, MAX_WORKFLOW_DEPTH,
};

/// Load a single workflow definition by name.
///
/// Checks the repo `.conductor/workflows/` directory first, then falls back to
/// built-in workflows embedded in the binary.
pub fn load_workflow_by_name(
    worktree_path: &str,
    repo_path: &str,
    name: &str,
) -> crate::error::Result<WorkflowDef> {
    match crate::workflow_dsl::load_workflow_by_name(worktree_path, repo_path, name) {
        Ok(def) => Ok(def),
        Err(_) => crate::builtin_workflows::load_builtin_by_name(name).ok_or_else(|| {
            crate::error::ConductorError::Workflow(format!(
                "Workflow '{name}' not found in .conductor/workflows/ or built-in workflows"
            ))
        }),
    }
}

// Re-export batch validation from the workflow layer (not DSL).
pub use batch_validate::{
    validate_workflows_batch, BatchValidationResult, BatchValidationWarning,
    WorkflowValidationEntry,
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
    resolve_conductor_bin_dir, ActiveWorkflowCounts, BlockedOn, ContextEntry, MetadataEntry,
    PendingGateRow, RunIdSlot, StepResult, WorkflowExecConfig, WorkflowExecInput,
    WorkflowExecStandalone, WorkflowResult, WorkflowResumeInput, WorkflowResumeStandalone,
    WorkflowRun, WorkflowRunContext, WorkflowRunStep, WorkflowStepSummary,
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
