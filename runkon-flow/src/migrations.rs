//! Refinery-managed schema migrations for the runkon-flow workflow tables.
//!
//! Gated on the `rusqlite` cargo feature alongside `persistence_sqlite`.
//! V001 is frozen as of 0.16.0 — refinery checksum-validates it on every run.

use refinery::Runner;
use rusqlite::Connection;

use crate::engine_error::EngineError;

mod embedded {
    refinery::embed_migrations!("migrations");
}

/// History table name. Distinct from conductor's `_conductor_meta` migration runner
/// so the two can co-exist in the same database without collision.
const HISTORY_TABLE: &str = "runkon_flow_schema_history";

pub fn run(conn: &mut Connection) -> Result<(), EngineError> {
    let mut runner: Runner = embedded::migrations::runner();
    runner.set_migration_table_name(HISTORY_TABLE);
    runner
        .run(conn)
        .map_err(|e| EngineError::Persistence(format!("runkon-flow migrations failed: {e}")))?;
    Ok(())
}

#[cfg(all(test, feature = "rusqlite"))]
mod tests {
    use std::collections::HashSet;

    use rusqlite::Connection;

    use crate::constants::{RUN_COLUMNS, STEP_COLUMNS};

    use super::run;

    fn column_names(conn: &Connection, table: &str) -> HashSet<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn parse_columns(cols: &str) -> HashSet<String> {
        cols.split(',').map(|s| s.trim().to_string()).collect()
    }

    #[test]
    fn v001_creates_expected_columns() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();

        let run_cols = column_names(&conn, "workflow_runs");
        let expected_run = parse_columns(RUN_COLUMNS);
        assert_eq!(
            run_cols, expected_run,
            "workflow_runs columns don't match RUN_COLUMNS"
        );

        let step_cols = column_names(&conn, "workflow_run_steps");
        let expected_step = parse_columns(STEP_COLUMNS);
        assert_eq!(
            step_cols, expected_step,
            "workflow_run_steps columns don't match STEP_COLUMNS"
        );
    }

    #[test]
    fn v001_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        run(&mut conn).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM runkon_flow_schema_history WHERE version = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "V001 should be recorded exactly once");
    }
}
