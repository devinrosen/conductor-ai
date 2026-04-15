use std::process::Command;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::config::Config;
use crate::db::{query_collect, with_in_clause};
use crate::error::{ConductorError, Result, SubprocessFailure};
use crate::git::{check_output, git_in};
use crate::repo::RepoManager;
use crate::tickets::TicketSyncer;
use crate::worktree::WorktreeManager;

use super::helpers::{
    batch_branch_timestamps, derive_branch_name, last_commit_timestamp, map_feature_row,
};
use super::types::{
    Feature, FeatureRow, FeatureStatus, RunSummary, SyncResult, UnregisteredBranch,
};

/// Build a milestone `source_id` from its components.
///
/// Produces the canonical format `github.com/{owner}/{repo}/milestones/{number}`
/// consumed by [`parse_milestone_source_id`] and stored in `features.source_id`.
pub fn build_milestone_source_id(owner: &str, repo: &str, number: u64) -> String {
    format!("github.com/{}/{}/milestones/{}", owner, repo, number)
}

/// Parse a milestone `source_id` in the format
/// `github.com/{owner}/{repo}/milestones/{number}` into its components.
///
/// Returns `(owner, repo, milestone_number)` or `ConductorError::InvalidInput`
/// for any malformed input (including the old bare-number format).
pub(crate) fn parse_milestone_source_id(source_id: &str) -> Result<(String, String, u64)> {
    // Expected: ["github.com", owner, repo, "milestones", number]
    let parts: Vec<&str> = source_id.splitn(5, '/').collect();
    if parts.len() != 5
        || parts[0] != "github.com"
        || parts[3] != "milestones"
        || parts[1].is_empty()
        || parts[2].is_empty()
    {
        return Err(ConductorError::InvalidInput(format!(
            "Invalid milestone source_id '{}'. Expected \
             'github.com/{{owner}}/{{repo}}/milestones/{{number}}'",
            source_id
        )));
    }
    let number: u64 = parts[4].parse().map_err(|_| {
        ConductorError::InvalidInput(format!(
            "Invalid milestone number '{}' in source_id '{}'",
            parts[4], source_id
        ))
    })?;
    Ok((parts[1].to_string(), parts[2].to_string(), number))
}

