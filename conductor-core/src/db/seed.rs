//! Populate a database with realistic fixture data for development.
//!
//! All seed rows use deterministic IDs prefixed with `seed-` so that:
//! 1. They are easily identifiable as test data.
//! 2. Re-running `seed_database` is idempotent (`INSERT OR IGNORE`).

use rusqlite::{params, Connection};

use crate::error::Result;

/// Insert realistic fixture data into the database.
///
/// Safe to call multiple times — uses `INSERT OR IGNORE` so duplicates are
/// skipped rather than causing errors.
pub fn seed_database(conn: &Connection) -> Result<()> {
    seed_repos(conn)?;
    seed_tickets(conn)?;
    seed_worktrees(conn)?;
    seed_agent_runs(conn)?;
    seed_workflow_runs(conn)?;
    seed_features(conn)?;
    Ok(())
}

fn seed_repos(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "seed-repo-001",
            "acme-app",
            "/tmp/conductor-seed/acme-app",
            "https://github.com/acme/acme-app.git",
            "/tmp/conductor-seed/workspaces/acme-app",
            "2025-01-15T10:00:00+00:00",
        ],
    )?;
    Ok(())
}

fn seed_tickets(conn: &Connection) -> Result<()> {
    let tickets = [
        (
            "seed-ticket-001",
            "seed-repo-001",
            "github",
            "42",
            "Fix login timeout on slow connections",
            "Users on mobile networks experience a timeout during OAuth flow.",
            "open",
            "[\"bug\", \"auth\"]",
        ),
        (
            "seed-ticket-002",
            "seed-repo-001",
            "github",
            "43",
            "Add dark mode support",
            "Implement a dark mode toggle in the settings page.",
            "in_progress",
            "[\"enhancement\", \"ui\"]",
        ),
        (
            "seed-ticket-003",
            "seed-repo-001",
            "github",
            "44",
            "Upgrade to React 19",
            "Migrate the frontend to React 19 and update deprecated APIs.",
            "open",
            "[\"chore\", \"frontend\"]",
        ),
        (
            "seed-ticket-004",
            "seed-repo-001",
            "github",
            "45",
            "Refactor payment module",
            "Break the monolithic payment module into smaller services.",
            "closed",
            "[\"refactor\"]",
        ),
        (
            "seed-ticket-005",
            "seed-repo-001",
            "github",
            "46",
            "Add CSV export for reports",
            "Allow users to download reports as CSV files.",
            "open",
            "[\"feature\"]",
        ),
    ];

    for (id, repo_id, source_type, source_id, title, body, state, labels) in &tickets {
        conn.execute(
            "INSERT OR IGNORE INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, repo_id, source_type, source_id, title, body, state, labels, "2025-01-20T12:00:00+00:00"],
        )?;
    }
    Ok(())
}

fn seed_worktrees(conn: &Connection) -> Result<()> {
    let worktrees = [
        (
            "seed-wt-001",
            "seed-repo-001",
            "fix-42-login-timeout",
            "fix/42-login-timeout",
            "/tmp/conductor-seed/workspaces/acme-app/fix-42-login-timeout",
            Some("seed-ticket-001"),
            "active",
            "2025-01-20T14:00:00+00:00",
            None::<&str>,
            Some("main"),
        ),
        (
            "seed-wt-002",
            "seed-repo-001",
            "feat-43-dark-mode",
            "feat/43-dark-mode",
            "/tmp/conductor-seed/workspaces/acme-app/feat-43-dark-mode",
            Some("seed-ticket-002"),
            "active",
            "2025-01-21T09:00:00+00:00",
            None::<&str>,
            Some("main"),
        ),
        (
            "seed-wt-003",
            "seed-repo-001",
            "fix-45-payment-refactor",
            "fix/45-payment-refactor",
            "/tmp/conductor-seed/workspaces/acme-app/fix-45-payment-refactor",
            Some("seed-ticket-004"),
            "merged",
            "2025-01-18T08:00:00+00:00",
            Some("2025-01-19T16:00:00+00:00"),
            Some("main"),
        ),
    ];

    for (
        id,
        repo_id,
        slug,
        branch,
        path,
        ticket_id,
        status,
        created_at,
        completed_at,
        base_branch,
    ) in &worktrees
    {
        conn.execute(
            "INSERT OR IGNORE INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at, base_branch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, repo_id, slug, branch, path, ticket_id, status, created_at, completed_at, base_branch],
        )?;
    }
    Ok(())
}

