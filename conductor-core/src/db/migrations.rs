use rusqlite::{params, Connection};
use serde::Deserialize;

use crate::error::Result;

/// Legacy plan step shape used only for migrating JSON data from agent_runs.plan.
#[derive(Deserialize)]
struct LegacyPlanStep {
    description: String,
    #[serde(default)]
    done: bool,
}

/// Run all schema migrations. Uses a simple version counter in a meta table.
pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _conductor_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;

    let version: i64 = conn.query_row(
        "SELECT COALESCE(
                (SELECT CAST(value AS INTEGER) FROM _conductor_meta WHERE key = 'schema_version'),
                0
            )",
        [],
        |row| row.get(0),
    )?;

    if version < 1 {
        conn.execute_batch(include_str!("migrations/001_initial.sql"))?;
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '1')",
            [],
        )?;
    }

    // Migration 002: add completed_at to worktrees.
    // Check column existence rather than version number to handle DBs that jumped
    // past version 1 via other feature branches.
    let has_completed_at: bool = conn
        .prepare("SELECT completed_at FROM worktrees LIMIT 0")
        .is_ok();
    if !has_completed_at {
        conn.execute_batch(include_str!("migrations/002_worktree_completed_at.sql"))?;
    }
    if version < 2 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '2')",
            [],
        )?;
    }

    if version < 3 {
        conn.execute_batch(include_str!("migrations/003_agent_runs.sql"))?;
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '3')",
            [],
        )?;
    }

    // Migration 004: add tmux_window to agent_runs.
    // Check column existence to handle DBs that already have it from feature branches.
    let has_tmux_window: bool = conn
        .prepare("SELECT tmux_window FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_tmux_window {
        conn.execute_batch(include_str!("migrations/004_agent_tmux.sql"))?;
    }
    if version < 4 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '4')",
            [],
        )?;
    }

    // Migration 005: add log_file to agent_runs.
    let has_log_file: bool = conn
        .prepare("SELECT log_file FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_log_file {
        conn.execute_batch(include_str!("migrations/005_agent_log_file.sql"))?;
    }
    if version < 5 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '5')",
            [],
        )?;
    }

    // Migration 006: drop sessions and session_worktrees tables.
    if version < 6 {
        conn.execute_batch(include_str!("migrations/006_drop_sessions.sql"))?;
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '6')",
            [],
        )?;
    }

    // Migration 007: add agent_run_events table (trace/span model).
    let has_agent_run_events: bool = conn
        .prepare("SELECT id FROM agent_run_events LIMIT 0")
        .is_ok();
    if !has_agent_run_events {
        conn.execute_batch(include_str!("migrations/007_agent_run_events.sql"))?;
    }
    if version < 7 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '7')",
            [],
        )?;
    }

    // Migration 008: add model column to worktrees.
    let has_worktree_model: bool = conn.prepare("SELECT model FROM worktrees LIMIT 0").is_ok();
    if !has_worktree_model {
        conn.execute_batch(include_str!("migrations/008_worktree_model.sql"))?;
    }
    if version < 8 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '8')",
            [],
        )?;
    }

    // Migration 009: add model column to agent_runs.
    let has_agent_run_model: bool = conn.prepare("SELECT model FROM agent_runs LIMIT 0").is_ok();
    if !has_agent_run_model {
        conn.execute_batch(include_str!("migrations/009_agent_run_model.sql"))?;
    }
    if version < 9 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '9')",
            [],
        )?;
    }

    // Migration 010: add model column to repos.
    let has_repo_model: bool = conn.prepare("SELECT model FROM repos LIMIT 0").is_ok();
    if !has_repo_model {
        conn.execute_batch(include_str!("migrations/010_repo_model.sql"))?;
    }
    if version < 10 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '10')",
            [],
        )?;
    }

    // Migration 011: add plan column to agent_runs.
    let has_plan: bool = conn.prepare("SELECT plan FROM agent_runs LIMIT 0").is_ok();
    if !has_plan {
        conn.execute_batch(include_str!("migrations/011_agent_plan.sql"))?;
    }
    if version < 11 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '11')",
            [],
        )?;
    }

    // Migration 012: add parent_run_id to agent_runs for parent/child relationships.
    let has_parent_run_id: bool = conn
        .prepare("SELECT parent_run_id FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_parent_run_id {
        conn.execute_batch(include_str!("migrations/012_parent_run_id.sql"))?;
    }
    if version < 12 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '12')",
            [],
        )?;
    }

    // Migration 013: add agent_created_issues table.
    let has_agent_created_issues: bool = conn
        .prepare("SELECT id FROM agent_created_issues LIMIT 0")
        .is_ok();
    if !has_agent_created_issues {
        conn.execute_batch(include_str!("migrations/007_agent_created_issues.sql"))?;
    }
    if version < 13 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '13')",
            [],
        )?;
    }

    // Migration 014: add allow_agent_issue_creation to repos.
    let has_allow_agent_issue_creation: bool = conn
        .prepare("SELECT allow_agent_issue_creation FROM repos LIMIT 0")
        .is_ok();
    if !has_allow_agent_issue_creation {
        conn.execute_batch(include_str!("migrations/008_repo_allow_agent_issues.sql"))?;
    }
    if version < 14 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '14')",
            [],
        )?;
    }

    // Migration 015: create agent_run_steps table and migrate JSON plan data.
    let has_agent_run_steps: bool = conn
        .prepare("SELECT id FROM agent_run_steps LIMIT 0")
        .is_ok();
    if !has_agent_run_steps {
        conn.execute_batch(include_str!("migrations/015_agent_run_steps.sql"))?;

        // Migrate existing JSON plan data from agent_runs.plan into the new table.
        let mut read_stmt =
            conn.prepare("SELECT id, plan FROM agent_runs WHERE plan IS NOT NULL")?;
        let rows: Vec<(String, String)> = read_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for (run_id, plan_json) in &rows {
            if let Ok(steps) = serde_json::from_str::<Vec<LegacyPlanStep>>(plan_json) {
                for (i, step) in steps.iter().enumerate() {
                    let step_id = ulid::Ulid::new().to_string();
                    let status = if step.done { "completed" } else { "pending" };
                    conn.execute(
                        "INSERT INTO agent_run_steps (id, run_id, position, description, status) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![step_id, run_id, i as i64, step.description, status],
                    )?;
                }
            }
        }
    }
    if version < 15 {
        conn.execute(
            "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', '15')",
            [],
        )?;
    }

    Ok(())
}
