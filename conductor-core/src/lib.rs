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
pub mod agent_comm;
pub mod agent_config;
pub mod agent_identity;
pub mod agent_orchestration;
pub mod agent_runtime;
pub mod autonomous;
pub mod config;
pub mod consistency;
pub mod db;
pub mod error;
pub mod error_vocabulary;
pub(crate) mod escape_hatch;
pub mod feature;
pub(crate) mod git;
pub mod github;
pub mod github_app;
pub mod hooks;
pub mod issue_source;
pub mod jira_acli;
pub mod models;
pub mod notification_manager;
pub mod notify;
pub mod operational_catalog;
pub mod orchestrator;
pub mod prompt_config;
pub mod recovery;
pub mod repo;
pub(crate) mod retry;
pub mod schema_config;
pub mod scoring;
pub mod text_util;
pub mod tickets;
pub mod verification;
pub mod workflow;
pub mod workflow_config;
pub(crate) mod workflow_dsl;
pub mod workflow_ephemeral;
pub mod worktree;

/// Generate a new ULID-based unique ID string.
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