fn seed_agent_runs(conn: &Connection) -> Result<()> {
    let runs = [
        (
            "seed-run-001",
            "seed-wt-001",
            "Fix the OAuth timeout by increasing the deadline to 30s",
            "completed",
            Some("Increased OAuth timeout from 10s to 30s and added retry logic."),
            Some(0.12),
            Some(15),
            Some(45_000),
            "2025-01-20T14:30:00+00:00",
            Some("2025-01-20T15:15:00+00:00"),
            Some(12_500i64),
            Some(3_200i64),
        ),
        (
            "seed-run-002",
            "seed-wt-002",
            "Implement dark mode toggle in settings",
            "running",
            None::<&str>,
            None::<f64>,
            None::<i32>,
            None::<i64>,
            "2025-01-21T09:30:00+00:00",
            None::<&str>,
            None::<i64>,
            None::<i64>,
        ),
        (
            "seed-run-003",
            "seed-wt-003",
            "Refactor payment module into separate services",
            "failed",
            Some("Build failed: missing dependency after splitting modules."),
            Some(0.08),
            Some(22),
            Some(120_000),
            "2025-01-18T10:00:00+00:00",
            Some("2025-01-18T12:00:00+00:00"),
            Some(18_000i64),
            Some(5_100i64),
        ),
    ];

    for (
        id,
        wt_id,
        prompt,
        status,
        result,
        cost,
        turns,
        dur,
        started,
        ended,
        input_tok,
        output_tok,
    ) in &runs
    {
        conn.execute(
            "INSERT OR IGNORE INTO agent_runs
             (id, worktree_id, prompt, status, result_text, cost_usd, num_turns, duration_ms, started_at, ended_at, input_tokens, output_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![id, wt_id, prompt, status, result, cost, turns, dur, started, ended, input_tok, output_tok],
        )?;
    }
    Ok(())
}

fn seed_workflow_runs(conn: &Connection) -> Result<()> {
    // We need a parent agent run for workflow runs. Use seed-run-001.
    conn.execute(
        "INSERT OR IGNORE INTO workflow_runs
         (id, workflow_name, worktree_id, parent_run_id, status, started_at, ended_at, result_summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            "seed-wfrun-001",
            "ci-fix-loop",
            "seed-wt-001",
            "seed-run-001",
            "completed",
            "2025-01-20T15:00:00+00:00",
            "2025-01-20T15:30:00+00:00",
            "All CI checks passed after 2 iterations.",
        ],
    )?;

    // Workflow steps
    let steps = [
        (
            "seed-wfstep-001",
            "seed-wfrun-001",
            "run-tests",
            "actor",
            "completed",
            0, // position
            0, // iteration
        ),
        (
            "seed-wfstep-002",
            "seed-wfrun-001",
            "review-code",
            "reviewer",
            "completed",
            1,
            0,
        ),
        (
            "seed-wfstep-003",
            "seed-wfrun-001",
            "approve-merge",
            "gate",
            "completed",
            2,
            0,
        ),
    ];

    for (id, run_id, name, role, status, position, iteration) in &steps {
        conn.execute(
            "INSERT OR IGNORE INTO workflow_run_steps
             (id, workflow_run_id, step_name, role, status, position, iteration)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, run_id, name, role, status, position, iteration],
        )?;
    }

    Ok(())
}

fn seed_features(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO features (id, repo_id, name, branch, base_branch, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "seed-feature-001",
            "seed-repo-001",
            "auth-overhaul",
            "feat/auth-overhaul",
            "main",
            "active",
            "2025-01-15T12:00:00+00:00",
        ],
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO features (id, repo_id, name, branch, base_branch, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "seed-feature-002",
            "seed-repo-001",
            "payments-v2",
            "feat/payments-v2",
            "main",
            "closed",
            "2025-01-10T08:00:00+00:00",
        ],
    )?;

    // Link a ticket to the auth-overhaul feature
    conn.execute(
        "INSERT OR IGNORE INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        params!["seed-feature-001", "seed-ticket-001"],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_database;
    use tempfile::NamedTempFile;

    #[test]
    fn test_seed_populates_tables() {
        let tmp = NamedTempFile::new().unwrap();
        let conn = open_database(tmp.path()).unwrap();
        seed_database(&conn).unwrap();

        let repo_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))
            .unwrap();
        assert!(repo_count >= 1, "expected at least 1 repo");

        let ticket_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tickets", [], |r| r.get(0))
            .unwrap();
        assert!(ticket_count >= 3, "expected at least 3 tickets");

        let wt_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM worktrees", [], |r| r.get(0))
            .unwrap();
        assert!(wt_count >= 2, "expected at least 2 worktrees");

        let run_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r.get(0))
            .unwrap();
        assert!(run_count >= 2, "expected at least 2 agent runs");
    }

    #[test]
    fn test_seed_is_idempotent() {
        let tmp = NamedTempFile::new().unwrap();
        let conn = open_database(tmp.path()).unwrap();
        seed_database(&conn).unwrap();
        // Second call should not error
        seed_database(&conn).unwrap();

        let repo_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))
            .unwrap();
        assert_eq!(repo_count, 1, "expected exactly 1 repo after double seed");
    }
}
