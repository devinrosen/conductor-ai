mod definitions;
mod fan_out;
mod helpers;
mod lifecycle;
mod queries;
pub(crate) mod recovery;
mod steps;

#[cfg(test)]
mod tests;

pub use fan_out::FanOutItemRow;

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
}
