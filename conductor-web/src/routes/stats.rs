use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ThemeUnlockStats {
    pub repos_registered: i64,
    pub prs_merged: i64,
    pub workflow_streak: i64,
    pub max_workflow_steps: i64,
    pub max_parallel_agents: i64,
    pub usage_days: f64,
}

/// GET /api/stats/theme-unlocks
///
/// Returns aggregated stats used to evaluate theme unlock conditions.
#[utoipa::path(
    get,
    path = "/api/stats/theme-unlocks",
    responses(
        (status = 200, description = "Theme unlock stats", body = ThemeUnlockStats),
    ),
    tag = "stats",
)]
pub async fn theme_unlock_stats(
    State(state): State<AppState>,
) -> Result<Json<ThemeUnlockStats>, ApiError> {
    let db = state.db.lock().await;

    // 1. repos_registered — simple count
    let repos_registered: i64 = db.query_row("SELECT COUNT(*) FROM repos", [], |r| r.get(0))?;

    // 2. prs_merged — count merged worktrees
    let prs_merged: i64 = db.query_row(
        "SELECT COUNT(*) FROM worktrees WHERE status = 'merged'",
        [],
        |r| r.get(0),
    )?;

    // 3. workflow_streak — longest consecutive run of completed root workflows (no parent)
    let workflow_streak: i64 = db
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

    // 4. max_workflow_steps — largest step count in any single workflow run
    let max_workflow_steps: i64 = db
        .query_row(
            "SELECT COALESCE(MAX(cnt), 0) FROM (
               SELECT COUNT(*) AS cnt FROM workflow_run_steps GROUP BY workflow_run_id
             )",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // 5. max_parallel_agents — max concurrent agent runs (overlapping time windows)
    // Simplified: count agent runs that started within the same 60-second window
    let max_parallel_agents: i64 = db
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

    // 6. usage_days — days since earliest record (repos or agent runs)
    let usage_days: f64 = db
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

    Ok(Json(ThemeUnlockStats {
        repos_registered,
        prs_merged,
        workflow_streak,
        max_workflow_steps,
        max_parallel_agents,
        usage_days,
    }))
}
