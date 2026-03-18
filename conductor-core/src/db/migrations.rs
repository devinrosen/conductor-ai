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

/// Reads the current `foreign_keys` pragma value, disables FK enforcement,
/// runs the provided closure, and unconditionally restores the original value —
/// even if the closure returns an error.
fn with_foreign_keys_off<F>(conn: &Connection, f: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    let fk_was_on: i64 = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    conn.pragma_update(None, "foreign_keys", "off")?;
    let result = f();
    // Always restore original state, even if `f` errored.
    let restore_val = if fk_was_on != 0 { "on" } else { "off" };
    let restore_result: Result<()> = conn
        .pragma_update(None, "foreign_keys", restore_val)
        .map_err(Into::into);
    match result {
        // Closure failed: return original error; discard restore error to avoid masking it.
        Err(original) => Err(original),
        // Closure succeeded: propagate any restore error so FK enforcement is never silently lost.
        Ok(()) => restore_result,
    }
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
    if version < 2 {
        let has_completed_at: bool = conn
            .prepare("SELECT completed_at FROM worktrees LIMIT 0")
            .is_ok();
        if !has_completed_at {
            conn.execute_batch(include_str!("migrations/002_worktree_completed_at.sql"))?;
        }
        bump_version(conn, 2)?;
    }

    if version < 3 {
        conn.execute_batch(include_str!("migrations/003_agent_runs.sql"))?;
        bump_version(conn, 3)?;
    }

    // Migration 004: add tmux_window to agent_runs.
    // Check column existence to handle DBs that already have it from feature branches.
    if version < 4 {
        let has_tmux_window: bool = conn
            .prepare("SELECT tmux_window FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_tmux_window {
            conn.execute_batch(include_str!("migrations/004_agent_tmux.sql"))?;
        }
        bump_version(conn, 4)?;
    }

    // Migration 005: add log_file to agent_runs.
    if version < 5 {
        let has_log_file: bool = conn
            .prepare("SELECT log_file FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_log_file {
            conn.execute_batch(include_str!("migrations/005_agent_log_file.sql"))?;
        }
        bump_version(conn, 5)?;
    }

    // Migration 006: drop sessions and session_worktrees tables.
    if version < 6 {
        conn.execute_batch(include_str!("migrations/006_drop_sessions.sql"))?;
        bump_version(conn, 6)?;
    }

    // Migration 007: add agent_run_events table (trace/span model).
    if version < 7 {
        let has_agent_run_events: bool = conn
            .prepare("SELECT id FROM agent_run_events LIMIT 0")
            .is_ok();
        if !has_agent_run_events {
            conn.execute_batch(include_str!("migrations/007_agent_run_events.sql"))?;
        }
        bump_version(conn, 7)?;
    }

    // Migration 008: add model column to worktrees.
    if version < 8 {
        let has_worktree_model: bool = conn.prepare("SELECT model FROM worktrees LIMIT 0").is_ok();
        if !has_worktree_model {
            conn.execute_batch(include_str!("migrations/008_worktree_model.sql"))?;
        }
        bump_version(conn, 8)?;
    }

    // Migration 009: add model column to agent_runs.
    if version < 9 {
        let has_agent_run_model: bool =
            conn.prepare("SELECT model FROM agent_runs LIMIT 0").is_ok();
        if !has_agent_run_model {
            conn.execute_batch(include_str!("migrations/009_agent_run_model.sql"))?;
        }
        bump_version(conn, 9)?;
    }

    // Migration 010: add model column to repos.
    if version < 10 {
        let has_repo_model: bool = conn.prepare("SELECT model FROM repos LIMIT 0").is_ok();
        if !has_repo_model {
            conn.execute_batch(include_str!("migrations/010_repo_model.sql"))?;
        }
        bump_version(conn, 10)?;
    }

    // Migration 011: add plan column to agent_runs.
    if version < 11 {
        let has_plan: bool = conn.prepare("SELECT plan FROM agent_runs LIMIT 0").is_ok();
        if !has_plan {
            conn.execute_batch(include_str!("migrations/011_agent_plan.sql"))?;
        }
        bump_version(conn, 11)?;
    }

    // Migration 012: add parent_run_id to agent_runs for parent/child relationships.
    if version < 12 {
        let has_parent_run_id: bool = conn
            .prepare("SELECT parent_run_id FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_parent_run_id {
            conn.execute_batch(include_str!("migrations/012_parent_run_id.sql"))?;
        }
        bump_version(conn, 12)?;
    }

    // Migration 013: add agent_created_issues table.
    if version < 13 {
        let has_agent_created_issues: bool = conn
            .prepare("SELECT id FROM agent_created_issues LIMIT 0")
            .is_ok();
        if !has_agent_created_issues {
            conn.execute_batch(include_str!("migrations/007_agent_created_issues.sql"))?;
        }
        bump_version(conn, 13)?;
    }

    // Migration 014: add allow_agent_issue_creation to repos.
    if version < 14 {
        let has_allow_agent_issue_creation: bool = conn
            .prepare("SELECT allow_agent_issue_creation FROM repos LIMIT 0")
            .is_ok();
        if !has_allow_agent_issue_creation {
            conn.execute_batch(include_str!("migrations/008_repo_allow_agent_issues.sql"))?;
        }
        bump_version(conn, 14)?;
    }

    // Migration 015: create agent_run_steps table and migrate JSON plan data.
    if version < 15 {
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
                        let step_id = crate::new_id();
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
        bump_version(conn, 15)?;
    }

    // Migration 017: create review_configs table for multi-agent PR review swarms.
    if version < 17 {
        let has_review_configs: bool = conn
            .prepare("SELECT id FROM review_configs LIMIT 0")
            .is_ok();
        if !has_review_configs {
            conn.execute_batch(include_str!("migrations/017_review_configs.sql"))?;
        }
        bump_version(conn, 17)?;
    }

    // Migration 018: feedback_requests table + update agent_runs CHECK constraint.
    // The agent_runs table must be recreated to add 'waiting_for_feedback' to the
    // status CHECK constraint. PRAGMA foreign_keys = OFF must be set outside a
    // transaction, so we handle the table swap in Rust code.
    if version < 18 {
        let has_feedback_requests: bool = conn
            .prepare("SELECT id FROM feedback_requests LIMIT 0")
            .is_ok();
        if !has_feedback_requests {
            with_foreign_keys_off(conn, || {
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
                Ok(())
            })?;

            // Now create the feedback_requests table
            conn.execute_batch(include_str!("migrations/018_feedback_requests.sql"))?;
        }
        bump_version(conn, 18)?;
    }

    // Migration 019: drop review_configs table (reviewer roles now file-based).
    if version < 19 {
        conn.execute_batch(include_str!("migrations/019_drop_review_configs.sql"))?;
        bump_version(conn, 19)?;
    }

    // Migration 020: workflow_runs and workflow_run_steps tables.
    if version < 20 {
        let has_workflow_runs: bool = conn.prepare("SELECT id FROM workflow_runs LIMIT 0").is_ok();
        if !has_workflow_runs {
            conn.execute_batch(include_str!("migrations/020_workflow_runs.sql"))?;
        }
        bump_version(conn, 20)?;
    }

    // Migration 021: workflow redesign — add structured output, iteration,
    // parallel, retry, gate, and snapshot columns.
    if version < 21 {
        let has_definition_snapshot: bool = conn
            .prepare("SELECT definition_snapshot FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_definition_snapshot {
            conn.execute_batch(include_str!("migrations/021_workflow_redesign.sql"))?;
        }
        // Recreate tables to update CHECK constraints (add 'waiting' status).
        with_foreign_keys_off(conn, || {
            conn.execute_batch(
                "CREATE TABLE workflow_runs_new (
                id                  TEXT PRIMARY KEY,
                workflow_name       TEXT NOT NULL,
                worktree_id         TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
                parent_run_id       TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
                status              TEXT NOT NULL DEFAULT 'pending'
                                    CHECK (status IN ('pending','running','completed','failed','cancelled','waiting')),
                dry_run             INTEGER NOT NULL DEFAULT 0,
                trigger             TEXT NOT NULL DEFAULT 'manual',
                started_at          TEXT NOT NULL,
                ended_at            TEXT,
                result_summary      TEXT,
                definition_snapshot TEXT
            );
            INSERT INTO workflow_runs_new SELECT id, workflow_name, worktree_id, parent_run_id,
                status, dry_run, trigger, started_at, ended_at, result_summary, definition_snapshot
                FROM workflow_runs;
            DROP TABLE workflow_runs;
            ALTER TABLE workflow_runs_new RENAME TO workflow_runs;
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);",
            )?;

            conn.execute_batch(
                "CREATE TABLE workflow_run_steps_new (
                id                TEXT PRIMARY KEY,
                workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                step_name         TEXT NOT NULL,
                role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate')),
                can_commit        INTEGER NOT NULL DEFAULT 0,
                condition_expr    TEXT,
                status            TEXT NOT NULL DEFAULT 'pending'
                                  CHECK (status IN ('pending','running','completed','failed','skipped','waiting')),
                child_run_id      TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
                position          INTEGER NOT NULL,
                started_at        TEXT,
                ended_at          TEXT,
                result_text       TEXT,
                condition_met     INTEGER,
                iteration         INTEGER NOT NULL DEFAULT 0,
                parallel_group_id TEXT,
                context_out       TEXT,
                markers_out       TEXT,
                retry_count       INTEGER NOT NULL DEFAULT 0,
                gate_type         TEXT,
                gate_prompt       TEXT,
                gate_timeout      TEXT,
                gate_approved_by  TEXT,
                gate_approved_at  TEXT,
                gate_feedback     TEXT
            );
            INSERT INTO workflow_run_steps_new SELECT id, workflow_run_id, step_name, role,
                can_commit, condition_expr, status, child_run_id, position, started_at, ended_at,
                result_text, condition_met, iteration, parallel_group_id, context_out, markers_out,
                retry_count, gate_type, gate_prompt, gate_timeout, gate_approved_by,
                gate_approved_at, gate_feedback
                FROM workflow_run_steps;
            DROP TABLE workflow_run_steps;
            ALTER TABLE workflow_run_steps_new RENAME TO workflow_run_steps;
            CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);",
            )?;

            Ok(())
        })?;
        bump_version(conn, 21)?;
    }

    // Migration 022: add base_branch column to worktrees.
    if version < 22 {
        conn.execute_batch(include_str!("migrations/022_worktree_base_branch.sql"))?;
        bump_version(conn, 22)?;
    }

    // Migration 023: add structured_output column to workflow_run_steps.
    if version < 23 {
        conn.execute_batch(include_str!("migrations/023_structured_output.sql"))?;
        bump_version(conn, 23)?;
    }

    // Migration 024: add 'timed_out' to the workflow_run_steps status CHECK constraint.
    // SQLite requires a table swap because ALTER TABLE cannot modify CHECK constraints.
    // PRAGMA foreign_keys = OFF must be done outside a transaction (handled in Rust).
    if version < 24 {
        with_foreign_keys_off(conn, || {
            conn.execute_batch(
                "BEGIN;
            CREATE TABLE workflow_run_steps_new (
                id                TEXT PRIMARY KEY,
                workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                step_name         TEXT NOT NULL,
                role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate')),
                can_commit        INTEGER NOT NULL DEFAULT 0,
                condition_expr    TEXT,
                status            TEXT NOT NULL DEFAULT 'pending'
                                  CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
                child_run_id      TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
                position          INTEGER NOT NULL,
                started_at        TEXT,
                ended_at          TEXT,
                result_text       TEXT,
                condition_met     INTEGER,
                iteration         INTEGER NOT NULL DEFAULT 0,
                parallel_group_id TEXT,
                context_out       TEXT,
                markers_out       TEXT,
                retry_count       INTEGER NOT NULL DEFAULT 0,
                gate_type         TEXT,
                gate_prompt       TEXT,
                gate_timeout      TEXT,
                gate_approved_by  TEXT,
                gate_approved_at  TEXT,
                gate_feedback     TEXT,
                structured_output TEXT
            );
            INSERT INTO workflow_run_steps_new SELECT
                id, workflow_run_id, step_name, role, can_commit, condition_expr,
                status, child_run_id, position, started_at, ended_at, result_text,
                condition_met, iteration, parallel_group_id, context_out, markers_out,
                retry_count, gate_type, gate_prompt, gate_timeout, gate_approved_by,
                gate_approved_at, gate_feedback, structured_output
                FROM workflow_run_steps;
            DROP TABLE workflow_run_steps;
            ALTER TABLE workflow_run_steps_new RENAME TO workflow_run_steps;
            CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);
            COMMIT;",
            )?;
            Ok(())
        })?;
        bump_version(conn, 24)?;
    }

    // Migration 025: add 'workflow' to the workflow_run_steps role CHECK constraint.
    if version < 25 {
        with_foreign_keys_off(conn, || {
            conn.execute_batch(
                "BEGIN;
            CREATE TABLE workflow_run_steps_new (
                id                TEXT PRIMARY KEY,
                workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                step_name         TEXT NOT NULL,
                role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow')),
                can_commit        INTEGER NOT NULL DEFAULT 0,
                condition_expr    TEXT,
                status            TEXT NOT NULL DEFAULT 'pending'
                                  CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
                child_run_id      TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
                position          INTEGER NOT NULL,
                started_at        TEXT,
                ended_at          TEXT,
                result_text       TEXT,
                condition_met     INTEGER,
                iteration         INTEGER NOT NULL DEFAULT 0,
                parallel_group_id TEXT,
                context_out       TEXT,
                markers_out       TEXT,
                retry_count       INTEGER NOT NULL DEFAULT 0,
                gate_type         TEXT,
                gate_prompt       TEXT,
                gate_timeout      TEXT,
                gate_approved_by  TEXT,
                gate_approved_at  TEXT,
                gate_feedback     TEXT,
                structured_output TEXT
            );
            INSERT INTO workflow_run_steps_new SELECT
                id, workflow_run_id, step_name, role, can_commit, condition_expr,
                status, child_run_id, position, started_at, ended_at, result_text,
                condition_met, iteration, parallel_group_id, context_out, markers_out,
                retry_count, gate_type, gate_prompt, gate_timeout, gate_approved_by,
                gate_approved_at, gate_feedback, structured_output
                FROM workflow_run_steps;
            DROP TABLE workflow_run_steps;
            ALTER TABLE workflow_run_steps_new RENAME TO workflow_run_steps;
            CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);
            COMMIT;",
            )?;
            Ok(())
        })?;
        bump_version(conn, 25)?;
    }

    // Migration 026: add inputs column to workflow_runs for resume support.
    if version < 26 {
        let has_workflow_run_inputs: bool = conn
            .prepare("SELECT inputs FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_workflow_run_inputs {
            conn.execute_batch(include_str!("migrations/026_workflow_run_inputs.sql"))?;
        }
        bump_version(conn, 26)?;
    }

    // Migration 027: make workflow_runs.worktree_id nullable (for ephemeral PR runs),
    // and make agent_runs.worktree_id nullable with FK preserved (for ephemeral PR runs
    // that have no registered worktree).
    if version < 27 {
        with_foreign_keys_off(conn, || {
            conn.execute_batch(
                "BEGIN;

            -- Recreate workflow_runs with nullable worktree_id
            CREATE TABLE workflow_runs_new (
                id                  TEXT PRIMARY KEY,
                workflow_name       TEXT NOT NULL,
                worktree_id         TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
                parent_run_id       TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
                status              TEXT NOT NULL DEFAULT 'pending'
                                    CHECK (status IN ('pending','running','completed','failed','cancelled','waiting')),
                dry_run             INTEGER NOT NULL DEFAULT 0,
                trigger             TEXT NOT NULL DEFAULT 'manual',
                started_at          TEXT NOT NULL,
                ended_at            TEXT,
                result_summary      TEXT,
                definition_snapshot TEXT,
                inputs              TEXT
            );
            INSERT INTO workflow_runs_new SELECT * FROM workflow_runs;
            DROP TABLE workflow_runs;
            ALTER TABLE workflow_runs_new RENAME TO workflow_runs;
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_worktree ON workflow_runs(worktree_id);
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent ON workflow_runs(parent_run_id);

            -- Recreate agent_runs with nullable worktree_id
            CREATE TABLE agent_runs_new (
                id                TEXT PRIMARY KEY,
                worktree_id       TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
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
            INSERT INTO agent_runs_new SELECT * FROM agent_runs;
            DROP TABLE agent_runs;
            ALTER TABLE agent_runs_new RENAME TO agent_runs;
            CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
            CREATE INDEX IF NOT EXISTS idx_agent_runs_worktree ON agent_runs(worktree_id);

            COMMIT;",
            )?;
            Ok(())
        })?;
        bump_version(conn, 27)?;
    }

    // Migration 028: drop the merge_queue table (replaced by gh pr merge --auto).
    if version < 28 {
        conn.execute_batch(include_str!("migrations/028_drop_merge_queue.sql"))?;
        bump_version(conn, 28)?;
    }

    // Migration 029: ticket_labels join table.
    if version < 29 {
        conn.execute_batch(include_str!("migrations/029_ticket_labels.sql"))?;
        bump_version(conn, 29)?;
    }

    // Migration 030: add ticket_id and repo_id to workflow_runs for workflow targets.
    if version < 30 {
        conn.execute_batch(include_str!("migrations/030_workflow_targets.sql"))?;
        bump_version(conn, 30)?;
    }

    // Migration 031: add parent_workflow_run_id to workflow_runs for sub-workflow linking.
    if version < 31 {
        conn.execute_batch(include_str!("migrations/031_workflow_parent_run_id.sql"))?;
        bump_version(conn, 31)?;
    }

    // Migration 032: add token count columns to agent_runs.
    if version < 32 {
        let has_input_tokens: bool = conn
            .prepare("SELECT input_tokens FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_input_tokens {
            conn.execute_batch(include_str!("migrations/032_agent_run_token_counts.sql"))?;
        }
        bump_version(conn, 32)?;
    }

    // Migration 033: add target_label column to workflow_runs.
    if version < 33 {
        let has_target_label: bool = conn
            .prepare("SELECT target_label FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_target_label {
            conn.execute_batch(include_str!("migrations/033_workflow_target_label.sql"))?;
        }
        bump_version(conn, 33)?;
    }

    // Migration 034: add bot_name column to agent_runs.
    if version < 34 {
        let has_bot_name: bool = conn
            .prepare("SELECT bot_name FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_bot_name {
            conn.execute_batch(include_str!("migrations/034_agent_run_bot_name.sql"))?;
        }
        bump_version(conn, 34)?;
    }

    // Migration 035: add default_bot_name column to workflow_runs.
    if version < 35 {
        let has_wf_default_bot_name: bool = conn
            .prepare("SELECT default_bot_name FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_wf_default_bot_name {
            conn.execute_batch(include_str!(
                "migrations/035_workflow_run_default_bot_name.sql"
            ))?;
        }
        bump_version(conn, 35)?;
    }

    // Migration 036: drop source_type CHECK constraint from tickets and repo_issue_sources.
    // SQLite cannot drop CHECK constraints in-place; a table swap is required.
    // PRAGMA foreign_keys = OFF must be set outside a transaction (handled in Rust).
    if version < 36 {
        with_foreign_keys_off(conn, || {
            conn.execute_batch(include_str!("migrations/036_drop_source_type_check.sql"))?;
            Ok(())
        })?;
        bump_version(conn, 36)?;
    }

    // Migration 037: add 'script' to the role CHECK constraint and add output_file column.
    // Requires a table swap (FK constraint). Guarded by output_file column existence
    // to be idempotent and to skip gracefully on partial test schemas.
    if version < 37 {
        let has_output_file: bool = conn
            .prepare("SELECT output_file FROM workflow_run_steps LIMIT 0")
            .is_ok();
        if !has_output_file {
            let steps_table_exists = conn
                .prepare("SELECT 1 FROM workflow_run_steps LIMIT 0")
                .is_ok();
            if steps_table_exists {
                with_foreign_keys_off(conn, || {
                    conn.execute_batch(include_str!(
                        "migrations/037_workflow_step_output_file.sql"
                    ))?;
                    Ok(())
                })?;
            }
        }
        bump_version(conn, 37)?;
    }

    // Migration 038: notification_log table for cross-process dedup.
    if version < 38 {
        conn.execute_batch(include_str!("migrations/038_notification_log.sql"))?;
        bump_version(conn, 38)?;
    }

    // Migration 039: composite index on workflow_run_steps(status, gate_type)
    // for list_all_waiting_gate_steps poll performance.
    // Guard: only create the index if the table exists (it may be absent in
    // minimal test schemas that start at version > 20).
    if version < 39 {
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='workflow_run_steps'",
            [],
            |row| row.get(0),
        )?;
        if table_exists {
            conn.execute_batch(include_str!("migrations/039_idx_steps_status_gate.sql"))?;
        }
        bump_version(conn, 39)?;
    }

    // Migration 040: add iteration column to workflow_runs for loop-based tree filtering.
    let has_wf_run_iteration: bool = conn
        .prepare("SELECT iteration FROM workflow_runs LIMIT 0")
        .is_ok();
    if !has_wf_run_iteration {
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='workflow_runs'",
            [],
            |row| row.get(0),
        )?;
        if table_exists {
            conn.execute_batch(include_str!("migrations/040_workflow_run_iteration.sql"))?;
        }
    }
    if version < 40 {
        bump_version(conn, 40)?;
    }

    // --- Migration 41: workflow_runs.blocked_on ---
    let has_blocked_on: bool = conn
        .prepare("SELECT blocked_on FROM workflow_runs LIMIT 0")
        .is_ok();
    if !has_blocked_on {
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='workflow_runs'",
            [],
            |row| row.get(0),
        )?;
        if table_exists {
            conn.execute_batch(include_str!("migrations/041_workflow_run_blocked_on.sql"))?;
        }
    }
    if version < 41 {
        bump_version(conn, 41)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Verifies that migration 027 preserves existing rows in `workflow_runs` and
    /// `agent_runs` when re-creating the tables to make `worktree_id` nullable.
    ///
    /// Sets up the schema as it exists at version 26 (NOT NULL worktree_id),
    /// inserts test rows, applies migration 027, then asserts the rows survived.
    #[test]
    fn test_migration_027_preserves_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        // Disable FK enforcement while building the simplified pre-027 schema.
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        // Build the minimal schema matching version 26.  The column order must
        // exactly match what migration 027 does with `INSERT … SELECT *`.
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE repos (
                id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE,
                local_path TEXT NOT NULL, remote_url TEXT NOT NULL,
                default_branch TEXT NOT NULL, workspace_dir TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE worktrees (
                id TEXT PRIMARY KEY, repo_id TEXT NOT NULL,
                slug TEXT NOT NULL, branch TEXT NOT NULL, path TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL
            );
            CREATE TABLE repo_issue_sources (
                id          TEXT PRIMARY KEY,
                repo_id     TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                source_type TEXT NOT NULL CHECK (source_type IN ('github', 'jira')),
                config_json TEXT NOT NULL
            );
            CREATE TABLE tickets (
                id TEXT PRIMARY KEY, repo_id TEXT NOT NULL,
                source_type TEXT NOT NULL, source_id TEXT NOT NULL,
                title TEXT NOT NULL, body TEXT, url TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'open', priority TEXT,
                labels TEXT, assignee TEXT, created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            -- agent_runs at version 26: worktree_id NOT NULL, columns in order
            CREATE TABLE agent_runs (
                id                TEXT PRIMARY KEY,
                worktree_id       TEXT NOT NULL,
                claude_session_id TEXT,
                prompt            TEXT NOT NULL,
                status            TEXT NOT NULL DEFAULT 'running',
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
                parent_run_id     TEXT
            );
            -- workflow_runs at version 26: worktree_id NOT NULL, columns in order
            CREATE TABLE workflow_runs (
                id                  TEXT PRIMARY KEY,
                workflow_name       TEXT NOT NULL,
                worktree_id         TEXT NOT NULL,
                parent_run_id       TEXT NOT NULL,
                status              TEXT NOT NULL DEFAULT 'pending',
                dry_run             INTEGER NOT NULL DEFAULT 0,
                trigger             TEXT NOT NULL DEFAULT 'manual',
                started_at          TEXT NOT NULL,
                ended_at            TEXT,
                result_summary      TEXT,
                definition_snapshot TEXT,
                inputs              TEXT
            );
            INSERT INTO _conductor_meta VALUES ('schema_version', '26');
            INSERT INTO repos VALUES ('r1', 'test-repo', '/tmp/repo',
                'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z');
            INSERT INTO worktrees VALUES ('w1', 'r1', 'feat-test', 'feat/test',
                '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z');
            INSERT INTO agent_runs (id, worktree_id, prompt, started_at)
                VALUES ('ar1', 'w1', 'workflow', '2024-01-01T00:00:00Z');
            INSERT INTO workflow_runs (id, workflow_name, worktree_id, parent_run_id,
                status, dry_run, trigger, started_at)
                VALUES ('wfr1', 'my-flow', 'w1', 'ar1',
                        'completed', 0, 'manual', '2024-01-01T00:00:00Z');",
        )
        .unwrap();

        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Apply migration 027 (the only pending migration given version = 26).
        run(&conn).unwrap();

        // The original workflow_runs row must survive the table recreation.
        let (name, wt_id): (String, Option<String>) = conn
            .query_row(
                "SELECT workflow_name, worktree_id FROM workflow_runs WHERE id = 'wfr1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("workflow_runs row must survive migration 027");
        assert_eq!(name, "my-flow");
        assert_eq!(wt_id.as_deref(), Some("w1"));

        // The original agent_runs row must also survive.
        let ar_wt_id: Option<String> = conn
            .query_row(
                "SELECT worktree_id FROM agent_runs WHERE id = 'ar1'",
                [],
                |row| row.get(0),
            )
            .expect("agent_runs row must survive migration 027");
        assert_eq!(ar_wt_id.as_deref(), Some("w1"));

        // After migration 027, worktree_id is nullable — a NULL insert must succeed.
        conn.execute(
            "INSERT INTO agent_runs (id, prompt, started_at) \
             VALUES ('ar2', 'ephemeral', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("agent_runs must accept NULL worktree_id after migration 027");

        conn.execute(
            "INSERT INTO workflow_runs \
             (id, workflow_name, worktree_id, parent_run_id, status, dry_run, trigger, started_at) \
             VALUES ('wfr2', 'eph-flow', NULL, 'ar2', 'running', 0, 'manual', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("workflow_runs must accept NULL worktree_id after migration 027");

        let null_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_runs WHERE worktree_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null_count, 1);
    }

    /// Verifies that `with_foreign_keys_off` restores FK enforcement even when
    /// the closure returns an error.
    #[test]
    fn test_foreign_keys_restored_on_migration_error() {
        let conn = Connection::open_in_memory().unwrap();

        // Enable FK enforcement before the call.
        conn.pragma_update(None, "foreign_keys", "on").unwrap();

        // Call with a closure that always fails.
        let result = with_foreign_keys_off(&conn, || {
            Err(crate::error::ConductorError::Git(
                "simulated migration error".to_string(),
            ))
        });
        assert!(result.is_err(), "helper must propagate the closure error");

        // FK pragma must be restored to ON despite the error.
        let fk_on: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(
            fk_on, 1,
            "foreign_keys pragma must be restored to ON after closure error"
        );
    }
}
