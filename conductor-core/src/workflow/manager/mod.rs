//! Conductor-core workflow manager helpers.
//!
//! ## Trait-vs-helper boundary
//!
//! Methods on `runkon_flow::traits::persistence::WorkflowPersistence` and
//! `GateApprovalStore` are the canonical engine-facing write path. The helpers
//! in this module (`steps`, `lifecycle`, `recovery`, …) are the
//! conductor-core API-layer write path: they own raw `&Connection` and write
//! directly to SQLite for operations that originate *above* the engine (UI
//! approvals, metric flushes from the action executor, background recovery).
//!
//! Do not call the raw-SQL helpers from inside `runkon-flow` code — use the
//! trait methods there. Do not add new raw-SQL helpers here for operations
//! that should be engine-facing — extend the trait instead.
pub(crate) mod definitions;
pub(crate) mod fan_out;
mod helpers;
pub(crate) mod lifecycle;
pub(crate) mod queries;
pub(crate) mod recovery;
pub(crate) mod steps;

#[cfg(test)]
mod tests;

pub use definitions::InvalidWorkflowEntry;
pub use steps::StepMetrics;

#[allow(unused_imports)]
pub(super) use helpers::{row_to_workflow_run, row_to_workflow_step};
