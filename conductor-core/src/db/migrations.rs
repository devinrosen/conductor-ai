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

fn bump_version(conn: &Connection, v: u32) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _conductor_meta (key, value) VALUES ('schema_version', ?1)",
        params![v.to_string()],
    )?;
    Ok(())
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
        bump_version(conn, 1)?;
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
        bump_version(conn, 2)?;
    }

    if version < 3 {
        conn.execute_batch(include_str!("migrations/003_agent_runs.sql"))?;
        bump_version(conn, 3)?;
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
        bump_version(conn, 4)?;
    }

    // Migration 005: add log_file to agent_runs.
    let has_log_file: bool = conn
        .prepare("SELECT log_file FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_log_file {
        conn.execute_batch(include_str!("migrations/005_agent_log_file.sql"))?;
    }
    if version < 5 {
        bump_version(conn, 5)?;
    }

    // Migration 006: drop sessions and session_worktrees tables.
    if version < 6 {
        conn.execute_batch(include_str!("migrations/006_drop_sessions.sql"))?;
        bump_version(conn, 6)?;
    }

    // Migration 007: add agent_run_events table (trace/span model).
    let has_agent_run_events: bool = conn
        .prepare("SELECT id FROM agent_run_events LIMIT 0")
        .is_ok();
    if !has_agent_run_events {
        conn.execute_batch(include_str!("migrations/007_agent_run_events.sql"))?;
    }
    if version < 7 {
        bump_version(conn, 7)?;
    }

    // Migration 008: add model column to worktrees.
    let has_worktree_model: bool = conn.prepare("SELECT model FROM worktrees LIMIT 0").is_ok();
    if !has_worktree_model {
        conn.execute_batch(include_str!("migrations/008_worktree_model.sql"))?;
    }
    if version < 8 {
        bump_version(conn, 8)?;
    }

    // Migration 009: add model column to agent_runs.
    let has_agent_run_model: bool = conn.prepare("SELECT model FROM agent_runs LIMIT 0").is_ok();
    if !has_agent_run_model {
        conn.execute_batch(include_str!("migrations/009_agent_run_model.sql"))?;
    }
    if version < 9 {
        bump_version(conn, 9)?;
    }

    // Migration 010: add model column to repos.
    let has_repo_model: bool = conn.prepare("SELECT model FROM repos LIMIT 0").is_ok();
    if !has_repo_model {
        conn.execute_batch(include_str!("migrations/010_repo_model.sql"))?;
    }
    if version < 10 {
        bump_version(conn, 10)?;
    }

    // Migration 011: add plan column to agent_runs.
    let has_plan: bool = conn.prepare("SELECT plan FROM agent_runs LIMIT 0").is_ok();
    if !has_plan {
        conn.execute_batch(include_str!("migrations/011_agent_plan.sql"))?;
    }
    if version < 11 {
        bump_version(conn, 11)?;
    }

    // Migration 012: add parent_run_id to agent_runs for parent/child relationships.
    let has_parent_run_id: bool = conn
        .prepare("SELECT parent_run_id FROM agent_runs LIMIT 0")
        .is_ok();
    if !has_parent_run_id {
        conn.execute_batch(include_str!("migrations/012_parent_run_id.sql"))?;
    }
    if version < 12 {
        bump_version(conn, 12)?;
    }

    // Migration 013: add agent_created_issues table.
    let has_agent_created_issues: bool = conn
        .prepare("SELECT id FROM agent_created_issues LIMIT 0")
        .is_ok();
    if !has_agent_created_issues {
        conn.execute_batch(include_str!("migrations/007_agent_created_issues.sql"))?;
    }
    if version < 13 {
        bump_version(conn, 13)?;
    }

    // Migration 014: add allow_agent_issue_creation to repos.
    let has_allow_agent_issue_creation: bool = conn
        .prepare("SELECT allow_agent_issue_creation FROM repos LIMIT 0")
        .is_ok();
    if !has_allow_agent_issue_creation {
        conn.execute_batch(include_str!("migrations/008_repo_allow_agent_issues.sql"))?;
    }
    if version < 14 {
        bump_version(conn, 14)?;
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
        bump_version(conn, 15)?;
    }

    // Migration 016: create merge_queue table for serializing parallel agent merges.
    let has_merge_queue: bool = conn.prepare("SELECT id FROM merge_queue LIMIT 0").is_ok();
    if !has_merge_queue {
        conn.execute_batch(include_str!("migrations/016_merge_queue.sql"))?;
    }
    if version < 16 {
        bump_version(conn, 16)?;
    }

    // Migration 017: create review_configs table for multi-agent PR review swarms.
    let has_review_configs: bool = conn
        .prepare("SELECT id FROM review_configs LIMIT 0")
        .is_ok();
    if !has_review_configs {
        conn.execute_batch(include_str!("migrations/017_review_configs.sql"))?;
    }
    if version < 17 {
        bump_version(conn, 17)?;
    }

    // Migration 018: feedback_requests table + update agent_runs CHECK constraint.
    // The agent_runs table must be recreated to add 'waiting_for_feedback' to the
    // status CHECK constraint. PRAGMA foreign_keys = OFF must be set outside a
    // transaction, so we handle the table swap in Rust code.
    let has_feedback_requests: bool = conn
        .prepare("SELECT id FROM feedback_requests LIMIT 0")
        .is_ok();
    if !has_feedback_requests {
        // Temporarily disable FK enforcement for the table swap
        conn.pragma_update(None, "foreign_keys", "off")?;

        conn.execute_batch(
            "CREATE TABLE agent_runs_new (
                id                TEXT PRIMARY KEY,
                worktree_id       TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
                claude_session_id TEXT,
                prompt            TEXT NOT NULL,
                status            TEXT NOT NULL DEFAULT 'running'
                                  CHECK (status IN ('running','completed','failed','cancelled','waiting_for_feedback')),
                result_text       TEXT,
                cost_usd          REAL,
                num_turns         INTEGER,
                duration_ms       INTEGER,
                started_at        TEXT NOT NULL,
                ended_at          TEXT,
                tmux_window       TEXT,
                log_file          TEXT,
                model             TEXT,
                plan              TEXT,
                parent_run_id     TEXT REFERENCES agent_runs_new(id) ON DELETE SET NULL
            );
            INSERT INTO agent_runs_new SELECT id, worktree_id, claude_session_id, prompt, status,
                result_text, cost_usd, num_turns, duration_ms, started_at, ended_at,
                tmux_window, log_file, model, plan, parent_run_id FROM agent_runs;
            DROP TABLE agent_runs;
            ALTER TABLE agent_runs_new RENAME TO agent_runs;
            CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
            CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree ON agent_runs(worktree_id);",
        )?;

        // Re-enable FK enforcement
        conn.pragma_update(None, "foreign_keys", "on")?;

        // Now create the feedback_requests table
        conn.execute_batch(include_str!("migrations/018_feedback_requests.sql"))?;
    }
    if version < 18 {
        bump_version(conn, 18)?;
    }

    // Migration 019: drop review_configs table (reviewer roles now file-based).
    if version < 19 {
        conn.execute_batch(include_str!("migrations/019_drop_review_configs.sql"))?;
        bump_version(conn, 19)?;
    }

    Ok(())
}
