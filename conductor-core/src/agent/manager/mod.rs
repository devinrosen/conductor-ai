pub(crate) mod aggregation;
pub(crate) mod events;
pub mod feedback;
pub(crate) mod lifecycle;
pub(crate) mod orphans;
pub(crate) mod plan_steps;
pub(crate) mod queries;

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
