pub mod aggregation;
pub mod events;
pub mod feedback;
pub mod lifecycle;
pub mod orphans;
pub mod plan_steps;
pub mod queries;

use rusqlite::Connection;

pub struct AgentManager<'a> {
    pub(super) conn: &'a Connection,
}

impl<'a> AgentManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

#[cfg(test)]
pub(super) fn setup_db() -> Connection {
    let conn = crate::test_helpers::setup_db();
    conn.execute(
        "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
         VALUES ('w2', 'r1', 'fix-bug', 'fix/bug', '/tmp/ws/fix-bug', 'active', '2024-01-01T00:00:00Z')",
        [],
    ).unwrap();
    conn
}
