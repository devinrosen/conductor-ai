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
pub mod config;
pub mod db;
pub mod error;
pub mod github;
pub mod github_app;
pub mod issue_source;
pub mod jira_acli;
pub mod merge_queue;
pub mod models;
pub mod orchestrator;
pub mod prompt_config;
pub mod repo;
pub mod schema_config;
pub mod text_util;
pub mod tickets;
pub mod workflow;
pub mod workflow_config;
pub(crate) mod workflow_dsl;
pub mod workflow_ephemeral;
pub mod worktree;

#[cfg(test)]
pub mod test_helpers;
