use rusqlite::{params, Connection};
use serde::Deserialize;

use crate::error::{ConductorError, Result};

/// The highest migration version this binary knows about.
/// **When adding a new migration, update this constant to match the new version.**
pub const LATEST_SCHEMA_VERSION: u32 = 79;

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

/// Migration 45 helper: copy `default_branch` and `model` column values from
/// the repos table into per-repo `.conductor/config.toml` files before the
/// columns are dropped. Errors are logged but do not abort the migration — the
/// worst case is that the user must re-set a repo-level override.
fn migrate_repo_columns_to_config(conn: &Connection) {
    use crate::config::RepoConfig;
    use std::path::Path;

    // The columns may not exist (fresh DB or already dropped by a prior attempt).
    let has_default_branch: bool = conn
        .prepare("SELECT default_branch FROM repos LIMIT 0")
        .is_ok();
    let has_model: bool = conn.prepare("SELECT model FROM repos LIMIT 0").is_ok();
    if !has_default_branch && !has_model {
        return;
    }

    // Build a query that reads whatever columns exist.
    let sql = if has_default_branch && has_model {
        "SELECT local_path, default_branch, model FROM repos"
    } else if has_default_branch {
        "SELECT local_path, default_branch, NULL FROM repos"
    } else {
        "SELECT local_path, NULL, model FROM repos"
    };

    let Ok(mut stmt) = conn.prepare(sql) else {
        return;
    };
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .ok();
    let Some(rows) = rows else { return };

    for row in rows.flatten() {
        let (local_path, default_branch, model) = row;
        let repo_path = Path::new(&local_path);

        // Skip if both are empty/default — nothing to migrate.
        let branch_is_custom = default_branch
            .as_deref()
            .is_some_and(|b| !b.is_empty() && b != "main");
        let model_is_set = model.as_deref().is_some_and(|m| !m.is_empty());
        if !branch_is_custom && !model_is_set {
            continue;
        }

        // Load existing repo config (or defaults) and merge the DB values.
        let mut rc = RepoConfig::load(repo_path).unwrap_or_default();
        if branch_is_custom && rc.defaults.default_branch.is_none() {
            rc.defaults.default_branch = default_branch;
        }
        if model_is_set && rc.defaults.model.is_none() {
            rc.defaults.model = model;
        }
        if let Err(e) = rc.save(repo_path) {
            tracing::warn!(
                path = %local_path,
                "migration 45: failed to write .conductor/config.toml: {e}"
            );
        }
    }
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

    // Stale-binary check: if the DB schema version is newer than what this
    // binary understands, another (newer) binary already migrated the DB.
    // Continuing would produce cryptic "no such column" SQL errors.
    // We check here (before running migrations) because if version >
    // LATEST_SCHEMA_VERSION, none of the `version < N` guards below will
    // fire, so `version` still equals the on-disk schema version.
    if version > LATEST_SCHEMA_VERSION as i64 {
        return Err(ConductorError::Schema(format!(
            "Database schema version ({version}) is newer than this binary supports ({LATEST_SCHEMA_VERSION}). \
             Please rebuild: `cargo build`"
        )));
    }

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
                conn.execute_batch(include_str!(
                    "migrations/018_agent_runs_check_constraint.sql"
                ))?;
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
            conn.execute_batch(include_str!("migrations/021_workflow_runs_table_swap.sql"))?;

            conn.execute_batch(include_str!(
                "migrations/021_workflow_run_steps_table_swap.sql"
            ))?;

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
            conn.execute_batch(include_str!(
                "migrations/024_workflow_run_steps_timed_out.sql"
            ))?;
            Ok(())
        })?;
        bump_version(conn, 24)?;
    }

    // Migration 025: add 'workflow' to the workflow_run_steps role CHECK constraint.
    if version < 25 {
        with_foreign_keys_off(conn, || {
            conn.execute_batch(include_str!(
                "migrations/025_workflow_run_steps_workflow_role.sql"
            ))?;
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
            conn.execute_batch(include_str!("migrations/027_nullable_worktree_id.sql"))?;
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
    // Requires a table swap (FK constraint). The table swap serves two purposes:
    // (1) add output_file column, (2) update role CHECK to include 'script'.
    // We must run the swap if EITHER is missing (column absent OR constraint stale).
    if version < 37 {
        let has_output_file: bool = conn
            .prepare("SELECT output_file FROM workflow_run_steps LIMIT 0")
            .is_ok();
        let needs_swap = if !has_output_file {
            // Column missing — need the swap if the table exists at all.
            conn.prepare("SELECT 1 FROM workflow_run_steps LIMIT 0")
                .is_ok()
        } else {
            // Column exists — check if the CHECK constraint includes 'script'.
            let has_script_role: bool = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='workflow_run_steps'",
                    [],
                    |row| {
                        let ddl: String = row.get(0)?;
                        Ok(ddl.contains("'script'"))
                    },
                )
                .unwrap_or(false);
            !has_script_role
        };
        if needs_swap {
            with_foreign_keys_off(conn, || {
                conn.execute_batch(include_str!("migrations/037_workflow_step_output_file.sql"))?;
                Ok(())
            })?;
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
    if version < 41 {
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
        bump_version(conn, 41)?;
    }

    // --- Migration 42: features + feature_tickets tables ---
    if version < 42 {
        conn.execute_batch(include_str!("migrations/042_features.sql"))?;
        bump_version(conn, 42)?;
    }

    // --- Migration 43: index on worktrees(repo_id, base_branch) for feature list subquery ---
    if version < 43 {
        conn.execute_batch(include_str!(
            "migrations/043_idx_worktrees_repo_base_branch.sql"
        ))?;
        bump_version(conn, 43)?;
    }

    if version < 44 {
        conn.execute_batch(include_str!("migrations/044_workflow_run_feature_id.sql"))?;
        bump_version(conn, 44)?;
    }

    if version < 45 {
        // Before dropping the columns, migrate any non-default default_branch values
        // to per-repo .conductor/config.toml so they are not lost.
        migrate_repo_columns_to_config(conn);
        conn.execute_batch(include_str!(
            "migrations/045_drop_repo_model_default_branch.sql"
        ))?;
        bump_version(conn, 45)?;
    }

    // Migration 046: notifications table for in-app notification system.
    if version < 46 {
        conn.execute_batch(include_str!("migrations/046_notifications.sql"))?;
        bump_version(conn, 46)?;
    }

    // Migration 047: widen trigger CHECK to include 'hook'.
    // SQLite cannot alter CHECK constraints in-place; a table swap is required.
    // Also recreates indexes dropped by the table swap (ticket, repo, parent_wf).
    if version < 47 {
        // Guard: check if the trigger CHECK already includes 'hook' by inspecting
        // the table schema DDL.
        let schema_sql: String = conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='workflow_runs'",
            [],
            |row| row.get(0),
        )?;
        let needs_table_swap =
            !schema_sql.contains("'hook'") || schema_sql.contains("triggered_by_hook");
        if needs_table_swap {
            // Either the CHECK is missing 'hook' or the old `triggered_by_hook`
            // column still exists — both require a full table swap.
            with_foreign_keys_off(conn, || {
                conn.execute_batch(include_str!("migrations/047_workflow_run_hooks.sql"))?;
                Ok(())
            })?;
        } else {
            // Table is up-to-date — just ensure indexes exist (idempotent).
            conn.execute_batch(include_str!("migrations/047_workflow_runs_indexes.sql"))?;
        }
        bump_version(conn, 47)?;
    }

    if version < 48 {
        conn.execute_batch(include_str!(
            "migrations/048_backfill_workflow_run_repo_id.sql"
        ))?;
        bump_version(conn, 48)?;
    }

    if version < 49 {
        conn.execute_batch(include_str!("migrations/049_feature_last_commit_at.sql"))?;
        bump_version(conn, 49)?;
    }

    if version < 50 {
        // Only ALTER if feedback_requests table exists (created in migration 18).
        let has_table: bool = conn
            .prepare("SELECT 1 FROM feedback_requests LIMIT 0")
            .is_ok();
        if has_table {
            let has_col: bool = conn
                .prepare("SELECT feedback_type FROM feedback_requests LIMIT 0")
                .is_ok();
            if !has_col {
                conn.execute_batch(include_str!("migrations/050_feedback_type_and_timeout.sql"))?;
            }
        }
        bump_version(conn, 50)?;
    }

    // Migration 051: add repo_id column to agent_runs for repo-scoped agents.
    if version < 51 {
        let has_repo_id: bool = conn
            .prepare("SELECT repo_id FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_repo_id {
            conn.execute_batch(include_str!("migrations/051_agent_run_repo_id.sql"))?;
        }
        bump_version(conn, 51)?;
    }

    // Migration 052: create push_subscriptions table for PWA push notifications.
    if version < 52 {
        let has_table: bool = conn
            .prepare("SELECT 1 FROM push_subscriptions LIMIT 0")
            .is_ok();
        if !has_table {
            conn.execute_batch(include_str!("migrations/052_push_subscriptions.sql"))?;
        }
        bump_version(conn, 52)?;
    }

    // Migration 053: covering index on agent_runs(worktree_id, started_at) to
    // speed up the latest-run-per-worktree subquery in list_all_with_status().
    if version < 53 {
        conn.execute_batch(include_str!(
            "migrations/053_idx_agent_runs_worktree_started.sql"
        ))?;
        bump_version(conn, 53)?;
    }

    // Migration 054: add workflow column to tickets table for routing overrides.
    if version < 54 {
        conn.execute_batch(include_str!("migrations/054_ticket_workflow.sql"))?;
        bump_version(conn, 54)?;
    }

    // Migration 055: add agent_map column to tickets table for pre-resolved agent assignments.
    if version < 55 {
        conn.execute_batch(include_str!("migrations/055_ticket_agent_map.sql"))?;
        bump_version(conn, 55)?;
    }

    // Migration 056: add gate_options and gate_selections columns to
    // workflow_run_steps for dynamic multi-select gate support.
    if version < 56 {
        conn.execute_batch(include_str!("migrations/056_gate_options.sql"))?;
        bump_version(conn, 56)?;
    }

    // Migration 057: backfill target_label for workflow_runs that have a
    // worktree_id but a NULL or empty target_label (pre-033 rows and TUI
    // race condition where cache hadn't refreshed after worktree creation).
    if version < 57 {
        conn.execute_batch(include_str!(
            "migrations/057_backfill_workflow_run_target_label.sql"
        ))?;
        bump_version(conn, 57)?;
    }

    // Migration 058: drop the FK constraint on workflow_run_steps.child_run_id
    // so that workflow-type steps can store a workflow_runs.id value (the iOS
    // app needs this for navigation to child workflow run detail views).
    if version < 58 {
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='workflow_run_steps'",
            [],
            |row| row.get(0),
        )?;
        if table_exists {
            with_foreign_keys_off(conn, || {
                conn.execute_batch(include_str!(
                    "migrations/058_workflow_step_child_run_id_drop_fk.sql"
                ))?;
                Ok(())
            })?;
        }
        bump_version(conn, 58)?;
    }

    // Migration 059: add 8 aggregated metrics columns to workflow_runs
    // (total_input_tokens, total_output_tokens, total_cache_read_input_tokens,
    //  total_cache_creation_input_tokens, total_turns, total_cost_usd,
    //  total_duration_ms, model). All nullable — no backfill required.
    if version < 59 {
        conn.execute_batch(include_str!("migrations/059_workflow_run_token_usage.sql"))?;
        bump_version(conn, 59)?;
    }

    // Migration 060: create conversations table for repo/worktree-scoped agent chat.
    if version < 60 {
        let has_table: bool = conn.prepare("SELECT 1 FROM conversations LIMIT 0").is_ok();
        if !has_table {
            conn.execute_batch(include_str!("migrations/060_conversations.sql"))?;
        }
        bump_version(conn, 60)?;
    }

    // Migration 061: add conversation_id FK to agent_runs.
    if version < 61 {
        let has_col: bool = conn
            .prepare("SELECT conversation_id FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_col {
            conn.execute_batch(include_str!(
                "migrations/061_agent_runs_conversation_id.sql"
            ))?;
        }
        bump_version(conn, 61)?;
    }

    // Migration 062: ticket_dependencies join table (RFC 009 prerequisite).
    if version < 62 {
        let has_table: bool = conn
            .prepare("SELECT 1 FROM ticket_dependencies LIMIT 0")
            .is_ok();
        if !has_table {
            conn.execute_batch(include_str!("migrations/062_ticket_dependencies.sql"))?;
        }
        bump_version(conn, 62)?;
    }

    // Migration 063: add dedicated error column to workflow_runs.
    if version < 63 {
        let has_col: bool = conn
            .prepare("SELECT error FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_col {
            conn.execute_batch(include_str!("migrations/063_workflow_run_error.sql"))?;
        }
        bump_version(conn, 63)?;
    }

    // Migration 064: add subprocess_pid for headless agent tracking (RFC 016).
    if version < 64 {
        let has_col: bool = conn
            .prepare("SELECT subprocess_pid FROM agent_runs LIMIT 0")
            .is_ok();
        if !has_col {
            conn.execute_batch(include_str!("migrations/064_subprocess_pid.sql"))?;
        }
        bump_version(conn, 64)?;
    }

    // Migration 065: add subprocess_pid to workflow_run_steps for script step
    // orphan detection (RFC 016).
    if version < 65 {
        let has_col: bool = conn
            .prepare("SELECT subprocess_pid FROM workflow_run_steps LIMIT 0")
            .is_ok();
        if !has_col {
            conn.execute_batch(include_str!(
                "migrations/065_workflow_step_subprocess_pid.sql"
            ))?;
        }
        bump_version(conn, 65)?;
    }

    // Migration 066: add last_heartbeat column to workflow_runs for the
    // heartbeat-based watchdog (#2041). NULL is handled by COALESCE(last_heartbeat, started_at)
    // in the detection query.
    if version < 66 {
        let has_col: bool = conn
            .prepare("SELECT last_heartbeat FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_col {
            conn.execute_batch(include_str!(
                "migrations/066_workflow_run_last_heartbeat.sql"
            ))?;
        }
        bump_version(conn, 66)?;
    }

    // Migration 067: add workflow_run_step_fan_out_items table and fan-out counter
    // columns on workflow_run_steps for foreach step type (RFC 010).
    //
    // Guard checks both the new table AND ALL four ALTER TABLE columns.  SQLite's
    // autocommit means CREATE TABLE and individual ALTER TABLEs can succeed or fail
    // independently, leaving any subset of the four columns present.  We handle
    // all partial states:
    //   • neither table nor any column present  → run the full SQL file
    //   • table present, ≥1 column missing      → add only the missing columns
    //   • table and all columns present          → nothing to do
    if version < 67 {
        let has_table: bool = conn
            .prepare("SELECT id FROM workflow_run_step_fan_out_items LIMIT 0")
            .is_ok();
        // Check all four columns — partial failure can leave any subset present.
        let has_total: bool = conn
            .prepare("SELECT fan_out_total FROM workflow_run_steps LIMIT 0")
            .is_ok();
        let has_completed: bool = conn
            .prepare("SELECT fan_out_completed FROM workflow_run_steps LIMIT 0")
            .is_ok();
        let has_failed: bool = conn
            .prepare("SELECT fan_out_failed FROM workflow_run_steps LIMIT 0")
            .is_ok();
        let has_skipped: bool = conn
            .prepare("SELECT fan_out_skipped FROM workflow_run_steps LIMIT 0")
            .is_ok();
        let has_all_cols = has_total && has_completed && has_failed && has_skipped;

        if !has_table && !has_all_cols {
            conn.execute_batch(include_str!(
                "migrations/067_workflow_run_step_fan_out_items.sql"
            ))?;
        } else if has_table && !has_all_cols {
            // Partial failure recovery: table was created but some ALTER TABLEs did
            // not run (or were interrupted mid-batch).  Add only the missing columns
            // so we don't re-add ones that already exist.
            if !has_total {
                conn.execute_batch(
                    "ALTER TABLE workflow_run_steps ADD COLUMN fan_out_total INTEGER;",
                )?;
            }
            if !has_completed {
                conn.execute_batch(
                    "ALTER TABLE workflow_run_steps ADD COLUMN fan_out_completed INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            if !has_failed {
                conn.execute_batch(
                    "ALTER TABLE workflow_run_steps ADD COLUMN fan_out_failed INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            if !has_skipped {
                conn.execute_batch(
                    "ALTER TABLE workflow_run_steps ADD COLUMN fan_out_skipped INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
        }
        bump_version(conn, 67)?;
    }

    // Migration 068: add 'foreach' to the role CHECK constraint on workflow_run_steps.
    // foreach.rs inserts rows with role='foreach' but the constraint only allowed
    // ('actor','reviewer','gate','workflow','script'). Uses table-recreation pattern
    // (same as 058) to include all columns from 058, 065, and 067.
    // workflow_run_step_fan_out_items (added in 067) holds an FK to workflow_run_steps,
    // so the DROP+RENAME sequence requires FK enforcement to be disabled first.
    if version < 68 {
        with_foreign_keys_off(conn, || {
            conn.execute_batch(include_str!(
                "migrations/068_workflow_step_foreach_role.sql"
            ))?;
            Ok(())
        })?;
        bump_version(conn, 68)?;
    }

    // Migration 069: add step_error TEXT to workflow_run_steps.
    // Persists schema validation error messages from call steps so they are
    // visible in `run-show` output and the web UI.
    if version < 69 {
        conn.execute_batch(include_str!("migrations/069_workflow_step_error.sql"))?;
        bump_version(conn, 69)?;
    }

    // Migration 070: RFC-018 features — source_type, source_id, tickets_total,
    // tickets_merged; expanded status CHECK; data-migrate active → in_progress.
    //
    // Guarded: only runs when the features table exists AND has a `status` column
    // (i.e. is the real schema from migration 042, not a minimal test stub).
    // Also skips if `source_type` is already present (idempotent against partial failure).
    if version < 70 {
        let has_status_col = conn.prepare("SELECT status FROM features LIMIT 0").is_ok();
        if has_status_col {
            let has_source_type = conn
                .prepare("SELECT source_type FROM features LIMIT 0")
                .is_ok();
            if !has_source_type {
                with_foreign_keys_off(conn, || {
                    conn.execute_batch(include_str!("migrations/070_features_rfc018.sql"))?;
                    Ok(())
                })?;
            }
        }
        bump_version(conn, 70)?;
    }

    // Migration 071: add 'needs_resume' to the workflow_runs.status CHECK constraint.
    // Table-swap required (SQLite cannot ALTER CHECK constraints in-place).
    // Skipped when needs_resume is already in the CHECK (idempotent guard via sqlite_master).
    // Also skipped when workflow_runs is a stub table (migration tests create minimal schemas
    // with nullable parent_run_id; the real table has `parent_run_id TEXT NOT NULL` from
    // migration 047). Intermediate ALTER TABLE migrations add columns like last_heartbeat to
    // stub tables, so column-presence checks are insufficient — we check the DDL directly.
    if version < 71 {
        // Guard: skip the table swap on stub DBs created by migration tests.
        // Stub tables use nullable `parent_run_id TEXT`; the real schema (from
        // migration 047) has `parent_run_id TEXT NOT NULL REFERENCES agent_runs`.
        // We detect the real table by checking for "REFERENCES agent_runs" after
        // "parent_run_id" in the sqlite_master DDL (stubs omit the FK clause).
        // Multi-space alignment in the real DDL is handled by `%` wildcards.
        let workflow_runs_has_full_schema: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='workflow_runs' \
                 AND sql LIKE '%parent_run_id%REFERENCES agent_runs%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        let already_migrated: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='workflow_runs' AND sql LIKE '%needs_resume%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if workflow_runs_has_full_schema && !already_migrated {
            with_foreign_keys_off(conn, || {
                conn.execute_batch(include_str!("migrations/071_workflow_run_needs_resume.sql"))?;
                Ok(())
            })?;
        }
        bump_version(conn, 71)?;
    }

    // Migration 072: Add 'worktree' to the item_type CHECK constraint on
    // workflow_run_step_fan_out_items. SQLite cannot ALTER CHECK constraints
    // in-place, so this uses the table-swap pattern with FK enforcement disabled.
    // Guard skips the swap when the table does not exist (test stubs) or when
    // 'worktree' is already present in the DDL (idempotent).
    if version < 72 {
        // Only run if the table exists AND has the full post-067 schema (child_run_id column).
        // Stub tables created by migration tests may lack these columns.
        let has_full_table = conn
            .prepare("SELECT child_run_id FROM workflow_run_step_fan_out_items LIMIT 0")
            .is_ok();
        if has_full_table {
            let already_has_worktree: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type='table' AND name='workflow_run_step_fan_out_items' \
                     AND sql LIKE '%worktree%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or_else(|e| {
                    tracing::warn!("migration 72: idempotency check failed, re-running: {e}");
                    false
                });
            if !already_has_worktree {
                with_foreign_keys_off(conn, || {
                    conn.execute_batch(include_str!(
                        "migrations/072_fan_out_item_type_worktree.sql"
                    ))?;
                    Ok(())
                })?;
            }
        }
        bump_version(conn, 72)?;
    }

    if version < 73 {
        let has_feature_id = conn
            .prepare("SELECT feature_id FROM workflow_runs LIMIT 0")
            .is_ok();
        if has_feature_id {
            conn.execute_batch("ALTER TABLE workflow_runs DROP COLUMN feature_id;")?;
        }
        conn.execute_batch(include_str!("migrations/073_drop_features.sql"))?;
        bump_version(conn, 73)?;
    }

    if version < 74 {
        conn.execute_batch(include_str!("migrations/074_drop_notifications.sql"))?;
        bump_version(conn, 74)?;
    }

    // Migration 075: add dismissed column to workflow_runs for soft-dismiss feature.
    if version < 75 {
        let has_col: bool = conn
            .prepare("SELECT dismissed FROM workflow_runs LIMIT 0")
            .is_ok();
        if !has_col {
            conn.execute_batch(include_str!("migrations/075_workflow_run_dismissed.sql"))?;
        }
        bump_version(conn, 75)?;
    }

    // Migration 076: add runtime column to agent_runs (RFC 007 AgentRuntime trait).
    if version < 76 {
        let table_exists: bool = conn.prepare("SELECT 1 FROM agent_runs LIMIT 0").is_ok();
        if table_exists {
            let has_col: bool = conn
                .prepare("SELECT runtime FROM agent_runs LIMIT 0")
                .is_ok();
            if !has_col {
                conn.execute_batch(include_str!("migrations/076_agent_runs_runtime.sql"))?;
            }
        }
        bump_version(conn, 76)?;
    }

    // Migration 077: add runtime_overrides column to repos (RFC 007 per-repo runtime config).
    if version < 77 {
        let table_exists: bool = conn.prepare("SELECT 1 FROM repos LIMIT 0").is_ok();
        if table_exists {
            let has_col: bool = conn
                .prepare("SELECT runtime_overrides FROM repos LIMIT 0")
                .is_ok();
            if !has_col {
                conn.execute_batch(include_str!("migrations/077_repos_runtime_overrides.sql"))?;
            }
        }
        bump_version(conn, 77)?;
    }

    // Migration 078: drop tmux_window column from agent_runs (CliRuntime no longer uses tmux).
    if version < 78 {
        let table_exists: bool = conn.prepare("SELECT 1 FROM agent_runs LIMIT 0").is_ok();
        if table_exists {
            let has_col: bool = conn
                .prepare("SELECT tmux_window FROM agent_runs LIMIT 0")
                .is_ok();
            if has_col {
                conn.execute_batch(include_str!("migrations/078_drop_tmux_window.sql"))?;
            }
        }
        bump_version(conn, 78)?;
    }

    // Migration 079: add 'cancelling' to the workflow_runs.status CHECK constraint.
    // Uses the table-swap pattern (same as 071). Guard skips when the table is absent
    // or already has 'cancelling' in the DDL (idempotent).
    if version < 79 {
        let workflow_runs_has_full_schema: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='workflow_runs' \
                 AND sql LIKE '%parent_run_id%REFERENCES agent_runs%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        let already_migrated: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='workflow_runs' AND sql LIKE '%cancelling%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if workflow_runs_has_full_schema && !already_migrated {
            with_foreign_keys_off(conn, || {
                conn.execute_batch(include_str!("migrations/079_workflow_run_cancelling.sql"))?;
                Ok(())
            })?;
        }
        bump_version(conn, 79)?;
    }

    Ok(())
}

/// Run all schema migrations in compatibility mode.
///
/// Identical to [`run`] except that a DB schema version *ahead* of this binary
/// (`version > LATEST_SCHEMA_VERSION`) emits a [`tracing::warn!`] and returns
/// `Ok(())` instead of a hard error.
///
/// # Safety
/// Compat mode is safe only for **additive** migrations (ADD COLUMN, CREATE
/// TABLE, ALTER TABLE ADD). If a future migration drops or renames a column
/// that this binary actively queries, compat mode will produce silent data
/// corruption rather than a clear error. This must be revisited before any
/// destructive migration is shipped.
pub fn run_compat(conn: &Connection) -> Result<()> {
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

    if version > LATEST_SCHEMA_VERSION as i64 {
        tracing::warn!(
            db_version = version,
            binary_version = LATEST_SCHEMA_VERSION,
            "DB schema is newer than this binary (db={}, binary={}); running in compatibility \
             mode. Safe only for additive migrations.",
            version,
            LATEST_SCHEMA_VERSION,
        );
        return Ok(());
    }

    // Delegate to run() for normal migration path.
    run(conn)
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
                created_at TEXT NOT NULL, model TEXT
            );
            CREATE TABLE worktrees (
                id TEXT PRIMARY KEY, repo_id TEXT NOT NULL,
                slug TEXT NOT NULL, branch TEXT NOT NULL, path TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL,
                base_branch TEXT NOT NULL DEFAULT 'main'
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
            -- workflow_run_steps at version 26: columns from migrations 020, 021, 023.
            -- Migration 037 does a table swap, so this minimal form is intentional.
            CREATE TABLE workflow_run_steps (
                id                TEXT PRIMARY KEY,
                workflow_run_id   TEXT NOT NULL,
                step_name         TEXT NOT NULL,
                role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate')),
                can_commit        INTEGER NOT NULL DEFAULT 0,
                condition_expr    TEXT,
                status            TEXT NOT NULL DEFAULT 'pending',
                child_run_id      TEXT,
                position          INTEGER NOT NULL DEFAULT 0,
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
            CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);
            INSERT INTO _conductor_meta VALUES ('schema_version', '26');
            INSERT INTO repos VALUES ('r1', 'test-repo', '/tmp/repo',
                'https://github.com/test/repo.git', 'main', '/tmp/ws', '2024-01-01T00:00:00Z', NULL);
            INSERT INTO worktrees VALUES ('w1', 'r1', 'feat-test', 'feat/test',
                '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z', 'main');
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
                crate::error::SubprocessFailure::from_message(
                    "test",
                    "simulated migration error".to_string(),
                ),
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

    #[test]
    fn test_migrate_repo_columns_to_config_writes_files() {
        use crate::config::RepoConfig;

        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        let conn = Connection::open_in_memory().unwrap();
        // Create a minimal repos table with the old columns still present.
        conn.execute_batch(
            "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                local_path TEXT NOT NULL,
                remote_url TEXT NOT NULL,
                default_branch TEXT NOT NULL DEFAULT 'main',
                workspace_dir TEXT NOT NULL,
                created_at TEXT NOT NULL,
                model TEXT
            );",
        )
        .unwrap();

        // Repo 1: custom default_branch and model — both should be migrated.
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at, model)
             VALUES ('r1', 'repo1', ?1, 'https://x/r1.git', 'develop', '/ws/repo1', '2025-01-01T00:00:00Z', 'opus')",
            rusqlite::params![dir1.path().to_str().unwrap()],
        )
        .unwrap();

        // Repo 2: default values — should be skipped (no config file created).
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at, model)
             VALUES ('r2', 'repo2', ?1, 'https://x/r2.git', 'main', '/ws/repo2', '2025-01-01T00:00:00Z', NULL)",
            rusqlite::params![dir2.path().to_str().unwrap()],
        )
        .unwrap();

        migrate_repo_columns_to_config(&conn);

        // Repo 1 should have a .conductor/config.toml with the migrated values.
        let rc1 = RepoConfig::load(dir1.path()).unwrap();
        assert_eq!(rc1.defaults.default_branch.as_deref(), Some("develop"));
        assert_eq!(rc1.defaults.model.as_deref(), Some("opus"));

        // Repo 2 should NOT have a config file (all defaults — nothing to migrate).
        assert!(
            !dir2.path().join(".conductor").join("config.toml").exists(),
            "repo with default values should not get a config file"
        );
    }

    #[test]
    fn test_migrate_repo_columns_to_config_no_columns() {
        // If columns are already gone, migration should be a no-op.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE repos (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                local_path TEXT NOT NULL,
                remote_url TEXT NOT NULL,
                workspace_dir TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )
        .unwrap();

        // Should not panic.
        migrate_repo_columns_to_config(&conn);
    }

    // -----------------------------------------------------------------------
    // Migration 047 tests
    // -----------------------------------------------------------------------

    /// Helper: create a minimal v46 schema with the given `workflow_runs` DDL.
    /// Returns a connection positioned at version 46, ready for migration 047.
    fn setup_v46_schema(conn: &Connection, workflow_runs_ddl: &str) {
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE repos (
                 id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE,
                 local_path TEXT NOT NULL, remote_url TEXT NOT NULL,
                 workspace_dir TEXT NOT NULL, created_at TEXT NOT NULL
             );
             CREATE TABLE worktrees (
                 id TEXT PRIMARY KEY, repo_id TEXT NOT NULL,
                 slug TEXT NOT NULL, branch TEXT NOT NULL, path TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL,
                 base_branch TEXT NOT NULL DEFAULT 'main'
             );
             CREATE TABLE tickets (
                 id TEXT PRIMARY KEY, repo_id TEXT NOT NULL,
                 source_type TEXT NOT NULL, source_id TEXT NOT NULL,
                 title TEXT NOT NULL, body TEXT, url TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'open', priority TEXT,
                 labels TEXT, assignee TEXT, created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE TABLE agent_runs (
                 id TEXT PRIMARY KEY, worktree_id TEXT,
                 claude_session_id TEXT, prompt TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'running', result_text TEXT,
                 cost_usd REAL, num_turns INTEGER, duration_ms INTEGER,
                 started_at TEXT NOT NULL, ended_at TEXT, tmux_window TEXT,
                 log_file TEXT, model TEXT, plan TEXT, parent_run_id TEXT
             );
             CREATE TABLE features (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute_batch(workflow_runs_ddl).unwrap();
        // workflow_run_steps as it exists at v46 (all columns up to migration 039).
        conn.execute_batch(
            "CREATE TABLE workflow_run_steps (
                 id                TEXT PRIMARY KEY,
                 workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                 step_name         TEXT NOT NULL,
                 role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow','script')),
                 can_commit        INTEGER NOT NULL DEFAULT 0,
                 condition_expr    TEXT,
                 status            TEXT NOT NULL DEFAULT 'pending'
                                   CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
                 child_run_id      TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
                 position          INTEGER NOT NULL,
                 started_at        TEXT, ended_at TEXT, result_text TEXT,
                 condition_met     INTEGER,
                 iteration         INTEGER NOT NULL DEFAULT 0,
                 parallel_group_id TEXT,
                 context_out       TEXT, markers_out TEXT,
                 retry_count       INTEGER NOT NULL DEFAULT 0,
                 gate_type TEXT, gate_prompt TEXT, gate_timeout TEXT,
                 gate_approved_by TEXT, gate_approved_at TEXT, gate_feedback TEXT,
                 structured_output TEXT, output_file TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run ON workflow_run_steps(workflow_run_id);",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO _conductor_meta VALUES ('schema_version', '46');
             INSERT INTO repos VALUES ('r1', 'test-repo', '/tmp/repo',
                 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z');
             INSERT INTO agent_runs (id, prompt, started_at)
                 VALUES ('ar1', 'workflow', '2024-01-01T00:00:00Z');",
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    }

    #[test]
    fn test_migration_047_table_swap_adds_hook_trigger() {
        // Path 1: table CHECK does not include 'hook' — full table swap required.
        let conn = Connection::open_in_memory().unwrap();
        let old_ddl = "CREATE TABLE workflow_runs (
            id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL,
            worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
            parent_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending','running','waiting','completed','failed','cancelled','timed_out')),
            dry_run INTEGER NOT NULL DEFAULT 0,
            trigger TEXT NOT NULL DEFAULT 'manual'
                CHECK (trigger IN ('manual','pr','scheduled')),
            started_at TEXT NOT NULL, ended_at TEXT, result_summary TEXT,
            definition_snapshot TEXT, inputs TEXT,
            ticket_id TEXT REFERENCES tickets(id),
            repo_id TEXT REFERENCES repos(id),
            parent_workflow_run_id TEXT,
            target_label TEXT, default_bot_name TEXT,
            iteration INTEGER NOT NULL DEFAULT 0, blocked_on TEXT,
            feature_id TEXT REFERENCES features(id)
        );
        CREATE INDEX idx_workflow_runs_ticket ON workflow_runs(ticket_id);
        CREATE INDEX idx_workflow_runs_repo ON workflow_runs(repo_id);
        CREATE INDEX idx_workflow_runs_parent_wf ON workflow_runs(parent_workflow_run_id);
        INSERT INTO workflow_runs (id, workflow_name, parent_run_id, status, trigger, started_at)
            VALUES ('wfr1', 'my-flow', 'ar1', 'completed', 'manual', '2024-01-01T00:00:00Z');";
        setup_v46_schema(&conn, old_ddl);

        run(&conn).unwrap();

        // 'hook' trigger must now be accepted.
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_name, parent_run_id, trigger, started_at)
             VALUES ('wfr2', 'hook-flow', 'ar1', 'hook', '2024-01-02T00:00:00Z')",
            [],
        )
        .expect("trigger='hook' should be accepted after migration 047");

        // Original row must survive.
        let name: String = conn
            .query_row(
                "SELECT workflow_name FROM workflow_runs WHERE id = 'wfr1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "my-flow");

        // Indexes must be recreated.
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('idx_workflow_runs_ticket','idx_workflow_runs_repo','idx_workflow_runs_parent_wf')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 3, "all three indexes must be recreated");
    }

    #[test]
    fn test_migration_047_drops_triggered_by_hook_column() {
        // Path 2: table already has 'hook' CHECK but still has old
        // `triggered_by_hook` column — table swap should drop it.
        let conn = Connection::open_in_memory().unwrap();
        let ddl_with_old_column = "CREATE TABLE workflow_runs (
            id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL,
            worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
            parent_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending','running','waiting','completed','failed','cancelled','timed_out')),
            dry_run INTEGER NOT NULL DEFAULT 0,
            trigger TEXT NOT NULL DEFAULT 'manual'
                CHECK (trigger IN ('manual','pr','scheduled','hook')),
            triggered_by_hook INTEGER NOT NULL DEFAULT 0,
            started_at TEXT NOT NULL, ended_at TEXT, result_summary TEXT,
            definition_snapshot TEXT, inputs TEXT,
            ticket_id TEXT REFERENCES tickets(id),
            repo_id TEXT REFERENCES repos(id),
            parent_workflow_run_id TEXT,
            target_label TEXT, default_bot_name TEXT,
            iteration INTEGER NOT NULL DEFAULT 0, blocked_on TEXT,
            feature_id TEXT REFERENCES features(id)
        );
        INSERT INTO workflow_runs (id, workflow_name, parent_run_id, trigger, started_at)
            VALUES ('wfr1', 'old-flow', 'ar1', 'hook', '2024-01-01T00:00:00Z');";
        setup_v46_schema(&conn, ddl_with_old_column);

        run(&conn).unwrap();

        // `triggered_by_hook` column must be gone.
        let schema: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='workflow_runs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !schema.contains("triggered_by_hook"),
            "triggered_by_hook column should be removed"
        );

        // Original row must survive (trigger value preserved).
        let trigger: String = conn
            .query_row(
                "SELECT trigger FROM workflow_runs WHERE id = 'wfr1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trigger, "hook");
    }

    #[test]
    fn test_migration_047_index_only_when_up_to_date() {
        // Path 3: table is already fully up-to-date (has 'hook' CHECK,
        // no `triggered_by_hook` column) — only indexes should be created.
        let conn = Connection::open_in_memory().unwrap();
        let up_to_date_ddl = "CREATE TABLE workflow_runs (
            id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL,
            worktree_id TEXT REFERENCES worktrees(id) ON DELETE CASCADE,
            parent_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending','running','waiting','completed','failed','cancelled','timed_out')),
            dry_run INTEGER NOT NULL DEFAULT 0,
            trigger TEXT NOT NULL DEFAULT 'manual'
                CHECK (trigger IN ('manual','pr','scheduled','hook')),
            started_at TEXT NOT NULL, ended_at TEXT, result_summary TEXT,
            definition_snapshot TEXT, inputs TEXT,
            ticket_id TEXT REFERENCES tickets(id),
            repo_id TEXT REFERENCES repos(id),
            parent_workflow_run_id TEXT,
            target_label TEXT, default_bot_name TEXT,
            iteration INTEGER NOT NULL DEFAULT 0, blocked_on TEXT,
            feature_id TEXT REFERENCES features(id)
        );
        INSERT INTO workflow_runs (id, workflow_name, parent_run_id, trigger, started_at)
            VALUES ('wfr1', 'ok-flow', 'ar1', 'hook', '2024-01-01T00:00:00Z');";
        setup_v46_schema(&conn, up_to_date_ddl);

        run(&conn).unwrap();

        // Indexes must exist.
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name IN ('idx_workflow_runs_ticket','idx_workflow_runs_repo','idx_workflow_runs_parent_wf')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 3, "indexes must be created in index-only path");

        // Data must be intact (no table swap).
        let name: String = conn
            .query_row(
                "SELECT workflow_name FROM workflow_runs WHERE id = 'wfr1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "ok-flow");
    }

    // -----------------------------------------------------------------------
    // Migration 058 tests
    // -----------------------------------------------------------------------

    /// Verifies that migration 058 preserves existing rows in `workflow_run_steps`
    /// and removes the FK constraint on `child_run_id` so that workflow-type steps
    /// can store a `workflow_runs.id` value (not just `agent_runs.id`).
    #[test]
    fn test_migration_058_preserves_rows_and_drops_fk() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        // Minimal pre-058 schema: only the tables migration 058 touches.
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY,
                 workflow_name TEXT NOT NULL,
                 worktree_id TEXT,
                 parent_run_id TEXT,
                 status TEXT NOT NULL DEFAULT 'pending',
                 dry_run INTEGER NOT NULL DEFAULT 0,
                 trigger TEXT NOT NULL DEFAULT 'manual',
                 started_at TEXT NOT NULL,
                 ended_at TEXT,
                 result_summary TEXT,
                 definition_snapshot TEXT,
                 inputs TEXT,
                 ticket_id TEXT,
                 repo_id TEXT,
                 parent_workflow_run_id TEXT,
                 target_label TEXT,
                 default_bot_name TEXT,
                 iteration INTEGER NOT NULL DEFAULT 0,
                 blocked_on TEXT,
                 feature_id TEXT,
                 feature_iteration INTEGER,
                 triggered_by TEXT,
                 run_id TEXT
             );
             CREATE TABLE agent_runs (
                 id TEXT PRIMARY KEY,
                 worktree_id TEXT,
                 prompt TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'running',
                 started_at TEXT NOT NULL
             );
             -- workflow_run_steps at v57: child_run_id has FK to agent_runs
             CREATE TABLE workflow_run_steps (
                 id                TEXT PRIMARY KEY,
                 workflow_run_id   TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                 step_name         TEXT NOT NULL,
                 role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow','script')),
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
                 structured_output TEXT,
                 output_file       TEXT,
                 gate_options      TEXT,
                 gate_selections   TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run
               ON workflow_run_steps(workflow_run_id);
             INSERT INTO _conductor_meta VALUES ('schema_version', '57');
             INSERT INTO agent_runs (id, prompt, started_at)
                 VALUES ('ar1', 'test', '2024-01-01T00:00:00Z');
             INSERT INTO workflow_runs (id, workflow_name, trigger, started_at)
                 VALUES ('wfr1', 'my-flow', 'manual', '2024-01-01T00:00:00Z'),
                        ('wfr2', 'child-flow', 'manual', '2024-01-01T00:00:00Z');
             INSERT INTO workflow_run_steps
                 (id, workflow_run_id, step_name, role, position, child_run_id)
                 VALUES ('s1', 'wfr1', 'workflow:child-flow', 'workflow', 0, 'ar1');",
        )
        .unwrap();

        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Apply migration 058.
        run(&conn).unwrap();

        // The original step row must survive with child_run_id intact.
        let child_id: Option<String> = conn
            .query_row(
                "SELECT child_run_id FROM workflow_run_steps WHERE id = 's1'",
                [],
                |row| row.get(0),
            )
            .expect("step row must survive migration 058");
        assert_eq!(child_id.as_deref(), Some("ar1"));

        // After migration, child_run_id must accept a workflow_runs.id value
        // (the FK to agent_runs has been dropped).
        conn.execute(
            "INSERT INTO workflow_run_steps
             (id, workflow_run_id, step_name, role, position, child_run_id)
             VALUES ('s2', 'wfr1', 'workflow:another', 'workflow', 1, 'wfr2')",
            [],
        )
        .expect("child_run_id must accept a workflow_runs id after migration 058");

        let wf_child_id: Option<String> = conn
            .query_row(
                "SELECT child_run_id FROM workflow_run_steps WHERE id = 's2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(wf_child_id.as_deref(), Some("wfr2"));
    }

    #[test]
    fn test_stale_binary_detection() {
        let conn = Connection::open_in_memory().unwrap();
        // Run all migrations normally first.
        run(&conn).unwrap();

        // Simulate a newer binary having migrated the DB further.
        let future_version = LATEST_SCHEMA_VERSION + 1;
        bump_version(&conn, future_version).unwrap();

        // Now run() should detect the stale binary and fail.
        let err = run(&conn).expect_err("should fail with stale binary error");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("Database schema version ({future_version})")),
            "error should mention the DB version, got: {msg}"
        );
        assert!(
            msg.contains(&format!("this binary supports ({LATEST_SCHEMA_VERSION})")),
            "error should mention the binary version, got: {msg}"
        );
        assert!(
            msg.contains("cargo build"),
            "error should suggest rebuilding, got: {msg}"
        );
    }

    /// Verifies that migration 056 adds `gate_options` and `gate_selections`
    /// columns to `workflow_run_steps` unconditionally.
    ///
    /// Builds a minimal schema positioned at version 55 (with `workflow_run_steps`
    /// as it exists at that point, without the new columns), then runs migration 056
    /// and asserts both columns appear.
    #[test]
    fn test_migration_056_adds_gate_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        // Minimal schema at v55: only the tables and columns that exist at that
        // version.  workflow_run_steps has the full v46 DDL (output_file included,
        // added by migration 037) but NOT gate_options/gate_selections.
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE repos (
                 id TEXT PRIMARY KEY, slug TEXT NOT NULL UNIQUE,
                 local_path TEXT NOT NULL, remote_url TEXT NOT NULL,
                 workspace_dir TEXT NOT NULL, created_at TEXT NOT NULL
             );
             CREATE TABLE worktrees (
                 id TEXT PRIMARY KEY, repo_id TEXT NOT NULL,
                 slug TEXT NOT NULL, branch TEXT NOT NULL, path TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL,
                 base_branch TEXT NOT NULL DEFAULT 'main'
             );
             CREATE TABLE agent_runs (
                 id TEXT PRIMARY KEY, worktree_id TEXT,
                 prompt TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running',
                 started_at TEXT NOT NULL
             );
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY, workflow_name TEXT NOT NULL,
                 worktree_id TEXT, parent_run_id TEXT NOT NULL DEFAULT '',
                 status TEXT NOT NULL DEFAULT 'pending',
                 dry_run INTEGER NOT NULL DEFAULT 0,
                 trigger TEXT NOT NULL DEFAULT 'manual',
                 started_at TEXT NOT NULL,
                 target_label TEXT
             );
             -- workflow_run_steps at v55: all columns through migration 039,
             -- but NOT gate_options/gate_selections (added by 056).
             -- Must match the SELECT list used by migration 058's table swap.
             CREATE TABLE workflow_run_steps (
                 id                TEXT PRIMARY KEY,
                 workflow_run_id   TEXT NOT NULL,
                 step_name         TEXT NOT NULL,
                 role              TEXT NOT NULL DEFAULT 'actor',
                 can_commit        INTEGER NOT NULL DEFAULT 0,
                 condition_expr    TEXT,
                 status            TEXT NOT NULL DEFAULT 'pending',
                 child_run_id      TEXT,
                 position          INTEGER NOT NULL DEFAULT 0,
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
                 structured_output TEXT,
                 output_file       TEXT
             );
             INSERT INTO _conductor_meta VALUES ('schema_version', '55');",
        )
        .unwrap();

        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Before migration 056: neither column should exist.
        let col_names: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(workflow_run_steps)")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(
            !col_names.contains(&"gate_options".to_string()),
            "gate_options must not exist before migration 056"
        );
        assert!(
            !col_names.contains(&"gate_selections".to_string()),
            "gate_selections must not exist before migration 056"
        );

        // Apply migration 056 (and anything beyond, though the DB has no other
        // tables needed by later migrations — they are no-ops for this test's
        // purpose because they touch different tables).
        run(&conn).unwrap();

        // After migration 056: both columns must exist.
        let col_names_after: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(workflow_run_steps)")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(
            col_names_after.contains(&"gate_options".to_string()),
            "gate_options must exist after migration 056"
        );
        assert!(
            col_names_after.contains(&"gate_selections".to_string()),
            "gate_selections must exist after migration 056"
        );
    }

    // -----------------------------------------------------------------------
    // Migration 062 tests
    // -----------------------------------------------------------------------

    /// Inserts a minimal repo (`r1`) and two tickets (`t1`, `t2`) used by migration 062
    /// fixture tests.
    fn insert_ticket_dependency_fixtures(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
             VALUES ('r1', 'repo1', '/tmp/repo', 'https://example.com/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z');
             INSERT INTO tickets (id, repo_id, source_type, source_id, title, url, synced_at)
             VALUES ('t1', 'r1', 'github', '1', 'Ticket 1', 'https://example.com/1', '2024-01-01T00:00:00Z');
             INSERT INTO tickets (id, repo_id, source_type, source_id, title, url, synced_at)
             VALUES ('t2', 'r1', 'github', '2', 'Ticket 2', 'https://example.com/2', '2024-01-01T00:00:00Z');",
        )
        .unwrap();
    }

    /// Verifies that `ticket_dependencies` exists on a fresh DB after all migrations.
    #[test]
    fn test_ticket_dependencies_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        run(&conn).unwrap();
        // A simple SELECT proves the table exists.
        conn.execute_batch("SELECT 1 FROM ticket_dependencies LIMIT 0")
            .expect("ticket_dependencies table must exist after migration 062");
    }

    /// Verifies that deleting a ticket cascades to its dependency rows.
    #[test]
    fn test_ticket_dependencies_on_delete_cascade() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run(&conn).unwrap();

        // Insert a minimal repo and two tickets.
        insert_ticket_dependency_fixtures(&conn);
        conn.execute_batch(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id) VALUES ('t1', 't2');",
        )
        .unwrap();

        // Confirm the row exists.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ticket_dependencies WHERE from_ticket_id = 't1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Delete the blocking ticket — the dependency row must cascade away.
        conn.execute("DELETE FROM tickets WHERE id = 't1'", [])
            .unwrap();

        let count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ticket_dependencies WHERE from_ticket_id = 't1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count_after, 0,
            "dependency row must be removed on ticket delete"
        );
    }

    /// Verifies that deleting the *target* ticket (to_ticket_id) also cascades.
    #[test]
    fn test_ticket_dependencies_on_delete_cascade_to_ticket() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run(&conn).unwrap();

        insert_ticket_dependency_fixtures(&conn);
        conn.execute_batch(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id) VALUES ('t1', 't2');",
        )
        .unwrap();

        // Delete the target ticket — the dependency row must cascade away.
        conn.execute("DELETE FROM tickets WHERE id = 't2'", [])
            .unwrap();

        let count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ticket_dependencies WHERE to_ticket_id = 't2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count_after, 0,
            "dependency row must be removed when target ticket is deleted"
        );
    }

    /// Verifies that `dep_type` defaults to `'blocks'` when omitted.
    #[test]
    fn test_ticket_dependencies_dep_type_default() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run(&conn).unwrap();

        insert_ticket_dependency_fixtures(&conn);
        conn.execute_batch(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id) VALUES ('t1', 't2');",
        )
        .unwrap();

        let dep_type: String = conn
            .query_row(
                "SELECT dep_type FROM ticket_dependencies WHERE from_ticket_id = 't1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dep_type, "blocks", "dep_type must default to 'blocks'");
    }

    /// Verifies that inserting an invalid `dep_type` value is rejected.
    #[test]
    fn test_ticket_dependencies_check_constraint() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run(&conn).unwrap();

        insert_ticket_dependency_fixtures(&conn);

        let result = conn.execute(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type)
             VALUES ('t1', 't2', 'invalid_type')",
            [],
        );
        assert!(
            result.is_err(),
            "CHECK constraint must reject invalid dep_type"
        );
    }

    #[test]
    fn test_ticket_dependencies_both_dep_types_for_same_pair() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run(&conn).unwrap();

        insert_ticket_dependency_fixtures(&conn);

        conn.execute_batch(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type)
             VALUES ('t1', 't2', 'blocks');
             INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type)
             VALUES ('t1', 't2', 'parent_of');",
        )
        .expect("both dep_types for the same ticket pair must be storable");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ticket_dependencies WHERE from_ticket_id = 't1' AND to_ticket_id = 't2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "both 'blocks' and 'parent_of' rows must coexist");
    }

    /// Verifies that migration 068 adds 'foreach' to the role CHECK constraint
    /// so that foreach.rs can insert rows with role='foreach', and invalid roles
    /// are still rejected.
    ///
    /// FK enforcement is left ON (the default for in-memory connections after
    /// `run()` enables it) so that `with_foreign_keys_off` in the production
    /// migration path is exercised end-to-end.
    #[test]
    fn test_migration_068_foreach_role_accepted() {
        let conn = Connection::open_in_memory().unwrap();
        // Enable FK enforcement so the with_foreign_keys_off wrapper in migration
        // 068 is exercised on the production code path (FK must be disabled to
        // DROP workflow_run_steps while workflow_run_step_fan_out_items references it).
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Minimal pre-068 schema: workflow_run_steps with the old constraint
        // (no 'foreach'), plus the fan_out_* and subprocess_pid columns from 065/067.
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY,
                 workflow_name TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending',
                 started_at TEXT NOT NULL
             );
             CREATE TABLE workflow_run_steps (
                 id                TEXT PRIMARY KEY,
                 workflow_run_id   TEXT NOT NULL,
                 step_name         TEXT NOT NULL,
                 role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow','script')),
                 can_commit        INTEGER NOT NULL DEFAULT 0,
                 condition_expr    TEXT,
                 status            TEXT NOT NULL DEFAULT 'pending'
                                   CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
                 child_run_id      TEXT,
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
                 structured_output TEXT,
                 output_file       TEXT,
                 gate_options      TEXT,
                 gate_selections   TEXT,
                 subprocess_pid    INTEGER,
                 fan_out_total     INTEGER,
                 fan_out_completed INTEGER DEFAULT 0,
                 fan_out_failed    INTEGER DEFAULT 0,
                 fan_out_skipped   INTEGER DEFAULT 0
             );
             CREATE TABLE workflow_run_step_fan_out_items (
                 id           TEXT PRIMARY KEY,
                 step_run_id  TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
                 item_type    TEXT NOT NULL CHECK (item_type IN ('ticket', 'repo', 'workflow_run')),
                 item_id      TEXT NOT NULL,
                 item_ref     TEXT NOT NULL,
                 child_run_id TEXT,
                 status       TEXT NOT NULL DEFAULT 'pending'
                              CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
                 dispatched_at TEXT,
                 completed_at  TEXT,
                 UNIQUE (step_run_id, item_type, item_id)
             );
             CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run
               ON workflow_run_steps(workflow_run_id);
             INSERT INTO _conductor_meta VALUES ('schema_version', '67');
             INSERT INTO workflow_runs (id, workflow_name, started_at)
                 VALUES ('wfr1', 'foreach-flow', '2024-01-01T00:00:00Z');
             INSERT INTO workflow_run_steps
                 (id, workflow_run_id, step_name, role, position)
                 VALUES ('s1', 'wfr1', 'label-ticket', 'actor', 0);",
        )
        .unwrap();

        // Apply migration 068.
        run(&conn).unwrap();

        // role='foreach' must now be accepted.
        conn.execute(
            "INSERT INTO workflow_run_steps
             (id, workflow_run_id, step_name, role, position)
             VALUES ('s2', 'wfr1', 'foreach-step', 'foreach', 1)",
            [],
        )
        .expect("role='foreach' must be accepted after migration 068");

        // Existing row must have survived.
        let role: String = conn
            .query_row(
                "SELECT role FROM workflow_run_steps WHERE id = 's1'",
                [],
                |row| row.get(0),
            )
            .expect("existing step row must survive migration 068");
        assert_eq!(role, "actor");

        // Invalid role must still be rejected.
        let err = conn.execute(
            "INSERT INTO workflow_run_steps
             (id, workflow_run_id, step_name, role, position)
             VALUES ('s3', 'wfr1', 'bad-step', 'invalid_role', 2)",
            [],
        );
        assert!(
            err.is_err(),
            "invalid role must still be rejected after migration 068"
        );
    }

    /// Verifies that the COALESCE guards in migration 068's INSERT…SELECT
    /// normalise existing NULL values in fan_out_completed/failed/skipped to 0.
    ///
    /// Before 068 those columns were nullable (INTEGER DEFAULT 0); an existing
    /// row could legally have NULL if it was written by an older code path or
    /// via direct SQL.  Migration 068 declares them NOT NULL DEFAULT 0, so the
    /// COALESCE guards must convert any NULLs before the rename, otherwise the
    /// NOT NULL constraint fires and the migration fails.
    #[test]
    fn test_migration_068_coalesce_guards_normalize_nulls() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Pre-068 schema: fan_out_completed/failed/skipped are nullable (no NOT NULL).
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY,
                 workflow_name TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending',
                 started_at TEXT NOT NULL
             );
             CREATE TABLE workflow_run_steps (
                 id                TEXT PRIMARY KEY,
                 workflow_run_id   TEXT NOT NULL,
                 step_name         TEXT NOT NULL,
                 role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow','script')),
                 can_commit        INTEGER NOT NULL DEFAULT 0,
                 condition_expr    TEXT,
                 status            TEXT NOT NULL DEFAULT 'pending'
                                   CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
                 child_run_id      TEXT,
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
                 structured_output TEXT,
                 output_file       TEXT,
                 gate_options      TEXT,
                 gate_selections   TEXT,
                 subprocess_pid    INTEGER,
                 fan_out_total     INTEGER,
                 fan_out_completed INTEGER,
                 fan_out_failed    INTEGER,
                 fan_out_skipped   INTEGER
             );
             CREATE TABLE workflow_run_step_fan_out_items (
                 id           TEXT PRIMARY KEY,
                 step_run_id  TEXT NOT NULL REFERENCES workflow_run_steps(id) ON DELETE CASCADE,
                 item_type    TEXT NOT NULL CHECK (item_type IN ('ticket', 'repo', 'workflow_run')),
                 item_id      TEXT NOT NULL,
                 item_ref     TEXT NOT NULL,
                 child_run_id TEXT,
                 status       TEXT NOT NULL DEFAULT 'pending'
                              CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
                 dispatched_at TEXT,
                 completed_at  TEXT,
                 UNIQUE (step_run_id, item_type, item_id)
             );
             CREATE INDEX IF NOT EXISTS idx_workflow_run_steps_run
               ON workflow_run_steps(workflow_run_id);
             INSERT INTO _conductor_meta VALUES ('schema_version', '67');
             INSERT INTO workflow_runs (id, workflow_name, started_at)
                 VALUES ('wfr1', 'foreach-flow', '2024-01-01T00:00:00Z');",
        )
        .unwrap();

        // Insert a row where fan_out_completed/failed/skipped are explicitly NULL —
        // the scenario the COALESCE guards exist to handle.
        conn.execute(
            "INSERT INTO workflow_run_steps
             (id, workflow_run_id, step_name, role, position,
              fan_out_total, fan_out_completed, fan_out_failed, fan_out_skipped)
             VALUES ('s1', 'wfr1', 'label-ticket', 'actor', 0, 5, NULL, NULL, NULL)",
            [],
        )
        .unwrap();

        // Confirm the NULLs are really there before migration.
        let (completed, failed, skipped): (Option<i64>, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT fan_out_completed, fan_out_failed, fan_out_skipped
                 FROM workflow_run_steps WHERE id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(
            completed.is_none() && failed.is_none() && skipped.is_none(),
            "pre-condition: fan_out columns must be NULL before migration 068"
        );

        // Apply migration 068 — must succeed despite the NULLs.
        run(&conn).unwrap();

        // After migration the NULLs must have been coalesced to 0.
        let (completed, failed, skipped): (i64, i64, i64) = conn
            .query_row(
                "SELECT fan_out_completed, fan_out_failed, fan_out_skipped
                 FROM workflow_run_steps WHERE id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            completed, 0,
            "fan_out_completed NULL must be coalesced to 0"
        );
        assert_eq!(failed, 0, "fan_out_failed NULL must be coalesced to 0");
        assert_eq!(skipped, 0, "fan_out_skipped NULL must be coalesced to 0");

        // fan_out_total (nullable by design) must pass through unchanged.
        let total: Option<i64> = conn
            .query_row(
                "SELECT fan_out_total FROM workflow_run_steps WHERE id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, Some(5), "fan_out_total must be preserved as-is");
    }

    /// Regression: partial-failure recovery for migration 067 must add ALL four
    /// fan_out columns, not only the first one it checks (`fan_out_total`).
    ///
    /// Simulates the case where a prior run's ALTER TABLE batch was interrupted
    /// after adding `fan_out_total` but before the remaining three columns.
    /// When `run()` is called again it must add the three missing columns so
    /// that migration 068 (which references all four) can succeed.
    #[test]
    fn test_migration_067_partial_failure_recovery_adds_all_missing_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        // Build the full schema at schema_version = 66 that already has
        // `workflow_run_step_fan_out_items` and `fan_out_total` present —
        // simulating a crash that created the table and added only the first
        // ALTER TABLE column before failing.
        //
        // The schema must include ALL columns referenced by migration 068's
        // INSERT…SELECT so that run() can complete the partial state and then
        // apply 068 without errors.
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY,
                 workflow_name TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending',
                 started_at TEXT NOT NULL
             );
             CREATE TABLE workflow_run_steps (
                 id                TEXT PRIMARY KEY,
                 workflow_run_id   TEXT NOT NULL,
                 step_name         TEXT NOT NULL,
                 role              TEXT NOT NULL CHECK (role IN ('actor','reviewer','gate','workflow','script')),
                 can_commit        INTEGER NOT NULL DEFAULT 0,
                 condition_expr    TEXT,
                 status            TEXT NOT NULL DEFAULT 'pending'
                                   CHECK (status IN ('pending','running','waiting','completed','failed','skipped','timed_out')),
                 child_run_id      TEXT,
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
                 structured_output TEXT,
                 output_file       TEXT,
                 gate_options      TEXT,
                 gate_selections   TEXT,
                 subprocess_pid    INTEGER,
                 fan_out_total     INTEGER
             );
             CREATE TABLE workflow_run_step_fan_out_items (
                 id           TEXT PRIMARY KEY,
                 step_run_id  TEXT NOT NULL,
                 item_type    TEXT NOT NULL,
                 item_id      TEXT NOT NULL,
                 item_ref     TEXT NOT NULL,
                 status       TEXT NOT NULL DEFAULT 'pending'
             );
             INSERT INTO _conductor_meta VALUES ('schema_version', '66');",
        )
        .unwrap();

        // Confirm the partial-failure state: table present, fan_out_total present,
        // but the other three columns absent.
        assert!(
            conn.prepare("SELECT fan_out_total FROM workflow_run_steps LIMIT 0")
                .is_ok(),
            "fan_out_total should already exist (partial failure state)"
        );
        assert!(
            conn.prepare("SELECT fan_out_completed FROM workflow_run_steps LIMIT 0")
                .is_err(),
            "fan_out_completed should NOT yet exist"
        );

        // run() must detect the partial state and add the three missing columns.
        run(&conn).unwrap();

        // All four fan_out_* columns must now exist.
        for col in &[
            "fan_out_total",
            "fan_out_completed",
            "fan_out_failed",
            "fan_out_skipped",
        ] {
            assert!(
                conn.prepare(&format!("SELECT {col} FROM workflow_run_steps LIMIT 0"))
                    .is_ok(),
                "column {col} must exist after recovery"
            );
        }

        // Migration 068's INSERT…SELECT (which references all four columns)
        // must also work — i.e., the schema is fully consistent.
        conn.execute(
            "INSERT INTO workflow_run_steps
             (id, workflow_run_id, step_name, role, position)
             VALUES ('s1', 'run1', 'step', 'foreach', 0)",
            [],
        )
        .expect("role='foreach' must be accepted after full 067+068 recovery");
    }

    /// `run_compat` must return `Ok(())` when the DB schema version is ahead of
    /// the binary (simulating a post-migration headless agent invocation).
    #[test]
    fn test_run_compat_tolerates_newer_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        // Set schema version to LATEST_SCHEMA_VERSION + 1 to simulate a newer DB.
        conn.execute(
            "INSERT INTO _conductor_meta (key, value) VALUES ('schema_version', ?1)",
            rusqlite::params![(LATEST_SCHEMA_VERSION + 1).to_string()],
        )
        .unwrap();
        let result = run_compat(&conn);
        assert!(
            result.is_ok(),
            "run_compat must tolerate a newer schema; got: {result:?}"
        );
    }

    /// `run` (strict) must return an error when the DB schema version is ahead
    /// of the binary.
    #[test]
    fn test_run_strict_rejects_newer_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO _conductor_meta (key, value) VALUES ('schema_version', ?1)",
            rusqlite::params![(LATEST_SCHEMA_VERSION + 1).to_string()],
        )
        .unwrap();
        let result = run(&conn);
        assert!(
            result.is_err(),
            "run must reject a schema newer than the binary"
        );
        assert!(
            matches!(result.unwrap_err(), ConductorError::Schema(_)),
            "error must be ConductorError::Schema"
        );
    }

    #[test]
    fn test_migration_073_drops_feature_tables_and_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        // Minimal pre-073 schema with feature_id column and feature tables.
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY,
                 workflow_name TEXT NOT NULL,
                 trigger TEXT NOT NULL DEFAULT 'manual',
                 started_at TEXT NOT NULL,
                 feature_id TEXT
             );
             CREATE TABLE features (
                 id TEXT PRIMARY KEY,
                 repo_id TEXT NOT NULL,
                 name TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'active',
                 created_at TEXT NOT NULL
             );
             CREATE TABLE feature_tickets (
                 id TEXT PRIMARY KEY,
                 feature_id TEXT NOT NULL REFERENCES features(id) ON DELETE CASCADE,
                 ticket_id TEXT NOT NULL
             );
             INSERT INTO _conductor_meta VALUES ('schema_version', '72');
             INSERT INTO workflow_runs (id, workflow_name, started_at, feature_id)
                 VALUES ('wfr1', 'my-flow', '2024-01-01T00:00:00Z', 'feat1'),
                        ('wfr2', 'other-flow', '2024-01-01T00:00:00Z', NULL);",
        )
        .unwrap();

        run(&conn).unwrap();

        // feature_id column must be gone from workflow_runs.
        let err = conn.prepare("SELECT feature_id FROM workflow_runs LIMIT 0");
        assert!(
            err.is_err(),
            "feature_id column should have been dropped from workflow_runs"
        );

        // features and feature_tickets tables must not exist.
        let features_gone: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='features'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(features_gone, 0, "features table should be dropped");

        let ft_gone: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='feature_tickets'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ft_gone, 0, "feature_tickets table should be dropped");

        // workflow_runs rows must survive with other columns intact.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM workflow_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "workflow_runs rows must survive migration 073");
    }

    #[test]
    fn test_migration_073_idempotent_without_feature_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        // Schema already at 72 but without the feature tables/column (already cleaned).
        conn.execute_batch(
            "CREATE TABLE _conductor_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE workflow_runs (
                 id TEXT PRIMARY KEY,
                 workflow_name TEXT NOT NULL,
                 trigger TEXT NOT NULL DEFAULT 'manual',
                 started_at TEXT NOT NULL
             );
             INSERT INTO _conductor_meta VALUES ('schema_version', '72');",
        )
        .unwrap();

        // Must not error when feature_id / feature tables are already absent.
        run(&conn).expect("migration 073 must be idempotent when tables are already absent");
    }
}
