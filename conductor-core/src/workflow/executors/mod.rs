mod api_call;
mod call;
mod call_workflow;
mod control_flow;
mod foreach;
mod gate;
mod parallel;
mod script;

#[cfg(test)]
mod tests;

// Public API — used by engine.rs and workflow/tests/
pub(super) use call::execute_call;
pub(super) use call_workflow::execute_call_workflow;
pub(super) use control_flow::{
    execute_do, execute_do_while, execute_if, execute_unless, execute_while,
};
pub(super) use foreach::execute_foreach;
pub(super) use gate::execute_gate;
#[cfg(test)]
pub(super) use gate::handle_gate_timeout;
pub(super) use parallel::execute_parallel;
pub(super) use script::execute_script;

// Private helpers exposed to inline tests via #[cfg(test)]
#[cfg(test)]
pub(super) use control_flow::eval_condition;
#[cfg(test)]
pub(super) use gate::execute_quality_gate;
#[cfg(test)]
pub(super) use script::{poll_script_child, read_stdout_bounded, ScriptPollResult};
