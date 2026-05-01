pub(crate) mod definitions;
mod fan_out;
mod helpers;
mod lifecycle;
pub(crate) mod queries;
pub(crate) mod recovery;
mod steps;

#[cfg(test)]
mod tests;

pub use definitions::InvalidWorkflowEntry;
pub use steps::StepMetrics;

#[allow(unused_imports)]
pub(super) use helpers::{row_to_workflow_run, row_to_workflow_step};

use rusqlite::Connection;

/// Manages workflow definitions, execution, and persistence.
pub struct WorkflowManager<'a> {
    pub(super) conn: &'a Connection,
}

impl<'a> WorkflowManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Borrow the underlying connection. Used by callers that need to invoke
    /// module-level free functions in `crate::workflow::manager::queries` (and,
    /// in subsequent migration PRs, the other manager-module free functions).
    pub fn conn(&self) -> &'a Connection {
        self.conn
    }
}