fn feature_not_found(id: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> ConductorError {
    let id = id.into();
    move |e| match e {
        rusqlite::Error::QueryReturnedNoRows => ConductorError::FeatureNotFound { name: id },
        _ => ConductorError::Database(e),
    }
}

// ---------------------------------------------------------------------------
// Shared SQL fragments & row mapper for FeatureRow queries
// ---------------------------------------------------------------------------

/// SQL fragment: column list through `FROM features f` (no leading `SELECT`,
/// no `WHERE`/`ORDER`). When used in `list_all_active`, prefix with
/// `f.repo_id, ` so the repo_id appears at column 0 and FeatureRow columns
/// start at offset 1.
const FEATURE_ROW_FRAGMENT: &str = "\
    f.id, f.name, f.branch, f.base_branch, f.status, f.created_at, \
    (SELECT COUNT(*) FROM worktrees w WHERE w.repo_id = f.repo_id AND w.base_branch = f.branch AND w.status = 'active') AS wt_count, \
    (SELECT COUNT(*) FROM feature_tickets ft WHERE ft.feature_id = f.id) AS ticket_count, \
    f.last_commit_at, \
    (SELECT MAX(w2.created_at) FROM worktrees w2 WHERE w2.repo_id = f.repo_id AND w2.base_branch = f.branch AND w2.status = 'active') AS last_wt_activity \
    FROM features f";

const FEATURE_ROW_ORDER: &str = " ORDER BY f.created_at DESC";

/// Column list for a plain `SELECT … FROM features` (no join, no subquery).
/// Used by `map_feature_row` — keep in sync with that function's column indices.
const FEATURE_COLS: &str =
    "id, repo_id, name, branch, base_branch, status, created_at, merged_at, source_type, source_id, tickets_total, tickets_merged";

/// Same columns but table-aliased (`f.`) for use in joins.
const FEATURE_COLS_ALIASED: &str =
    "f.id, f.repo_id, f.name, f.branch, f.base_branch, f.status, f.created_at, f.merged_at, f.source_type, f.source_id, f.tickets_total, f.tickets_merged";

/// Number of columns selected by `FEATURE_COLS` / `FEATURE_COLS_ALIASED`.
/// Used as a named offset when appending extra columns in a JOIN query.
const FEATURE_COLS_COUNT: usize = 12;

/// Map a rusqlite row to a `FeatureRow`, starting at the given column offset.
fn map_feature_row_cols(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> std::result::Result<FeatureRow, rusqlite::Error> {
    Ok(FeatureRow {
        id: row.get(offset)?,
        name: row.get(offset + 1)?,
        branch: row.get(offset + 2)?,
        base_branch: row.get(offset + 3)?,
        status: row.get(offset + 4)?,
        created_at: row.get(offset + 5)?,
        worktree_count: row.get(offset + 6)?,
        ticket_count: row.get(offset + 7)?,
        last_commit_at: row.get(offset + 8)?,
        last_worktree_activity: row.get(offset + 9)?,
    })
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

pub struct FeatureManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl<'a> FeatureManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    /// Create a feature: insert DB record, create git branch, push to origin,
    /// and optionally link tickets.
    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        from_branch: Option<&str>,
        source_type: Option<&str>,
        source_id: Option<&str>,
        ticket_source_ids: &[String],
    ) -> Result<Feature> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        // Check for duplicate
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM features WHERE repo_id = ?1 AND name = ?2)",
            params![repo.id, name],
            |row| row.get(0),
        )?;
        if exists {
            return Err(ConductorError::FeatureAlreadyExists {
                name: name.to_string(),
            });
        }

        // Resolve ticket source_ids to internal ULID IDs before doing anything else
        let ticket_ids = if !ticket_source_ids.is_empty() {
            self.resolve_ticket_ids(&repo.id, ticket_source_ids)?
        } else {
            Vec::new()
        };

        let branch = derive_branch_name(name);

        let base = from_branch
            .map(|b| b.to_string())
            .unwrap_or_else(|| repo.default_branch.clone());

        // Create git branch and push — clean up local branch on push failure
        check_output(git_in(&repo.local_path).args([
            "branch",
            "--",
            &branch,
            &format!("refs/heads/{base}"),
        ]))?;
        if let Err(e) =
            check_output(git_in(&repo.local_path).args(["push", "-u", "origin", "--", &branch]))
        {
            // Best-effort cleanup of the local branch so the command is retriable
            let _ = git_in(&repo.local_path)
                .args(["branch", "-D", "--", &branch])
                .output();
            return Err(e);
        }

        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let feature = Feature {
            id: id.clone(),
            repo_id: repo.id.clone(),
            name: name.to_string(),
            branch,
            base_branch: base,
            status: FeatureStatus::InProgress,
            created_at: now,
            merged_at: None,
            source_type: source_type.map(|s| s.to_string()),
            source_id: source_id.map(|s| s.to_string()),
            tickets_total: 0,
            tickets_merged: 0,
        };

        if let Err(e) = self.insert_feature_record(&feature) {
            // Best-effort cleanup of branches created above so the command is retriable
            let _ = git_in(&repo.local_path)
                .args(["push", "origin", "--delete", "--", &feature.branch])
                .output();
            let _ = git_in(&repo.local_path)
                .args(["branch", "-D", "--", &feature.branch])
                .output();
            return Err(e);
        }

        // Link tickets if provided (already resolved to internal IDs)
        if !ticket_ids.is_empty() {
            self.link_tickets_internal(&feature.id, &ticket_ids)?;
        }

        Ok(feature)
    }

    /// List features for a repo with worktree and ticket counts.
    pub fn list(&self, repo_slug: &str) -> Result<Vec<FeatureRow>> {
        self.list_with_status_filter(repo_slug, None)
    }

    /// List only active features for a repo (with worktree and ticket counts).
    pub fn list_active(&self, repo_slug: &str) -> Result<Vec<FeatureRow>> {
        self.list_with_status_filter(repo_slug, Some(FeatureStatus::InProgress))
    }

    /// List active features for all repos in a single query, keyed by repo_id.
    pub fn list_all_active(&self) -> Result<std::collections::HashMap<String, Vec<FeatureRow>>> {
        let sql = format!(
            "SELECT f.repo_id, {FEATURE_ROW_FRAGMENT} WHERE f.status = ?1{FEATURE_ROW_ORDER}"
        );

        let pairs: Vec<(String, FeatureRow)> = query_collect(
            self.conn,
            &sql,
            params![FeatureStatus::InProgress],
            |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, map_feature_row_cols(row, 1)?)),
        )?;

        let mut map = std::collections::HashMap::new();
        for (repo_id, row) in pairs {
            map.entry(repo_id).or_insert_with(Vec::new).push(row);
        }
        Ok(map)
    }

    /// Shared helper: list features with an optional status filter.
    ///
    /// Worktree count uses an implicit join via branch name
    /// (w.base_branch = f.branch) rather than an FK join table. This is
    /// intentional: worktrees are created independently of features and
    /// linked by which branch they're based on, while ticket-feature links
    /// are explicit user actions stored in the `feature_tickets` table.
    fn list_with_status_filter(
        &self,
        repo_slug: &str,
        status: Option<FeatureStatus>,
    ) -> Result<Vec<FeatureRow>> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        let row_mapper = |row: &rusqlite::Row<'_>| map_feature_row_cols(row, 0);

        match status {
            Some(s) => {
                let sql = format!(
                    "SELECT {FEATURE_ROW_FRAGMENT} WHERE f.repo_id = ?1 AND f.status = ?2{FEATURE_ROW_ORDER}"
                );
                query_collect(self.conn, &sql, params![repo.id, s], row_mapper)
            }
            None => {
                let sql = format!(
                    "SELECT {FEATURE_ROW_FRAGMENT} WHERE f.repo_id = ?1{FEATURE_ROW_ORDER}"
                );
                query_collect(self.conn, &sql, params![repo.id], row_mapper)
            }
        }
    }

    /// Look up a single feature by repo slug + feature name.
    pub fn get_by_name(&self, repo_slug: &str, name: &str) -> Result<Feature> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        self.get_feature_by_repo_id(&repo.id, name)
    }

    /// Link tickets (by source_id) to a feature.
    pub fn link_tickets(
        &self,
        repo_slug: &str,
        feature_name: &str,
        ticket_source_ids: &[String],
    ) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;
        let ticket_ids = self.resolve_ticket_ids(&repo.id, ticket_source_ids)?;
        self.link_tickets_internal(&feature.id, &ticket_ids)
    }

    /// Look up a single feature by its internal ULID ID.
    pub fn get_by_id(&self, id: &str) -> Result<Feature> {
        self.conn
            .query_row(
                &format!("SELECT {FEATURE_COLS} FROM features WHERE id = ?1"),
                params![id],
                map_feature_row,
            )
            .map_err(feature_not_found(id))
    }

    /// Look up a feature by repo slug + name and verify it is active.
    ///
    /// Returns `ConductorError::Workflow` if the feature exists but is not active.
    pub fn resolve_active_feature(&self, repo_slug: &str, name: &str) -> Result<Feature> {
        let f = self.get_by_name(repo_slug, name)?;
        if f.status != FeatureStatus::InProgress {
            return Err(ConductorError::Workflow(format!(
                "Feature '{}' is {} — only in-progress features can be used.",
                name, f.status
            )));
        }
        Ok(f)
    }

    /// Find the active feature linked to a ticket, if any.
    ///
    /// Returns `None` when the ticket is not linked to any feature or when all
    /// linked features are closed/merged. Returns an error if the ticket is linked
    /// to multiple *active* features (ambiguous).
    pub fn find_feature_for_ticket(&self, ticket_id: &str) -> Result<Option<Feature>> {
        let features: Vec<Feature> = query_collect(
            self.conn,
            &format!(
                "SELECT {FEATURE_COLS_ALIASED} FROM features f \
                 INNER JOIN feature_tickets ft ON ft.feature_id = f.id \
                 WHERE ft.ticket_id = ?1 AND f.status = 'in_progress'"
            ),
            params![ticket_id],
            map_feature_row,
        )?;
        match features.len() {
            0 => Ok(None),
            1 => Ok(Some(features.into_iter().next().unwrap())),
            n => Err(ConductorError::Workflow(format!(
                "Ticket is linked to {n} active features ({}) — specify which feature to use.",
                features
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Resolve a feature ID for a workflow run.
    ///
    /// This is an intentional cross-domain convenience orchestrator: it touches
    /// `TicketSyncer` and `WorktreeManager` so that callers (CLI, TUI, web, MCP)
    /// share a single resolution path instead of each re-implementing the lookup
    /// chain. The coupling is accepted because feature resolution inherently
    /// requires ticket and worktree context.
    ///
    /// Resolution order:
    /// 1. Explicit feature name → look up by repo slug + name, verify active.
    /// 2. Ticket ID provided → auto-detect from feature_tickets table.
    /// 3. Repo + worktree slugs provided → look up worktree's linked ticket, then auto-detect.
    /// 4. None of the above → `Ok(None)`.
    ///
    /// When `feature_name` is `Some`, a repo slug is required — it is derived from
    /// `repo_slug`, or by looking up the ticket's repo when only `ticket_id` is given.
    /// Returns an error if no repo context can be determined.
    pub fn resolve_feature_id_for_run(
        &self,
        feature_name: Option<&str>,
        repo_slug: Option<&str>,
        ticket_id: Option<&str>,
        worktree_slug: Option<&str>,
    ) -> Result<Option<String>> {
        if let Some(feat_name) = feature_name {
            // Explicit feature — need a repo slug.
            let slug = if let Some(s) = repo_slug {
                s.to_string()
            } else if let Some(tid) = ticket_id {
                let t = TicketSyncer::new(self.conn).get_by_id(tid)?;
                let r = RepoManager::new(self.conn, self.config).get_by_id(&t.repo_id)?;
                r.slug
            } else {
                return Err(ConductorError::Workflow(
                    "Feature resolution requires a repo context (provide a repo, ticket, or worktree)"
                        .to_string(),
                ));
            };
            let f = self.resolve_active_feature(&slug, feat_name)?;
            return Ok(Some(f.id));
        }

        if let Some(tid) = ticket_id {
            return Ok(self.find_feature_for_ticket(tid)?.map(|f| f.id));
        }

        if let (Some(rs), Some(ws)) = (repo_slug, worktree_slug) {
            let r = RepoManager::new(self.conn, self.config).get_by_slug(rs)?;
            let wt = WorktreeManager::new(self.conn, self.config).get_by_slug(&r.id, ws)?;
            if let Some(ref tid) = wt.ticket_id {
                return Ok(self.find_feature_for_ticket(tid)?.map(|f| f.id));
            }
        }

        Ok(None)
    }

    /// Unlink tickets (by source_id) from a feature.
    pub fn unlink_tickets(
        &self,
        repo_slug: &str,
        feature_name: &str,
        ticket_source_ids: &[String],
    ) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;
        let ticket_ids = self.resolve_ticket_ids(&repo.id, ticket_source_ids)?;

        if !ticket_ids.is_empty() {
            with_in_clause(
                "DELETE FROM feature_tickets WHERE feature_id = ?1 AND ticket_id IN",
                &[&feature.id as &dyn rusqlite::types::ToSql],
                &ticket_ids,
                |sql, params| -> Result<()> {
                    self.conn.prepare(sql)?.execute(params)?;
                    Ok(())
                },
            )?;
        }
        Ok(())
    }

    /// Sync open issues from the feature's GitHub milestone into its ticket queue.
    ///
    /// Requires the feature to have `source_type = "github_milestone"` and a
    /// `source_id` in the format `github.com/{owner}/{repo}/milestones/{number}`.
    ///
    /// 1. Fetches open issues from the milestone via `gh api`.
    /// 2. Upserts them into `tickets` (idempotent on `(repo_id, source_type, source_id)`).
    /// 3. Adds `feature_tickets` links for newly discovered issues.
    /// 4. Removes links for issues no longer in the milestone (ticket records are kept).
    /// 5. Updates `features.tickets_total` with the current linked count.
    pub fn sync_from_milestone(&self, repo_slug: &str, name: &str) -> Result<SyncResult> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, name)?;

        match feature.source_type.as_deref() {
            Some("github_milestone") => {}
            _ => {
                return Err(ConductorError::InvalidInput(format!(
                    "Feature '{}' source_type is not 'github_milestone'",
                    name
                )));
            }
        }

        let source_id = feature.source_id.as_deref().ok_or_else(|| {
            ConductorError::InvalidInput(format!("Feature '{}' has no source_id configured", name))
        })?;

        let (owner, repo_name, milestone_number) = parse_milestone_source_id(source_id)?;

        let inputs =
            crate::github::fetch_milestone_issues(&owner, &repo_name, milestone_number, None)?;

        self.apply_milestone_sync(&repo.id, &feature.id, &inputs)
    }

    /// Apply a set of milestone ticket inputs to a feature's ticket queue.
    ///
    /// Upserts tickets, computes the diff against currently linked tickets, adds new
    /// links, removes stale ones, and updates `tickets_total`. Exposed as
    /// `pub(super)` so tests can call it directly with pre-built inputs without
    /// requiring a live `gh` CLI.
    pub(super) fn apply_milestone_sync(
        &self,
        repo_id: &str,
        feature_id: &str,
        inputs: &[crate::tickets::TicketInput],
    ) -> Result<SyncResult> {
        use std::collections::HashSet;

        // Upsert tickets into the tickets table. TicketSyncer wraps this in
        // its own transaction so it is already atomic.
        let syncer = TicketSyncer::new(self.conn);
        syncer.upsert_tickets(repo_id, inputs)?;

        // Resolve just-upserted tickets to their internal IDs.
        let fetched_source_ids: Vec<String> = inputs.iter().map(|i| i.source_id.clone()).collect();
        let fetched_ids: Vec<String> = if fetched_source_ids.is_empty() {
            Vec::new()
        } else {
            with_in_clause(
                "SELECT id FROM tickets WHERE repo_id = ?1 AND source_type = 'github' AND source_id IN",
                &[&repo_id as &dyn rusqlite::types::ToSql],
                &fetched_source_ids,
                |sql, params| -> Result<Vec<String>> {
                    let mut stmt = self.conn.prepare(sql)?;
                    let mut rows = stmt.query(params)?;
                    let mut ids = Vec::new();
                    while let Some(row) = rows.next()? {
                        ids.push(row.get(0)?);
                    }
                    Ok(ids)
                },
            )?
        };

        // Get currently linked ticket IDs for this feature.
        let linked_ids: Vec<String> = query_collect(
            self.conn,
            "SELECT ticket_id FROM feature_tickets WHERE feature_id = ?1",
            params![feature_id],
            |row| row.get(0),
        )?;

        let fetched_set: HashSet<String> = fetched_ids.into_iter().collect();
        let linked_set: HashSet<String> = linked_ids.into_iter().collect();

        let to_add: Vec<String> = fetched_set.difference(&linked_set).cloned().collect();
        let to_remove: Vec<String> = linked_set.difference(&fetched_set).cloned().collect();

        let added = to_add.len();
        let removed = to_remove.len();

        // Wrap the link/unlink/count-update steps in a single transaction so
        // they land atomically and avoid N individual auto-commits.
        let tx = self.conn.unchecked_transaction()?;

        self.link_tickets_internal(feature_id, &to_add)?;

        if !to_remove.is_empty() {
            with_in_clause(
                "DELETE FROM feature_tickets WHERE feature_id = ?1 AND ticket_id IN",
                &[&feature_id as &dyn rusqlite::types::ToSql],
                &to_remove,
                |sql, params| -> Result<()> {
                    self.conn.prepare(sql)?.execute(params)?;
                    Ok(())
                },
            )?;
        }

        // Recount linked tickets and update the denormalized total.
        self.conn.execute(
            "UPDATE features SET tickets_total = (SELECT COUNT(*) FROM feature_tickets WHERE feature_id = ?1) WHERE id = ?1",
            params![feature_id],
        )?;

        tx.commit()?;

        Ok(SyncResult { added, removed })
    }

    /// Create a PR for the feature branch via `gh pr create`.
    pub fn create_pr(&self, repo_slug: &str, feature_name: &str, draft: bool) -> Result<String> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;

        let mut args = vec![
            "pr",
            "create",
            "--fill",
            "--head",
            &feature.branch,
            "--base",
            &feature.base_branch,
        ];
        if draft {
            args.push("--draft");
        }

        let output = Command::new("gh")
            .args(&args)
            .current_dir(&repo.local_path)
            .output()
            .map_err(|e| {
                ConductorError::GhCli(SubprocessFailure::from_message(
                    "gh",
                    format!("failed to run `gh`: {e}"),
                ))
            })?;
        if !output.status.success() {
            return Err(ConductorError::GhCli(SubprocessFailure {
                command: "gh".to_string(),
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            }));
        }
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(url)
    }

    /// Permanently delete a closed or merged feature: removes the local git branch
    /// (safe `-d`, not `-D`), cascade-deletes `feature_tickets` rows, and deletes
    /// the `features` record. Active features are rejected with `FeatureStillActive`.
    pub fn delete(&self, repo_slug: &str, name: &str) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, name)?;

        if feature.status == FeatureStatus::InProgress {
            return Err(ConductorError::FeatureStillActive {
                repo: repo_slug.to_string(),
                name: name.to_string(),
            });
        }

        // Delete local branch (safe -d). If the branch doesn't exist locally,
        // treat it as a no-op so the command remains retryable.
        let branch_output = git_in(&repo.local_path)
            .args(["branch", "-d", "--", &feature.branch])
            .output()
            .map_err(|e| {
                ConductorError::Git(SubprocessFailure::from_message(
                    "git branch -d",
                    format!("failed to run git: {e}"),
                ))
            })?;
        if !branch_output.status.success() {
            let stderr = String::from_utf8_lossy(&branch_output.stderr);
            // Branch already gone — that's fine.
            if !stderr.contains("not found") && !stderr.contains("no branch named") {
                return Err(ConductorError::Git(SubprocessFailure {
                    command: "git branch -d".to_string(),
                    exit_code: branch_output.status.code(),
                    stderr: stderr.trim().to_string(),
                    stdout: String::from_utf8_lossy(&branch_output.stdout)
                        .trim()
                        .to_string(),
                }));
            }
        }

        self.conn.execute(
            "DELETE FROM feature_tickets WHERE feature_id = ?1",
            params![feature.id],
        )?;
        self.conn
            .execute("DELETE FROM features WHERE id = ?1", params![feature.id])?;

        Ok(())
    }

    /// Transition a feature through the explicit state machine.
    ///
    /// Valid transitions:
    /// - `in_progress → ready_for_review`
    /// - `ready_for_review → approved`
    /// - `approved → merged`
    /// - `any → closed` (delegates to `close_with_merge_detection` so `merged_at` is set correctly)
    ///
    /// Returns `ConductorError::InvalidFeatureTransition` for all other pairs.
    pub fn transition(
        &self,
        repo_slug: &str,
        feature_name: &str,
        to: FeatureStatus,
    ) -> Result<Feature> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;

        // `closed` is always allowed — delegate to merge-detection close.
        if to == FeatureStatus::Closed {
            self.close_with_merge_detection(&repo.local_path, &feature)?;
            return self.get_by_id(&feature.id);
        }

        let valid = matches!(
            (&feature.status, &to),
            (FeatureStatus::InProgress, FeatureStatus::ReadyForReview)
                | (FeatureStatus::ReadyForReview, FeatureStatus::Approved)
                | (FeatureStatus::Approved, FeatureStatus::Merged)
        );

        if !valid {
            return Err(ConductorError::InvalidFeatureTransition {
                name: feature_name.to_string(),
                from: feature.status.to_string(),
                to: to.to_string(),
            });
        }

        self.conn.execute(
            "UPDATE features SET status = ?1 WHERE id = ?2",
            params![to, feature.id],
        )?;

        self.get_by_id(&feature.id)
    }

    /// Update a feature's status by ID without re-running state-machine guards.
    ///
    /// Used internally by callers that already know the current status (e.g.
    /// `auto_ready_for_review_if_complete`). Kept `pub(crate)` to prevent
    /// accidental bypass of the public `transition()` guard.
    pub(crate) fn transition_by_id(&self, feature_id: &str, to: FeatureStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE features SET status = ?1 WHERE id = ?2",
            params![to, feature_id],
        )?;
        Ok(())
    }

    /// Close a feature (set status to closed, or merged if the branch was merged).
    pub fn close(&self, repo_slug: &str, feature_name: &str) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;
        self.close_with_merge_detection(&repo.local_path, &feature)
    }

    /// Detect whether the feature branch was merged and update its status
    /// accordingly (merged vs closed). Shared by `close()` and
    /// `auto_close_if_orphaned()`.
    fn close_with_merge_detection(&self, repo_path: &str, feature: &Feature) -> Result<()> {
        let merged =
            crate::git::is_branch_merged_local(repo_path, &feature.branch, &feature.base_branch)
                || crate::git::is_branch_merged_remote(
                    repo_path,
                    &feature.branch,
                    &feature.base_branch,
                );

        let now = Utc::now().to_rfc3339();
        if merged {
            self.conn.execute(
                "UPDATE features SET status = ?1, merged_at = ?2 WHERE id = ?3",
                params![FeatureStatus::Merged, now, feature.id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE features SET status = ?1 WHERE id = ?2",
                params![FeatureStatus::Closed, feature.id],
            )?;
        }
        Ok(())
    }

    /// Auto-close a feature if it has no remaining active worktrees and its
    /// git branch no longer exists locally.
    ///
    /// Called after a worktree is deleted. If the feature still has active
    /// worktrees, or if the branch still exists locally (user may create new
    /// worktrees later), this is a no-op.
    pub(crate) fn auto_close_if_orphaned(
        &self,
        repo: &crate::repo::Repo,
        feature_branch: &str,
    ) -> Result<()> {
        // Find the active feature for this branch
        let feature: Option<Feature> = self
            .conn
            .query_row(
                &format!("SELECT {FEATURE_COLS} FROM features WHERE repo_id = ?1 AND branch = ?2 AND status = 'in_progress'"),
                params![repo.id, feature_branch],
                map_feature_row,
            )
            .optional()?;

        let feature = match feature {
            Some(f) => f,
            None => return Ok(()), // No active feature for this branch
        };

        // Count remaining active worktrees targeting this feature's branch
        let active_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM worktrees WHERE repo_id = ?1 AND base_branch = ?2 AND status = 'active'",
            params![repo.id, feature_branch],
            |row| row.get(0),
        )?;

        if active_count > 0 {
            return Ok(()); // Other active worktrees still reference this feature
        }

        // Check if the branch still exists locally
        if crate::git::local_branch_exists(&repo.local_path, feature_branch)? {
            return Ok(()); // Branch still exists — user may reuse it
        }

        self.close_with_merge_detection(&repo.local_path, &feature)
    }

    /// Convenience wrapper called after a worktree is deleted.
    ///
    /// Looks up the repo from `repo_id`, checks the worktree's `base_branch`
    /// and, if it differs from the repo's default branch, delegates to
    /// [`auto_close_if_orphaned`]. Accepts plain IDs instead of domain structs
    /// to avoid bidirectional module coupling.
    pub(crate) fn auto_close_after_worktree_delete(
        &self,
        repo_id: &str,
        base_branch: Option<&str>,
    ) -> Result<()> {
        let base_branch = match base_branch {
            Some(b) => b,
            None => return Ok(()),
        };
        let repo = crate::repo::RepoManager::new(self.conn, self.config).get_by_id(repo_id)?;
        if base_branch != repo.default_branch {
            return self.auto_close_if_orphaned(&repo, base_branch);
        }
        Ok(())
    }

    /// Transition an `in_progress` feature to `ready_for_review` if all of its
    /// worktrees have been merged.
    ///
    /// Called by `cleanup_merged_worktrees` after marking a worktree merged,
    /// when `config.defaults.auto_ready_for_review` is `true`. Safe to call
    /// even when no feature exists for the branch — returns `Ok(())` in that case.
    pub(crate) fn auto_ready_for_review_if_complete(
        &self,
        repo_id: &str,
        feature_branch: &str,
    ) -> Result<()> {
        // Find the in_progress feature for this branch.
        let feature: Option<Feature> = self
            .conn
            .query_row(
                &format!("SELECT {FEATURE_COLS} FROM features WHERE repo_id = ?1 AND branch = ?2 AND status = 'in_progress'"),
                params![repo_id, feature_branch],
                map_feature_row,
            )
            .optional()?;

        let feature = match feature {
            Some(f) => f,
            None => return Ok(()),
        };

        // Count worktrees that are still active on this feature branch.
        let active_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM worktrees WHERE repo_id = ?1 AND base_branch = ?2 AND status = 'active'",
            params![repo_id, feature_branch],
            |row| row.get(0),
        )?;

        if active_count > 0 {
            return Ok(());
        }

        tracing::info!(
            feature_id = %feature.id,
            feature_name = %feature.name,
            "auto-transitioning feature to ready_for_review (last worktree merged)"
        );

        self.transition_by_id(&feature.id, FeatureStatus::ReadyForReview)
    }

    /// Auto-register a feature for a branch if none exists yet.
    ///
    /// This is a **DB-only** operation — the branch already exists (the caller
    /// is targeting it for a worktree). Returns `Ok(Some(feature))` when a new
    /// feature was created, `Ok(None)` when no action was needed (branch is the
    /// default, or a feature already exists).
    pub fn ensure_feature_for_branch(
        &self,
        repo: &crate::repo::Repo,
        branch: &str,
        base_branch: Option<&str>,
    ) -> Result<Option<Feature>> {
        use super::helpers::branch_to_feature_name;

        // No feature for the default branch.
        if branch == repo.default_branch {
            return Ok(None);
        }

        // Already registered?
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM features WHERE repo_id = ?1 AND branch = ?2 AND status = 'in_progress')",
            params![repo.id, branch],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(None);
        }

        let base_name = branch_to_feature_name(branch);

        // Use the caller-supplied base_branch, or fall back to the repo default.
        let resolved_base: String = base_branch
            .map(|s| s.to_string())
            .unwrap_or_else(|| repo.default_branch.clone());

        // Check if a non-active feature with the base name exists — if so,
        // reactivate it rather than creating a suffixed duplicate.
        let maybe_inactive: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, status, created_at FROM features WHERE repo_id = ?1 AND name = ?2 AND status != 'in_progress'",
                params![repo.id, base_name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((inactive_id, _status, created_at)) = maybe_inactive {
            self.conn.execute(
                "UPDATE features SET branch = ?1, base_branch = ?2, status = 'in_progress', merged_at = NULL WHERE id = ?3",
                params![branch, resolved_base, inactive_id],
            )?;
            return Ok(Some(Feature {
                id: inactive_id,
                repo_id: repo.id.clone(),
                name: base_name.to_string(),
                branch: branch.to_string(),
                base_branch: resolved_base,
                status: FeatureStatus::InProgress,
                created_at,
                merged_at: None,
                source_type: None,
                source_id: None,
                tickets_total: 0,
                tickets_merged: 0,
            }));
        }

        // Disambiguate if an active feature with this name already exists (on a
        // different branch).
        let mut name = base_name.to_string();
        let mut suffix = 2u32;
        loop {
            let taken: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM features WHERE repo_id = ?1 AND name = ?2)",
                params![repo.id, name],
                |row| row.get(0),
            )?;
            if !taken {
                break;
            }
            name = format!("{base_name}-{suffix}");
            suffix += 1;
        }

        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let feature = Feature {
            id: id.clone(),
            repo_id: repo.id.clone(),
            name,
            branch: branch.to_string(),
            base_branch: resolved_base,
            status: FeatureStatus::InProgress,
            created_at: now,
            merged_at: None,
            source_type: None,
            source_id: None,
            tickets_total: 0,
            tickets_merged: 0,
        };

        self.insert_feature_record(&feature)?;

        Ok(Some(feature))
    }

    /// List non-default branches that have active worktrees but no active
    /// feature record. Used by the TUI branch picker to show "orphan" branches.
    pub fn list_unregistered_branches(
        &self,
        repo_id: &str,
        default_branch: &str,
    ) -> Result<Vec<UnregisteredBranch>> {
        query_collect(
            self.conn,
            "SELECT w.branch, COUNT(*) as worktree_count, MIN(w.base_branch) as base_branch
             FROM worktrees w
             WHERE w.repo_id = ?1
               AND w.status = 'active'
               AND w.branch != ?2
               AND w.branch NOT IN (SELECT f.branch FROM features f WHERE f.repo_id = ?1 AND f.status = 'in_progress')
             GROUP BY w.branch",
            params![repo_id, default_branch],
            |row| {
                Ok(UnregisteredBranch {
                    branch: row.get(0)?,
                    worktree_count: row.get(1)?,
                    base_branch: row.get(2)?,
                })
            },
        )
    }

    // -----------------------------------------------------------------------
    // Staleness detection
    // -----------------------------------------------------------------------

    /// Refresh `last_commit_at` for a single feature by running `git log` on
    /// the feature branch. Returns the new timestamp, or `None` if the branch
    /// is not reachable locally.
    pub fn refresh_last_commit(&self, feature_id: &str) -> Result<Option<String>> {
        let feature = self.get_by_id(feature_id)?;
        let repo = RepoManager::new(self.conn, self.config).get_by_id(&feature.repo_id)?;

        let ts = last_commit_timestamp(&repo.local_path, &feature.branch);

        self.conn.execute(
            "UPDATE features SET last_commit_at = ?1 WHERE id = ?2",
            params![ts, feature_id],
        )?;
        Ok(ts)
    }

    /// Batch-refresh `last_commit_at` for all active features of a repo.
    /// Uses a single `git for-each-ref` call to avoid N+1 subprocess spawns.
    pub fn refresh_last_commit_all(&self, repo_slug: &str) -> Result<()> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        let features: Vec<(String, String)> = query_collect(
            self.conn,
            "SELECT id, branch FROM features WHERE repo_id = ?1 AND status = 'in_progress'",
            params![repo.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if features.is_empty() {
            return Ok(());
        }

        // Batch-fetch committer dates for all local branches in one subprocess.
        let branch_timestamps = batch_branch_timestamps(&repo.local_path);

        for (id, branch) in &features {
            let ts = branch_timestamps.get(branch.as_str()).cloned();

            self.conn.execute(
                "UPDATE features SET last_commit_at = ?1 WHERE id = ?2",
                params![ts, id],
            )?;
        }
        Ok(())
    }

    /// Returns `true` when the feature is stale: active with no recent git
    /// commits and no recent worktree activity within `threshold_days`.
    /// A threshold of 0 disables stale detection (always returns false).
    pub fn is_stale(feature: &FeatureRow, threshold_days: u32) -> bool {
        if threshold_days == 0 {
            return false;
        }
        if feature.status != FeatureStatus::InProgress {
            return false;
        }
        let cutoff = Utc::now() - chrono::Duration::days(threshold_days as i64);

        let is_recent = |ts: &str| -> bool {
            chrono::DateTime::parse_from_rfc3339(ts)
                .map(|dt| dt.with_timezone(&Utc) >= cutoff)
                .unwrap_or(false)
        };

        let commit_recent = feature.last_commit_at.as_deref().is_some_and(is_recent);
        let wt_recent = feature
            .last_worktree_activity
            .as_deref()
            .is_some_and(is_recent);

        !commit_recent && !wt_recent
    }

    /// Compute the number of days since the most recent activity (commit or
    /// worktree). Returns `None` when no activity data is available.
    pub fn stale_days(feature: &FeatureRow) -> Option<u64> {
        let latest = [
            feature.last_commit_at.as_deref(),
            feature.last_worktree_activity.as_deref(),
        ]
        .into_iter()
        .flatten()
        .max()?;

        let parsed = chrono::DateTime::parse_from_rfc3339(latest).ok()?;
        let diff = Utc::now().signed_duration_since(parsed);
        Some(diff.num_days().max(0) as u64)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn get_feature_by_repo_id(&self, repo_id: &str, name: &str) -> Result<Feature> {
        self.conn
            .query_row(
                &format!("SELECT {FEATURE_COLS} FROM features WHERE repo_id = ?1 AND name = ?2"),
                params![repo_id, name],
                map_feature_row,
            )
            .map_err(feature_not_found(name))
    }

    /// Resolve ticket source_ids (e.g. "1262") to internal ULID ticket IDs.
    fn resolve_ticket_ids(&self, repo_id: &str, source_ids: &[String]) -> Result<Vec<String>> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }

        let map = with_in_clause(
            "SELECT id, source_id FROM tickets WHERE repo_id = ?1 AND source_id IN",
            &[&repo_id as &dyn rusqlite::types::ToSql],
            source_ids,
            |sql, params| -> Result<std::collections::HashMap<String, String>> {
                let mut stmt = self.conn.prepare(sql)?;
                let mut rows = stmt.query(params)?;
                let mut map = std::collections::HashMap::new();
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    let source_id: String = row.get(1)?;
                    map.insert(source_id, id);
                }
                Ok(map)
            },
        )?;

        // Verify all requested source_ids were found, preserving order
        let mut ids = Vec::with_capacity(source_ids.len());
        for sid in source_ids {
            match map.get(sid) {
                Some(id) => ids.push(id.clone()),
                None => {
                    return Err(ConductorError::TicketNotFound { id: sid.clone() });
                }
            }
        }
        Ok(ids)
    }

    /// Link a single ticket to a feature (idempotent — uses INSERT OR IGNORE).
    pub fn link_ticket(&self, feature_id: &str, ticket_id: &str) -> Result<()> {
        self.link_tickets_internal(feature_id, &[ticket_id.to_string()])
    }

    fn link_tickets_internal(&self, feature_id: &str, ticket_ids: &[String]) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO feature_tickets (feature_id, ticket_id) VALUES (?1, ?2)",
        )?;
        for tid in ticket_ids {
            stmt.execute(params![feature_id, tid])?;
        }
        Ok(())
    }

    pub(super) fn insert_feature_record(&self, feature: &Feature) -> Result<()> {
        self.conn.execute(
            "INSERT INTO features (id, repo_id, name, branch, base_branch, status, created_at, source_type, source_id, tickets_total, tickets_merged)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                feature.id,
                feature.repo_id,
                feature.name,
                feature.branch,
                feature.base_branch,
                feature.status,
                feature.created_at,
                feature.source_type,
                feature.source_id,
                feature.tickets_total,
                feature.tickets_merged,
            ],
        )?;
        Ok(())
    }

    /// Return the feature ID for an active feature matching `repo_id` + `branch`,
    /// or `None` if no such feature exists.
    pub fn get_active_id_by_repo_and_branch(
        &self,
        repo_id: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM features WHERE repo_id = ?1 AND branch = ?2 AND status = 'in_progress'",
                params![repo_id, branch],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Slugify a string for use in a worktree name: lowercase, replace non-alphanum with hyphens, deduplicate hyphens, trim.
    fn slugify(s: &str) -> String {
        let mut slug = s
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>();
        // Collapse consecutive hyphens
        while slug.contains("--") {
            slug = slug.replace("--", "-");
        }
        slug.trim_matches('-').to_string()
    }

    /// Query eligible tickets for a feature: tickets linked via `feature_tickets` that do not
    /// already have an `active` or `merged` worktree in the given repo.
    /// Tickets with only `abandoned` worktrees are eligible (retry-able).
    pub fn eligible_tickets(&self, feature_id: &str, repo_id: &str) -> Result<Vec<String>> {
        query_collect(
            self.conn,
            "SELECT t.id FROM tickets t \
             JOIN feature_tickets ft ON ft.ticket_id = t.id \
             WHERE ft.feature_id = ?1 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM worktrees w \
                 WHERE w.ticket_id = t.id \
                   AND w.repo_id = ?2 \
                   AND w.status IN ('active', 'merged') \
               )",
            params![feature_id, repo_id],
            |row| row.get(0),
        )
    }

    /// Dispatch a single ticket: create a worktree, create an agent run, spawn a headless agent,
    /// and notify `done_tx` when the drain thread finishes.
    ///
    /// Available on Unix only (headless spawning uses `#[cfg(unix)]` APIs).
    #[cfg(unix)]
    fn dispatch_ticket(
        &self,
        repo_slug: &str,
        feature: &Feature,
        ticket_id: &str,
        done_tx: std::sync::mpsc::Sender<()>,
    ) -> Result<()> {
        use crate::agent::AgentManager;
        use crate::agent_runtime::{self, SpawnHeadlessParams};
        use crate::config::agent_log_path;
        use crate::db::open_database_compat;
        use crate::worktree::WorktreeCreateOptions;

        // Fetch ticket details for prompt + slug
        let ticket = crate::tickets::TicketSyncer::new(self.conn).get_by_id(ticket_id)?;

        // Build a descriptive worktree name from the source_id + title slug
        let title_slug = Self::slugify(&ticket.title);
        let raw_name = format!("feat-{}-{}", ticket.source_id, title_slug);
        // Cap at 60 chars to keep slugs manageable.
        // Use char_indices to find a safe UTF-8 boundary (slugify passes non-ASCII
        // alphanumerics through unchanged, so multi-byte codepoints are possible).
        let wt_name = {
            let end = raw_name
                .char_indices()
                .nth(60)
                .map_or(raw_name.len(), |(i, _)| i);
            raw_name[..end].trim_end_matches('-').to_string()
        };

        // Create worktree based off the feature branch
        let wt_opts = WorktreeCreateOptions {
            from_branch: Some(feature.branch.clone()),
            ticket_id: Some(ticket_id.to_string()),
            ..Default::default()
        };
        let (wt, _warnings) =
            WorktreeManager::new(self.conn, self.config).create(repo_slug, &wt_name, wt_opts)?;

        // Build agent prompt. Ticket bodies are user-supplied (GitHub issue content) and
        // are not sanitized for prompt injection; truncate to a reasonable length to limit
        // the blast radius of any adversarially crafted issue body.
        const MAX_BODY_CHARS: usize = 4_000;
        let prompt = if ticket.body.trim().is_empty() {
            format!("Implement ticket #{}: {}", ticket.source_id, ticket.title)
        } else {
            let body_chars = ticket.body.chars().count();
            let body: std::borrow::Cow<str> = if body_chars > MAX_BODY_CHARS {
                let end = ticket
                    .body
                    .char_indices()
                    .nth(MAX_BODY_CHARS)
                    .map_or(ticket.body.len(), |(i, _)| i);
                format!("{}\n\n[body truncated at {MAX_BODY_CHARS} chars]", &ticket.body[..end])
                    .into()
            } else {
                ticket.body.as_str().into()
            };
            format!(
                "Implement ticket #{}: {}\n\n---\n{}",
                ticket.source_id, ticket.title, body
            )
        };

        // Create agent run record (no tmux window — headless)
        let run = AgentManager::new(self.conn).create_run(Some(&wt.id), &prompt, None, None)?;

        eprintln!(
            "[feature::run] Spawning agent run {} for ticket #{} ({})",
            run.id, ticket.source_id, ticket.title
        );

        // Spawn headless subprocess
        let params = SpawnHeadlessParams {
            run_id: &run.id,
            working_dir: &wt.path,
            prompt: &prompt,
            resume_session_id: None,
            model: None,
            bot_name: None,
            permission_mode: Some(&self.config.general.agent_permission_mode),
            plugin_dirs: &[],
        };
        let (handle, prompt_file) =
            agent_runtime::try_spawn_headless_run(&params).map_err(|e| {
                ConductorError::Workflow(format!(
                    "spawn failed for ticket {}: {}",
                    ticket.source_id, e
                ))
            })?;

        // Persist the subprocess PID. This is best-effort: if it fails the agent still
        // runs and the orphan reaper will clean it up via log recovery. Intentionally
        // fire-and-forget — do not propagate the error.
        let pid = handle.pid();
        let _ = AgentManager::new(self.conn).update_run_subprocess_pid(&run.id, pid);

        // Spawn a drain thread with its own DB connection (Connection is not Send)
        let run_id = run.id.clone();
        let log_path = agent_log_path(&run.id);
        std::thread::spawn(move || {
            let (stdout, finish) = handle.into_drain_parts();
            let conn = match open_database_compat(&crate::config::db_path()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("fan-out drain: failed to open DB for run {run_id}: {e}");
                    let _ = std::fs::remove_file(&prompt_file);
                    let _ = done_tx.send(());
                    return;
                }
            };
            let mgr = AgentManager::new(&conn);
            agent_runtime::drain_stream_json(stdout, &run_id, &log_path, &mgr, |_| {});
            let _ = std::fs::remove_file(&prompt_file);
            finish();
            let _ = done_tx.send(());
        });

        Ok(())
    }

    /// Fan-out: create worktrees and spawn headless agents for all eligible tickets in a feature.
    ///
    /// Respects `config.general.max_feature_parallelism` (default 3); `parallel_override`
    /// takes precedence when `Some`.  Blocks until all agents finish.
    ///
    /// Returns a [`RunSummary`] with counts of dispatched and failed tickets.
    ///
    /// Available on Unix only (headless spawning uses `#[cfg(unix)]` APIs).
    #[cfg(unix)]
    pub fn run(
        &self,
        repo_slug: &str,
        feature_name: &str,
        parallel_override: Option<u32>,
    ) -> Result<RunSummary> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;
        let feature = self.get_feature_by_repo_id(&repo.id, feature_name)?;

        let parallelism = parallel_override
            .unwrap_or(self.config.general.max_feature_parallelism)
            .max(1) as usize;

        let ticket_ids = self.eligible_tickets(&feature.id, &repo.id)?;
        if ticket_ids.is_empty() {
            eprintln!("[feature::run] No eligible tickets for feature '{feature_name}'");
            return Ok(RunSummary {
                dispatched: 0,
                failed: 0,
            });
        }

        eprintln!(
            "[feature::run] {} eligible ticket(s), parallelism={}",
            ticket_ids.len(),
            parallelism
        );

        let mut queue: std::collections::VecDeque<String> = ticket_ids.into_iter().collect();
        let mut in_flight: usize = 0;
        let mut dispatched: u32 = 0;
        let mut failed: u32 = 0;

        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        // Returns `(dispatched_ok, error_message)` for one dispatch attempt.
        let try_dispatch = |tid: &str| -> (bool, Option<String>) {
            let tx = done_tx.clone();
            match self.dispatch_ticket(repo_slug, &feature, tid, tx) {
                Ok(()) => (true, None),
                Err(e) => (
                    false,
                    Some(format!("[feature::run] Failed to dispatch ticket {tid}: {e}")),
                ),
            }
        };

        // Spawn initial batch up to parallelism
        while in_flight < parallelism {
            let Some(tid) = queue.pop_front() else { break };
            let (ok, err_msg) = try_dispatch(&tid);
            if ok {
                dispatched += 1;
                in_flight += 1;
            } else {
                eprintln!("{}", err_msg.unwrap());
                failed += 1;
            }
        }

        // Wait for completions and dispatch queued tickets
        while in_flight > 0 {
            match done_rx.recv() {
                Ok(()) => {
                    in_flight -= 1;
                    if let Some(tid) = queue.pop_front() {
                        let (ok, err_msg) = try_dispatch(&tid);
                        if ok {
                            dispatched += 1;
                            in_flight += 1;
                        } else {
                            eprintln!("{}", err_msg.unwrap());
                            failed += 1;
                        }
                    }
                }
                Err(_) => {
                    // Channel closed unexpectedly; count as in-flight completing
                    in_flight -= 1;
                }
            }
        }

        eprintln!(
            "[feature::run] Done — dispatched={}, failed={}",
            dispatched, failed
        );
        Ok(RunSummary { dispatched, failed })
    }

    /// Non-Unix stub for `run()` — returns an error on unsupported platforms.
    #[cfg(not(unix))]
    pub fn run(
        &self,
        _repo_slug: &str,
        _feature_name: &str,
        _parallel_override: Option<u32>,
    ) -> Result<RunSummary> {
        Err(ConductorError::Workflow(
            "feature run is not supported on this platform".to_string(),
        ))
    }

    /// Scan features with `status = 'in_progress'` and zero active worktrees for `repo_slug`.
    /// For each candidate, checks `gh pr list --base <branch>` for open PRs.
    /// Returns features with no open PRs — these are "dangling" (abandoned without cleanup).
    ///
    /// `dangling` is a derived state — no DB column is written.
    ///
    /// Use this when you have a specific repo slug; for cross-repo checks use
    /// [`reap_dangling_all`] instead (preferred by TUI, web, and CLI integration points).
    pub fn reap_dangling(&self, repo_slug: &str) -> Result<Vec<Feature>> {
        let repo = RepoManager::new(self.conn, self.config).get_by_slug(repo_slug)?;

        let candidates: Vec<Feature> = query_collect(
            self.conn,
            &format!(
                "SELECT {FEATURE_COLS} FROM features f \
                 WHERE f.repo_id = ?1 \
                   AND f.status = 'in_progress' \
                   AND (SELECT COUNT(*) FROM worktrees w \
                        WHERE w.repo_id = f.repo_id \
                          AND w.base_branch = f.branch \
                          AND w.status = 'active') = 0"
            ),
            params![repo.id],
            map_feature_row,
        )?;

        let mut dangling = Vec::new();
        for feature in candidates {
            if !self.has_open_pr(&repo.local_path, &feature.branch) {
                dangling.push(feature);
            }
        }
        Ok(dangling)
    }

    /// Scan features with `status = 'in_progress'` and zero active worktrees across all repos.
    /// For each candidate, checks `gh pr list --base <branch>` for open PRs.
    /// Returns features with no open PRs.
    ///
    /// `dangling` is a derived state — no DB column is written.
    pub fn reap_dangling_all(&self) -> Result<Vec<Feature>> {
        // Join features with repos to get local_path for gh pr list
        let candidates: Vec<(Feature, String)> = query_collect(
            self.conn,
            &format!(
                "SELECT {FEATURE_COLS_ALIASED}, r.local_path \
                 FROM features f \
                 JOIN repos r ON r.id = f.repo_id \
                 WHERE f.status = 'in_progress' \
                   AND (SELECT COUNT(*) FROM worktrees w \
                        WHERE w.repo_id = f.repo_id \
                          AND w.base_branch = f.branch \
                          AND w.status = 'active') = 0"
            ),
            params![],
            |row| {
                let feature = map_feature_row(row)?;
                let local_path: String = row.get(FEATURE_COLS_COUNT)?;
                Ok((feature, local_path))
            },
        )?;

        let mut dangling = Vec::new();
        for (feature, local_path) in candidates {
            if !self.has_open_pr(&local_path, &feature.branch) {
                dangling.push(feature);
            }
        }
        Ok(dangling)
    }

    /// Returns `true` if there is at least one open PR targeting `branch` in the repo
    /// at `repo_path`. Returns `false` on error or if no open PR exists.
    fn has_open_pr(&self, repo_path: &str, branch: &str) -> bool {
        let output = Command::new("gh")
            .args([
                "pr", "list", "--base", branch, "--state", "open", "--json", "number", "--jq",
                "length",
            ])
            .current_dir(repo_path)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let count_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                count_str.parse::<u64>().map(|n| n > 0).unwrap_or(false)
            }
            _ => false,
        }
    }
}
