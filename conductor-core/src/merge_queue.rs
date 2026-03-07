use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::db::query_collect;
use crate::error::Result;

/// A single entry in the merge queue — represents a worktree whose changes
/// should be landed onto the target branch by the refinery agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueEntry {
    pub id: String,
    pub repo_id: String,
    pub worktree_id: String,
    pub run_id: Option<String>,
    pub target_branch: String,
    pub position: i64,
    /// One of: queued, processing, merged, failed.
    pub status: String,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Manages the per-repo merge queue that serializes parallel agent merges.
pub struct MergeQueueManager<'a> {
    conn: &'a Connection,
}

impl<'a> MergeQueueManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Add a worktree (and optional run) to the merge queue for a repo.
    /// Automatically assigns the next position.
    pub fn enqueue(
        &self,
        repo_id: &str,
        worktree_id: &str,
        run_id: Option<&str>,
        target_branch: Option<&str>,
    ) -> Result<MergeQueueEntry> {
        let id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();
        let branch = target_branch.unwrap_or("main");

        // Next position = max existing position for this repo + 1.
        let next_pos: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM merge_queue WHERE repo_id = ?1",
            params![repo_id],
            |row| row.get(0),
        )?;

        self.conn.execute(
            "INSERT INTO merge_queue (id, repo_id, worktree_id, run_id, target_branch, position, status, queued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7)",
            params![id, repo_id, worktree_id, run_id, branch, next_pos, now],
        )?;

        Ok(MergeQueueEntry {
            id,
            repo_id: repo_id.to_string(),
            worktree_id: worktree_id.to_string(),
            run_id: run_id.map(|s| s.to_string()),
            target_branch: branch.to_string(),
            position: next_pos,
            status: "queued".to_string(),
            queued_at: now,
            started_at: None,
            completed_at: None,
        })
    }

    /// List all merge queue entries for a repo, ordered by position.
    pub fn list_for_repo(&self, repo_id: &str) -> Result<Vec<MergeQueueEntry>> {
        query_collect(
            self.conn,
            "SELECT id, repo_id, worktree_id, run_id, target_branch, position, status,
                    queued_at, started_at, completed_at
             FROM merge_queue
             WHERE repo_id = ?1
             ORDER BY position ASC",
            params![repo_id],
            |row| {
                Ok(MergeQueueEntry {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    worktree_id: row.get(2)?,
                    run_id: row.get(3)?,
                    target_branch: row.get(4)?,
                    position: row.get(5)?,
                    status: row.get(6)?,
                    queued_at: row.get(7)?,
                    started_at: row.get(8)?,
                    completed_at: row.get(9)?,
                })
            },
        )
    }

    /// List only pending (queued/processing) entries for a repo, ordered by position.
    pub fn list_pending(&self, repo_id: &str) -> Result<Vec<MergeQueueEntry>> {
        query_collect(
            self.conn,
            "SELECT id, repo_id, worktree_id, run_id, target_branch, position, status,
                    queued_at, started_at, completed_at
             FROM merge_queue
             WHERE repo_id = ?1 AND status IN ('queued', 'processing')
             ORDER BY position ASC",
            params![repo_id],
            |row| {
                Ok(MergeQueueEntry {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    worktree_id: row.get(2)?,
                    run_id: row.get(3)?,
                    target_branch: row.get(4)?,
                    position: row.get(5)?,
                    status: row.get(6)?,
                    queued_at: row.get(7)?,
                    started_at: row.get(8)?,
                    completed_at: row.get(9)?,
                })
            },
        )
    }

    /// Get a single entry by ID.
    pub fn get(&self, entry_id: &str) -> Result<Option<MergeQueueEntry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, repo_id, worktree_id, run_id, target_branch, position, status,
                    queued_at, started_at, completed_at
             FROM merge_queue
             WHERE id = ?1",
        )?;
        let entry = stmt
            .query_row(params![entry_id], |row| {
                Ok(MergeQueueEntry {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    worktree_id: row.get(2)?,
                    run_id: row.get(3)?,
                    target_branch: row.get(4)?,
                    position: row.get(5)?,
                    status: row.get(6)?,
                    queued_at: row.get(7)?,
                    started_at: row.get(8)?,
                    completed_at: row.get(9)?,
                })
            })
            .optional()?;
        Ok(entry)
    }

    /// Pop the next queued entry for a repo and mark it as processing.
    /// Returns None if the queue is empty or an entry is already processing.
    pub fn pop_next(&self, repo_id: &str) -> Result<Option<MergeQueueEntry>> {
        // Don't pop if something is already being processed.
        let processing_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM merge_queue WHERE repo_id = ?1 AND status = 'processing'",
            params![repo_id],
            |row| row.get(0),
        )?;
        if processing_count > 0 {
            return Ok(None);
        }

        let now = Utc::now().to_rfc3339();
        let updated = self.conn.execute(
            "UPDATE merge_queue SET status = 'processing', started_at = ?1
             WHERE id = (
                 SELECT id FROM merge_queue
                 WHERE repo_id = ?2 AND status = 'queued'
                 ORDER BY position ASC
                 LIMIT 1
             )",
            params![now, repo_id],
        )?;
        if updated == 0 {
            return Ok(None);
        }

        // Return the entry we just updated.
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, repo_id, worktree_id, run_id, target_branch, position, status,
                    queued_at, started_at, completed_at
             FROM merge_queue
             WHERE repo_id = ?1 AND status = 'processing'
             ORDER BY position ASC
             LIMIT 1",
        )?;
        let entry = stmt
            .query_row(params![repo_id], |row| {
                Ok(MergeQueueEntry {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    worktree_id: row.get(2)?,
                    run_id: row.get(3)?,
                    target_branch: row.get(4)?,
                    position: row.get(5)?,
                    status: row.get(6)?,
                    queued_at: row.get(7)?,
                    started_at: row.get(8)?,
                    completed_at: row.get(9)?,
                })
            })
            .optional()?;
        Ok(entry)
    }

    /// Mark an entry as merged.
    pub fn mark_merged(&self, entry_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE merge_queue SET status = 'merged', completed_at = ?1 WHERE id = ?2",
            params![now, entry_id],
        )?;
        Ok(())
    }

    /// Mark an entry as failed.
    pub fn mark_failed(&self, entry_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE merge_queue SET status = 'failed', completed_at = ?1 WHERE id = ?2",
            params![now, entry_id],
        )?;
        Ok(())
    }

    /// Remove an entry from the queue (cancel before it's processed).
    pub fn remove(&self, entry_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM merge_queue WHERE id = ?1", params![entry_id])?;
        Ok(())
    }

    /// Get the count of entries by status for a repo.
    pub fn queue_stats(&self, repo_id: &str) -> Result<QueueStats> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT status, COUNT(*) FROM merge_queue WHERE repo_id = ?1 GROUP BY status",
        )?;
        let mut stats = QueueStats::default();
        let rows = stmt.query_map(params![repo_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            match status.as_str() {
                "queued" => stats.queued = count,
                "processing" => stats.processing = count,
                "merged" => stats.merged = count,
                "failed" => stats.failed = count,
                _ => {}
            }
        }
        Ok(stats)
    }
}

