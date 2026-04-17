/// Implements `rusqlite::types::ToSql` and `rusqlite::types::FromSql` for an
/// enum that already implements `std::fmt::Display` and `std::str::FromStr<Err = String>`.
macro_rules! impl_sql_enum {
    ($Type:ty) => {
        impl rusqlite::types::ToSql for $Type {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.to_string()))
            }
        }

        impl rusqlite::types::FromSql for $Type {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let s = String::column_result(value)?;
                s.parse().map_err(|e: String| {
                    rusqlite::types::FromSqlError::Other(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        e,
                    )))
                })
            }
        }
    };
}

pub(crate) use impl_sql_enum;

pub mod agent;
pub mod agent_config;
pub mod agent_runtime;
pub mod attachments;
pub mod config;
pub mod conversation;
pub mod db;
pub mod error;
pub mod feature;
pub(crate) mod git;
pub mod github;
pub mod github_app;
pub(crate) mod graph;
pub mod hooks;
pub mod infer;
pub mod issue_source;
pub mod jira_acli;
pub mod models;
pub mod notification_event;
pub mod notification_hooks;
pub mod notification_manager;
pub mod notify;
pub mod orchestrator;
pub mod process_utils;
pub mod prompt_config;
pub mod repo;
pub(crate) mod retry;
pub mod schema_config;
pub mod text_util;
pub mod ticket_source;
pub mod tickets;
pub mod vantage;
pub mod workflow;
pub mod workflow_config;
pub(crate) mod workflow_dsl;
pub mod workflow_ephemeral;
pub mod workflow_template;
pub mod worktree;

/// Generate a new ULID-based unique ID string.
///
/// Uses a thread-local monotonic generator so IDs created within the
/// same millisecond are guaranteed to sort in creation order.
pub fn new_id() -> String {
    use std::cell::RefCell;
    thread_local! {
        static GEN: RefCell<ulid::Generator> = const { RefCell::new(ulid::Generator::new()) };
    }
    GEN.with(|g| {
        g.borrow_mut()
            .generate()
            .unwrap_or_else(|_| ulid::Ulid::new())
            .to_string()
    })
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
