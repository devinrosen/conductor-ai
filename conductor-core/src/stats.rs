use crate::error::Result;
use rusqlite::Connection;
use serde::Serialize;

#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct ThemeUnlockStats {
    pub repos_registered: i64,
    pub prs_merged: i64,
    pub workflow_streak: i64,
    pub max_workflow_steps: i64,
    pub max_parallel_agents: i64,
    pub usage_days: f64,
}

pub struct StatsManager<'a> {
    conn: &'a Connection,
}

impl<'a> StatsManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn theme_unlock_stats(&self) -> Result<ThemeUnlockStats> {
        let repos_registered: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))?;

        let prs_merged: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM worktrees WHERE status = 'merged'",
            [],
            |r| r.get(0),
        )?;

        let workflow_streak: i64 = self
            .conn
            .query_row(
                "WITH ordered AS (
                   SELECT status,
                          ROW_NUMBER() OVER (ORDER BY started_at) AS rn,
                          ROW_NUMBER() OVER (PARTITION BY status ORDER BY started_at) AS grp
                   FROM workflow_runs
                   WHERE parent_workflow_run_id IS NULL OR parent_workflow_run_id = ''
                 )
                 SELECT COALESCE(MAX(cnt), 0) FROM (
                   SELECT COUNT(*) AS cnt
                   FROM ordered
                   WHERE status = 'completed'
                   GROUP BY rn - grp
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let max_workflow_steps: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(cnt), 0) FROM (
                   SELECT COUNT(*) AS cnt FROM workflow_run_steps GROUP BY workflow_run_id
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let max_parallel_agents: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(cnt), 0) FROM (
                   SELECT COUNT(*) AS cnt
                   FROM agent_runs a1
                   WHERE EXISTS (
                     SELECT 1 FROM agent_runs a2
                     WHERE a2.id != a1.id
                       AND a2.started_at BETWEEN datetime(a1.started_at, '-60 seconds') AND datetime(a1.started_at, '+60 seconds')
                   )
                   GROUP BY strftime('%Y-%m-%d %H:%M', a1.started_at)
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let usage_days: f64 = self
            .conn
            .query_row(
                "SELECT COALESCE(
                   julianday('now') - julianday(MIN(earliest)),
                   0
                 ) FROM (
                   SELECT MIN(created_at) AS earliest FROM repos
                   UNION ALL
                   SELECT MIN(started_at) FROM agent_runs
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        Ok(ThemeUnlockStats {
            repos_registered,
            prs_merged,
            workflow_streak,
            max_workflow_steps,
            max_parallel_agents,
            usage_days,
        })
    }
}
