#![allow(unused_imports)]

pub(super) mod common;
mod gates;
mod manager;
mod output;
mod resumption;
mod types;

pub(super) fn completed_keys_from_steps(
    steps: &[WorkflowRunStep],
) -> std::collections::HashSet<StepKey> {
    steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| (s.step_name.clone(), s.iteration as u32))
        .collect()
}
pub(super) use super::output::{interpret_agent_output, parse_flow_output};
pub(super) use super::prompt_builder::substitute_variables;
pub(super) use super::types::{StepKey, WorkflowResumeInput};
pub(super) use super::*;
pub(super) use super::{WorkflowRunStatus, WorkflowStepStatus};
pub(super) use crate::config::Config;
pub(super) use crate::workflow::{
    ContextEntry, StepResult, WorkflowExecConfig, WorkflowRun, WorkflowRunStep,
};
pub(super) use common::*;
pub(super) use std::collections::HashMap;