/// Summary counts for a repo's merge queue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueStats {
    pub queued: i64,
    pub processing: i64,
    pub merged: i64,
    pub failed: i64,
}

// ── Bring rusqlite::OptionalExtension into scope ─────────────────────
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_database;
    use tempfile::NamedTempFile;

    fn setup() -> (Connection, String) {
        let tmp = NamedTempFile::new().unwrap();
        let conn = open_database(tmp.path()).unwrap();

        // Insert a repo.
        let repo_id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at)
             VALUES (?1, 'test-repo', '/tmp/test', 'https://github.com/test/repo', 'main', '/tmp/ws', ?2)",
            params![repo_id, now],
        )
        .unwrap();

        (conn, repo_id)
    }

    fn insert_worktree(conn: &Connection, repo_id: &str, slug: &str) -> String {
        let wt_id = ulid::Ulid::new().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at)
             VALUES (?1, ?2, ?3, ?4, '/tmp/wt', 'active', ?5)",
            params![wt_id, repo_id, slug, format!("feat/{slug}"), now],
        )
        .unwrap();
        wt_id
    }

    #[test]
    fn test_enqueue_and_list() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");
        let wt2 = insert_worktree(&conn, &repo_id, "feature-b");

        let mgr = MergeQueueManager::new(&conn);

        let e1 = mgr.enqueue(&repo_id, &wt1, None, None).unwrap();
        assert_eq!(e1.position, 0);
        assert_eq!(e1.status, "queued");

        let e2 = mgr.enqueue(&repo_id, &wt2, None, None).unwrap();
        assert_eq!(e2.position, 1);

        let all = mgr.list_for_repo(&repo_id).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].worktree_id, wt1);
        assert_eq!(all[1].worktree_id, wt2);
    }

    #[test]
    fn test_pop_next_serializes() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");
        let wt2 = insert_worktree(&conn, &repo_id, "feature-b");

        let mgr = MergeQueueManager::new(&conn);
        mgr.enqueue(&repo_id, &wt1, None, None).unwrap();
        mgr.enqueue(&repo_id, &wt2, None, None).unwrap();

        // Pop first — should get wt1.
        let first = mgr.pop_next(&repo_id).unwrap().unwrap();
        assert_eq!(first.worktree_id, wt1);
        assert_eq!(first.status, "processing");
        assert!(first.started_at.is_some());

        // Pop again while first is processing — should get None (serialized).
        let blocked = mgr.pop_next(&repo_id).unwrap();
        assert!(blocked.is_none());

        // Mark first merged, then pop — should get wt2.
        mgr.mark_merged(&first.id).unwrap();
        let second = mgr.pop_next(&repo_id).unwrap().unwrap();
        assert_eq!(second.worktree_id, wt2);
    }

    #[test]
    fn test_mark_failed() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");

        let mgr = MergeQueueManager::new(&conn);
        let entry = mgr.enqueue(&repo_id, &wt1, None, None).unwrap();

        mgr.pop_next(&repo_id).unwrap();
        mgr.mark_failed(&entry.id).unwrap();

        let e = mgr.get(&entry.id).unwrap().unwrap();
        assert_eq!(e.status, "failed");
        assert!(e.completed_at.is_some());
    }

    #[test]
    fn test_remove() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");

        let mgr = MergeQueueManager::new(&conn);
        let entry = mgr.enqueue(&repo_id, &wt1, None, None).unwrap();

        mgr.remove(&entry.id).unwrap();
        let gone = mgr.get(&entry.id).unwrap();
        assert!(gone.is_none());
    }

    #[test]
    fn test_queue_stats() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");
        let wt2 = insert_worktree(&conn, &repo_id, "feature-b");
        let wt3 = insert_worktree(&conn, &repo_id, "feature-c");

        let mgr = MergeQueueManager::new(&conn);
        mgr.enqueue(&repo_id, &wt1, None, None).unwrap();
        let e2 = mgr.enqueue(&repo_id, &wt2, None, None).unwrap();
        mgr.enqueue(&repo_id, &wt3, None, None).unwrap();

        // Pop and merge one.
        let first = mgr.pop_next(&repo_id).unwrap().unwrap();
        mgr.mark_merged(&first.id).unwrap();

        // Fail another.
        // Need to pop_next again now that first is merged.
        let second = mgr.pop_next(&repo_id).unwrap().unwrap();
        assert_eq!(second.id, e2.id);
        mgr.mark_failed(&second.id).unwrap();

        let stats = mgr.queue_stats(&repo_id).unwrap();
        assert_eq!(stats.queued, 1);
        assert_eq!(stats.processing, 0);
        assert_eq!(stats.merged, 1);
        assert_eq!(stats.failed, 1);
    }

    #[test]
    fn test_list_pending() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");
        let wt2 = insert_worktree(&conn, &repo_id, "feature-b");

        let mgr = MergeQueueManager::new(&conn);
        let e1 = mgr.enqueue(&repo_id, &wt1, None, None).unwrap();
        mgr.enqueue(&repo_id, &wt2, None, None).unwrap();

        // Mark first merged.
        mgr.pop_next(&repo_id).unwrap();
        mgr.mark_merged(&e1.id).unwrap();

        // Only wt2 should be pending.
        let pending = mgr.list_pending(&repo_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].worktree_id, wt2);
    }

    #[test]
    fn test_custom_target_branch() {
        let (conn, repo_id) = setup();
        let wt1 = insert_worktree(&conn, &repo_id, "feature-a");

        let mgr = MergeQueueManager::new(&conn);
        let entry = mgr.enqueue(&repo_id, &wt1, None, Some("develop")).unwrap();
        assert_eq!(entry.target_branch, "develop");
    }
}
