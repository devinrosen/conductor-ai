use chrono::Utc;
use rusqlite::{named_params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::config::Config;
use crate::db::query_collect;
use crate::error::{ConductorError, Result};
use crate::git::{check_gh_output, check_output, git_in};
use crate::repo::RepoManager;
use crate::tickets::TicketSyncer;

use super::git_helpers::*;
use super::types::{map_worktree_row, Worktree, WorktreeStatus, WorktreeWithStatus};
use super::{WORKTREE_COLUMNS, WORKTREE_COLUMNS_W};

/// Map a ticket label to the conventional-commit branch prefix it implies.
///
/// Matching is case-insensitive and exact (no substring matching).
/// First matching label wins. Returns `"feat"` when no label matches.
pub fn label_to_branch_prefix(labels: &[&str]) -> &'static str {
    for label in labels {
        match label.to_lowercase().as_str() {
            "bug" | "fix" | "security" => return "fix",
            "chore" | "maintenance" => return "chore",
            "documentation" | "docs" => return "docs",
            "refactor" => return "refactor",
            "test" | "testing" => return "test",
            "ci" | "build" => return "ci",
            "perf" | "performance" => return "perf",
            _ => {}
        }
    }
    "feat"
}

fn worktree_not_found(slug: impl Into<String>) -> impl FnOnce(rusqlite::Error) -> ConductorError {
    let slug = slug.into();
    move |e| match e {
        rusqlite::Error::QueryReturnedNoRows => ConductorError::WorktreeNotFound { slug },
        _ => ConductorError::Database(e),
    }
}

/// SQL fragment that LEFT JOINs the latest agent run per worktree.
///
/// Adds one extra column: `latest.status AS agent_status`.
/// Must be used together with `map_enriched_row`.
const AGENT_LATEST_JOIN: &str = "LEFT JOIN (\
        SELECT a.worktree_id, a.status \
        FROM agent_runs a \
        INNER JOIN (\
            SELECT worktree_id, MAX(started_at) AS max_started \
            FROM agent_runs \
            WHERE worktree_id IS NOT NULL \
            GROUP BY worktree_id\
        ) top ON a.worktree_id = top.worktree_id AND a.started_at = top.max_started \
        GROUP BY a.worktree_id\
    ) latest ON latest.worktree_id = w.id";

/// Returns the base SELECT+FROM+JOIN fragment for enriched worktree queries.
/// This includes all worktree columns plus ticket info and latest agent status.
/// To be used with `map_enriched_row`.
fn enriched_worktree_base() -> String {
    format!(
        "SELECT {cols}, latest.status AS agent_status, \
         t.title AS ticket_title, t.source_id AS ticket_number, t.url AS ticket_url \
         FROM worktrees w \
         {agent_join} \
         LEFT JOIN tickets t ON t.id = w.ticket_id",
        cols = &*WORKTREE_COLUMNS_W,
        agent_join = AGENT_LATEST_JOIN,
    )
}

/// Map a row that contains the standard worktree columns followed by
/// `agent_status`, `ticket_title`, `ticket_number`, and `ticket_url`.
fn map_enriched_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeWithStatus> {
    let worktree = map_worktree_row(row)?;
    let agent_status: Option<crate::agent::AgentRunStatus> = row.get("agent_status")?;
    let ticket_title: Option<String> = row.get("ticket_title")?;
    let ticket_number: Option<String> = row.get("ticket_number")?;
    let ticket_url: Option<String> = row.get("ticket_url")?;
    Ok(WorktreeWithStatus {
        worktree,
        agent_status,
        ticket_title,
        ticket_number,
        ticket_url,
    })
}

/// Look up a ticket's dependencies and return the branch of the first parent that has
/// an active worktree.  Returns `None` if the ticket has no resolvable parent branch
/// (no dependency metadata for its source type, no deps, or no parent worktree).
///
/// Dependency IDs are extracted via [`crate::ticket_source::get_dependency_ids`];
/// swap to a `ticket_dependencies` table query once RFC 009 lands.
fn resolve_parent_branch(conn: &Connection, ticket_id: &str, repo_id: &str) -> Option<String> {
    let syncer = TicketSyncer::new(conn);
    let ticket = match syncer.get_by_id(ticket_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("resolve_parent_branch: failed to look up ticket {ticket_id}: {e}");
            return None;
        }
    };

    let dep_ids = crate::ticket_source::get_dependency_ids(&ticket.raw_json, &ticket.source_type);
    if dep_ids.is_empty() {
        return None;
    }

    // Query 1: Get all parent tickets and their worktrees in a single JOIN query
    // This reduces N+1 queries to just 2 total: one to get the child ticket, one to get all relevant data
    let placeholders = dep_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT w.branch, t.source_id \
         FROM tickets t \
         LEFT JOIN worktrees w ON w.ticket_id = t.id AND w.status = 'active' \
         WHERE t.repo_id = ?1 AND t.source_id IN ({placeholders}) \
         ORDER BY w.created_at DESC"
    );

    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&repo_id];
    for dep_id in &dep_ids {
        params.push(dep_id);
    }

    let results: Vec<(Option<String>, String)> =
        match query_collect(conn, &sql, params.as_slice(), |row| {
            Ok((
                row.get::<_, Option<String>>(0)?, // branch (nullable if no active worktree)
                row.get::<_, String>(1)?,         // source_id
            ))
        }) {
            Ok(results) => results,
            Err(e) => {
                tracing::warn!("resolve_parent_branch: batch query failed: {e}");
                return None;
            }
        };

    // Return the first active worktree branch we find, respecting dependency order
    for dep_id in &dep_ids {
        for (branch_opt, source_id) in &results {
            if source_id == dep_id {
                if let Some(branch) = branch_opt {
                    return Some(branch.clone());
                }
                break; // Found the ticket but no active worktree, move to next dependency
            }
        }
    }

    None
}

/// Options for [`WorktreeManager::set_base_branch`].
#[derive(Debug, Default)]
pub struct SetBaseBranchOptions {
    /// When `true`, rebase the worktree branch onto the new base before recording it.
    /// When `false` (default), reject with an error if the new base is not an ancestor of HEAD.
    pub rebase: bool,
}

/// Options for creating a new worktree.
///
/// Passed to [`WorktreeManager::create`] to avoid a long positional argument list.
/// All fields are optional and default to `None` / `false`.
#[derive(Debug, Default)]
pub struct WorktreeCreateOptions {
    /// When `Some(n)`, the worktree is backed by the branch of PR #n instead
    /// of a newly-created branch. `from_branch` is ignored in that case.
    pub from_pr: Option<u32>,
    /// Start the worktree from an existing branch name instead of creating a
    /// new one.  Ignored when `from_pr` is set.
    pub from_branch: Option<String>,
    /// Associate the new worktree with this ticket ID.
    pub ticket_id: Option<String>,
    /// When `true`, skip the dirty-state check. Use only after the caller has
    /// explicitly confirmed the user wants to proceed with uncommitted changes.
    pub force_dirty: bool,
    /// Pre-computed health status from a prior `check_main_health()` call.
    /// When `Some` and the working tree is clean, the redundant `git status`
    /// inside `ensure_base_up_to_date()` is skipped.
    pub pre_health: Option<super::git_helpers::MainHealthStatus>,
}

/// Write `branch.<branch>.remote = origin` and `branch.<branch>.merge = refs/heads/<branch>`
/// into the git config at `path`. This is the non-network equivalent of `git push -u origin <branch>`,
/// ensuring bare `git push` inside the worktree always targets the correct remote branch.
pub(crate) fn set_upstream_tracking(path: &Path, branch: &str) -> Result<()> {
    check_output(git_in(path).args(["config", &format!("branch.{branch}.remote"), "origin"]))?;
    check_output(git_in(path).args([
        "config",
        &format!("branch.{branch}.merge"),
        &format!("refs/heads/{branch}"),
    ]))?;
    Ok(())
}

/// Look up the `ticket_id` linked to the worktree on `branch` in `repo_id`.
///
/// Returns `Ok(Some(ticket_id))` when found, `Ok(None)` when the worktree has
/// no linked ticket, and `Err(WorktreeNotFound)` when no worktree exists for
/// that branch.
pub fn get_ticket_id_by_branch(
    conn: &Connection,
    repo_id: &str,
    branch: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT ticket_id FROM worktrees WHERE repo_id = :repo_id AND branch = :branch",
        named_params![":repo_id": repo_id, ":branch": branch],
        |row| row.get("ticket_id"),
    )
    .map_err(worktree_not_found(branch))
}

pub struct WorktreeManager<'a> {
    conn: &'a Connection,
    config: &'a Config,
}

impl<'a> WorktreeManager<'a> {
    pub fn new(conn: &'a Connection, config: &'a Config) -> Self {
        Self { conn, config }
    }

    /// Run a read-only health check on the base branch of `repo_slug`.
    ///
    /// Resolves the base branch in the same priority order as `create()`.
    /// Returns a `MainHealthStatus` describing dirty state and staleness.
    pub fn check_main_health(
        &self,
        repo_slug: &str,
        base_branch: Option<&str>,
    ) -> Result<super::git_helpers::MainHealthStatus> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let base = base_branch
            .map(|b| b.to_string())
            .unwrap_or_else(|| resolve_base_branch(&repo.local_path, &repo.default_branch));
        Ok(check_main_health(&repo.local_path, &base))
    }

    /// Create a new worktree, ensuring the base branch is up to date first.
    ///
    /// Returns the created worktree and a list of non-fatal warnings
    /// (e.g., fetch failures, diverged base branch).
    ///
    /// When `from_pr` is `Some(n)`, the worktree is backed by the branch of PR #n
    /// instead of a newly-created branch.  `from_branch` is ignored in that case.
    ///
    /// When `force_dirty` is `true`, the dirty-state check inside
    /// `ensure_base_up_to_date()` is skipped. Use this only after the caller has
    /// explicitly confirmed the user wants to proceed with uncommitted changes.
    ///
    /// When `opts.pre_health` is `Some` and the health status shows a clean working tree,
    /// the redundant `git status --porcelain` call inside `ensure_base_up_to_date()` is
    /// skipped. Callers that already ran `check_main_health()` should pass the result here.
    pub fn create(
        &self,
        repo_slug: &str,
        name: &str,
        opts: WorktreeCreateOptions,
    ) -> Result<(Worktree, Vec<String>)> {
        let WorktreeCreateOptions {
            from_pr,
            from_branch,
            ticket_id,
            force_dirty,
            pre_health,
        } = opts;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        // Determine branch name and worktree slug.
        // "bug-" slugs are preserved as-is but map to "fix/" in git.
        const SLUG_PREFIXES: &[(&str, &str)] = &[
            ("fix-", "fix"),
            ("bug-", "fix"),
            ("feat-", "feat"),
            ("release-", "release"),
            ("chore-", "chore"),
            ("docs-", "docs"),
            ("refactor-", "refactor"),
            ("test-", "test"),
            ("ci-", "ci"),
            ("perf-", "perf"),
        ];
        let (wt_slug, branch) =
            if let Some(&(dash, slash)) = SLUG_PREFIXES.iter().find(|(d, _)| name.starts_with(d)) {
                let clean = name.strip_prefix(dash).unwrap();
                (format!("{dash}{clean}"), format!("{slash}/{clean}"))
            } else {
                (format!("feat-{name}"), format!("feat/{name}"))
            };

        // Check for existing worktree with same slug
        let existing_status: Option<WorktreeStatus> = self
            .conn
            .query_row(
                "SELECT status FROM worktrees WHERE repo_id = :repo_id AND slug = :slug",
                named_params![":repo_id": repo.id, ":slug": wt_slug],
                |row| row.get("status"),
            )
            .optional()?;

        match existing_status {
            Some(WorktreeStatus::Active) => {
                return Err(ConductorError::WorktreeAlreadyExists {
                    slug: wt_slug.clone(),
                });
            }
            Some(_) => {
                // Purge the completed record to allow slug reuse
                self.conn.execute(
                    "DELETE FROM worktrees WHERE repo_id = :repo_id AND slug = :slug",
                    named_params![":repo_id": repo.id, ":slug": wt_slug],
                )?;
            }
            None => {}
        }

        // Auto-clone if the local path doesn't exist on disk yet
        if !Path::new(&repo.local_path).exists() {
            clone_repo(&repo.remote_url, &repo.local_path)?;
        }

        let wt_path = Path::new(&repo.workspace_dir).join(&wt_slug);

        // Ensure the per-repo workspace directory exists
        std::fs::create_dir_all(&repo.workspace_dir)?;

        // (branch_name, base_branch_for_db, warnings)
        let (branch, base_for_db, warnings) = if let Some(pr_number) = from_pr {
            // --from-pr path: fetch the PR branch and record the PR's base branch
            // so that create_pr can target the correct base.
            let (pr_branch, pr_base) = fetch_pr_branch(&repo.local_path, pr_number)?;
            (pr_branch, Some(pr_base), Vec::new())
        } else {
            // Normal path: resolve base, ensure it's up to date, create a new branch.
            //
            // `resolve_and_update_base` handles:
            //   - explicit from_branch with prefix fallback (feat/, fix/)
            //   - auto-creating a local tracking branch from remote
            //   - non-checkout fast-forward updates
            let explicit_base = if let Some(b) = from_branch {
                Some(b)
            } else {
                ticket_id
                    .as_deref()
                    .and_then(|tid| resolve_parent_branch(self.conn, tid, &repo.id))
            };
            let pre_verified_clean = pre_health
                .map(|h| !h.is_dirty && !h.status_check_failed)
                .unwrap_or(false);
            let (base, warnings) = resolve_and_update_base(
                &repo.local_path,
                explicit_base.as_deref(),
                &repo.default_branch,
                force_dirty,
                pre_verified_clean,
            )?;
            check_output(git_in(&repo.local_path).args([
                "branch",
                "--",
                &branch,
                &format!("refs/heads/{base}"),
            ]))?;
            (branch, Some(base), warnings)
        };

        // Create git worktree
        check_output(git_in(&repo.local_path).args([
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            &branch,
        ]))?;

        // Set upstream tracking config so bare `git push` targets the correct remote branch.
        // This is the non-network equivalent of `git push -u origin <branch>`.
        set_upstream_tracking(&wt_path, &branch)?;

        // Detect and install deps
        install_deps(&wt_path);

        // Create isolated DB for the worktree (runs migrations + seeds)
        let wt_db_path = wt_path.join(".conductor.db");
        let wt_conn = crate::db::open_database(&wt_db_path)?;
        crate::db::seed::seed_database(&wt_conn)?;

        let id = crate::new_id();
        let now = Utc::now().to_rfc3339();

        let worktree = Worktree {
            id: id.clone(),
            repo_id: repo.id.clone(),
            slug: wt_slug,
            branch,
            path: wt_path.to_string_lossy().to_string(),
            ticket_id,
            status: WorktreeStatus::Active,
            created_at: now,
            completed_at: None,
            model: None,
            base_branch: base_for_db.clone(),
        };

        self.conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, ticket_id, status, created_at, base_branch)
             VALUES (:id, :repo_id, :slug, :branch, :path, :ticket_id, :status, :created_at, :base_branch)",
            named_params![
                ":id": worktree.id,
                ":repo_id": worktree.repo_id,
                ":slug": worktree.slug,
                ":branch": worktree.branch,
                ":path": worktree.path,
                ":ticket_id": worktree.ticket_id,
                ":status": worktree.status,
                ":created_at": worktree.created_at,
                ":base_branch": worktree.base_branch,
            ],
        )?;

        Ok((worktree, warnings))
    }

    /// Create a set of worktrees from a ticket dependency graph.
    ///
    /// Tickets are topologically sorted by their `blocks` relationships so each
    /// worktree is branched from its blocker's branch rather than `root_branch`.
    /// Fails fast if a cycle is detected or a ticket ID cannot be resolved.
    pub fn create_from_dep_graph(
        &self,
        repo_slug: &str,
        root_branch: &str,
        ticket_ids: &[String],
    ) -> Result<Vec<(Worktree, Vec<String>)>> {
        if ticket_ids.is_empty() {
            return Ok(vec![]);
        }

        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        // Resolve all caller-supplied IDs in 2 DB queries (batch ULID + batch source_id).
        let syncer = TicketSyncer::new(self.conn);
        let tickets = syncer.resolve_tickets_in_repo(&repo.id, ticket_ids)?;

        // Build the set of internal ULIDs for intra-set edge filtering.
        let ulid_set: Vec<String> = tickets.iter().map(|t| t.id.clone()).collect();

        // Delegate to TicketSyncer which owns the ticket_dependencies table.
        // Returns (from_ticket_id, to_ticket_id) pairs where both endpoints are
        // in the set and dep_type = 'blocks'.
        let edges = syncer.get_blocks_edges_within_set(&ulid_set)?;

        // Cycle detection — fail early with a clear message.
        if let Some(cycle) = crate::graph::detect_cycles(&ulid_set, &edges) {
            // Map ULIDs back to source_ids for a human-readable error.
            let id_map: HashMap<&str, &str> = tickets
                .iter()
                .map(|t| (t.id.as_str(), t.source_id.as_str()))
                .collect();
            let cycle_display: Vec<&str> = cycle
                .iter()
                .map(|id| id_map.get(id.as_str()).copied().unwrap_or(id.as_str()))
                .collect();
            return Err(ConductorError::InvalidInput(format!(
                "ticket dependency cycle detected: {}",
                cycle_display.join(" → ")
            )));
        }

        // Topological sort — dependencies first.
        let sorted_ids = crate::graph::topological_sort(&ulid_set, &edges);

        // Build a lookup map: ticket ULID → Ticket.
        let ticket_map: HashMap<&str, &crate::tickets::Ticket> =
            tickets.iter().map(|t| (t.id.as_str(), t)).collect();

        // Build blocker map: ticket ULID → Vec<blocker ULIDs in set>
        // edge = (blocker, dependent), so for each edge (from, to): to's blocker is from.
        let mut blockers_map: HashMap<&str, Vec<&str>> = HashMap::new();
        for id in &ulid_set {
            blockers_map.entry(id.as_str()).or_default();
        }
        for (from, to) in &edges {
            blockers_map
                .entry(to.as_str())
                .or_default()
                .push(from.as_str());
        }

        // Pre-validate: fail before any git I/O if any ticket has multiple in-set blockers.
        for (ticket_ulid, blockers) in &blockers_map {
            if blockers.len() > 1 {
                let ticket_src = ticket_map
                    .get(*ticket_ulid)
                    .map(|t| t.source_id.as_str())
                    .unwrap_or(*ticket_ulid);
                let blocker_src: Vec<&str> = blockers
                    .iter()
                    .filter_map(|id| ticket_map.get(*id).map(|t| t.source_id.as_str()))
                    .collect();
                return Err(ConductorError::InvalidInput(format!(
                    "ticket {} has multiple in-set blockers ({}); ambiguous branch stacking",
                    ticket_src,
                    blocker_src.join(", ")
                )));
            }
        }

        // Create worktrees in topo order, tracking created branches by ticket ULID.
        let mut created_branches: HashMap<String, String> = HashMap::new();
        let mut results: Vec<(Worktree, Vec<String>)> = Vec::with_capacity(sorted_ids.len());

        for ticket_ulid in &sorted_ids {
            let ticket = match ticket_map.get(ticket_ulid.as_str()) {
                Some(t) => t,
                None => {
                    return Err(ConductorError::TicketNotFound {
                        id: ticket_ulid.clone(),
                    });
                }
            };

            let wt_name =
                crate::text_util::worktree_name_for_ticket(&ticket.source_id, &ticket.title);

            // Determine from_branch: single in-set blocker → its branch; no blocker → root_branch.
            let in_set_blockers = blockers_map
                .get(ticket_ulid.as_str())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let from_branch = match in_set_blockers {
                [] => root_branch.to_string(),
                [blocker_id] => match created_branches.get(*blocker_id) {
                    Some(branch) => branch.clone(),
                    None => {
                        return Err(ConductorError::InvalidInput(format!(
                            "ticket {}: blocker {} has no created worktree (topo sort error)",
                            ticket.source_id, blocker_id
                        )));
                    }
                },
                _ => unreachable!("pre-validation above ensures at most one in-set blocker"),
            };

            let opts = WorktreeCreateOptions {
                from_branch: Some(from_branch),
                ticket_id: Some(ticket.id.clone()),
                ..Default::default()
            };
            let (wt, warnings) = self.create(repo_slug, &wt_name, opts)?;
            created_branches.insert(ticket_ulid.clone(), wt.branch.clone());
            results.push((wt, warnings));
        }

        Ok(results)
    }

    pub fn get_by_id(&self, id: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = :id"),
                named_params![":id": id],
                map_worktree_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// Fetch a worktree by ID, returning `WorktreeNotFound` if it does not exist
    /// or does not belong to `repo_id`.
    pub fn get_by_id_for_repo(&self, id: &str, repo_id: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = :id AND repo_id = :repo_id"),
                named_params![":id": id, ":repo_id": repo_id],
                map_worktree_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// Fetch multiple worktrees by their IDs in a single query.
    /// Returns an empty Vec when `ids` is empty (avoids a syntax-error `IN ()` clause).
    pub fn get_by_ids(&self, ids: &[&str]) -> Result<Vec<Worktree>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = crate::db::sql_placeholders(ids.len());
        let sql = format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id IN ({placeholders})");
        query_collect(
            self.conn,
            &sql,
            rusqlite::params_from_iter(ids.iter()),
            map_worktree_row,
        )
    }

    pub fn get_by_slug(&self, repo_id: &str, slug: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = :repo_id AND slug = :slug"
                ),
                named_params![":repo_id": repo_id, ":slug": slug],
                map_worktree_row,
            )
            .map_err(worktree_not_found(slug))
    }

    pub fn get_by_branch(&self, repo_id: &str, branch: &str) -> Result<Worktree> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = :repo_id AND branch = :branch"
                ),
                named_params![":repo_id": repo_id, ":branch": branch],
                map_worktree_row,
            )
            .map_err(worktree_not_found(branch))
    }

    /// Try to resolve a worktree by slug first, then by branch name.
    /// If neither matches, returns a "did you mean" error listing available slugs.
    pub fn get_by_slug_or_branch(&self, repo_id: &str, slug_or_branch: &str) -> Result<Worktree> {
        match self.get_by_slug(repo_id, slug_or_branch) {
            Ok(wt) => return Ok(wt),
            Err(ConductorError::WorktreeNotFound { .. }) => {}
            Err(e) => return Err(e),
        }

        match self.get_by_branch(repo_id, slug_or_branch) {
            Ok(wt) => return Ok(wt),
            Err(ConductorError::WorktreeNotFound { .. }) => {}
            Err(e) => return Err(e),
        }

        // Neither slug nor branch matched — build a "did you mean" error.
        let available = self.list_by_repo_id(repo_id, false).unwrap_or_default();
        let suggestions: Vec<&str> = available.iter().take(5).map(|w| w.slug.as_str()).collect();
        let hint = if suggestions.is_empty() {
            String::new()
        } else {
            format!(" — did you mean one of: {}", suggestions.join(", "))
        };
        Err(ConductorError::WorktreeNotFound {
            slug: format!("{slug_or_branch}{hint}"),
        })
    }

    pub fn list_by_ticket(&self, ticket_id: &str) -> Result<Vec<Worktree>> {
        query_collect(
            self.conn,
            &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE ticket_id = :ticket_id ORDER BY created_at DESC"),
            named_params![":ticket_id": ticket_id],
            map_worktree_row,
        )
    }

    pub fn list_by_repo_id(&self, repo_id: &str, active_only: bool) -> Result<Vec<Worktree>> {
        let status_filter = if active_only {
            " AND status = 'active'"
        } else {
            ""
        };
        let query = format!(
            "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = :repo_id{} ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
            status_filter
        );
        query_collect(
            self.conn,
            &query,
            named_params![":repo_id": repo_id],
            map_worktree_row,
        )
    }

    pub fn list_by_repo_id_and_base_branch(
        &self,
        repo_id: &str,
        base_branch: &str,
    ) -> Result<Vec<Worktree>> {
        let query = format!(
            "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = :repo_id AND base_branch = :base_branch AND status = 'active' ORDER BY created_at"
        );
        query_collect(
            self.conn,
            &query,
            named_params![":repo_id": repo_id, ":base_branch": base_branch],
            map_worktree_row,
        )
    }

    /// Shared query builder for [`list`] and [`list_paginated`].
    ///
    /// `pagination` is `Some((limit, offset))` to add `LIMIT ?N OFFSET ?M`; `None` for unbounded.
    fn list_inner(
        &self,
        repo_slug: Option<&str>,
        active_only: bool,
        pagination: Option<(usize, usize)>,
    ) -> Result<Vec<Worktree>> {
        let status_filter = if active_only {
            " AND status = 'active'"
        } else {
            ""
        };

        let base_query = match repo_slug {
            Some(_) => format!(
                "SELECT {} FROM worktrees w JOIN repos r ON r.id = w.repo_id WHERE r.slug = :slug{} ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
                &*WORKTREE_COLUMNS_W,
                status_filter,
            ),
            None => format!(
                "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE 1=1{} ORDER BY CASE WHEN status = 'active' THEN 0 ELSE 1 END, created_at",
                status_filter,
            ),
        };

        match (repo_slug, pagination) {
            (Some(slug), Some((limit, offset))) => {
                let query = format!("{base_query} LIMIT :limit OFFSET :offset");
                query_collect(
                    self.conn,
                    &query,
                    named_params![":slug": slug, ":limit": limit as i64, ":offset": offset as i64],
                    map_worktree_row,
                )
            }
            (Some(slug), None) => query_collect(
                self.conn,
                &base_query,
                named_params![":slug": slug],
                map_worktree_row,
            ),
            (None, Some((limit, offset))) => {
                let query = format!("{base_query} LIMIT :limit OFFSET :offset");
                query_collect(
                    self.conn,
                    &query,
                    named_params![":limit": limit as i64, ":offset": offset as i64],
                    map_worktree_row,
                )
            }
            (None, None) => query_collect(self.conn, &base_query, [], map_worktree_row),
        }
    }

    pub fn list(&self, repo_slug: Option<&str>, active_only: bool) -> Result<Vec<Worktree>> {
        self.list_inner(repo_slug, active_only, None)
    }

    pub fn list_paginated(
        &self,
        repo_slug: Option<&str>,
        active_only: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Worktree>> {
        self.list_inner(repo_slug, active_only, Some((limit, offset)))
    }

    /// List all worktrees joined with the status of each worktree's latest agent run.
    ///
    /// Uses a LEFT JOIN so worktrees with no agent runs still appear (with `agent_status = None`).
    /// Uses the INNER JOIN subquery pattern to avoid duplicate rows when two runs share
    /// the same MAX(started_at) timestamp.
    pub fn list_all_with_status(&self, active_only: bool) -> Result<Vec<WorktreeWithStatus>> {
        let status_filter = if active_only {
            " AND w.status = 'active'"
        } else {
            ""
        };
        let sql = format!(
            "{base} \
             WHERE 1=1{status_filter} \
             ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
            base = enriched_worktree_base(),
            status_filter = status_filter,
        );
        query_collect(self.conn, &sql, [], map_enriched_row)
    }

    /// Fetch a worktree by ID, returning a `WorktreeWithStatus` with ticket info populated.
    pub fn get_by_id_enriched(&self, id: &str) -> Result<WorktreeWithStatus> {
        self.conn
            .query_row(
                &format!("{base} WHERE w.id = :id", base = enriched_worktree_base()),
                named_params![":id": id],
                map_enriched_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// Fetch a worktree by ID and repo, returning a `WorktreeWithStatus` with ticket info.
    /// Returns `WorktreeNotFound` if the worktree does not exist or belongs to a different repo.
    pub fn get_by_id_for_repo_enriched(
        &self,
        id: &str,
        repo_id: &str,
    ) -> Result<WorktreeWithStatus> {
        self.conn
            .query_row(
                &format!(
                    "{base} WHERE w.id = :id AND w.repo_id = :repo_id",
                    base = enriched_worktree_base(),
                ),
                named_params![":id": id, ":repo_id": repo_id],
                map_enriched_row,
            )
            .map_err(worktree_not_found(id))
    }

    /// List worktrees for a repo with ticket info populated.
    /// Does not modify `list_by_repo_id` to avoid breaking internal callers.
    pub fn list_by_repo_id_enriched(
        &self,
        repo_id: &str,
        active_only: bool,
    ) -> Result<Vec<WorktreeWithStatus>> {
        let status_filter = if active_only {
            " AND w.status = 'active'"
        } else {
            ""
        };
        let sql = format!(
            "{base} \
             WHERE w.repo_id = :repo_id{status_filter} \
             ORDER BY CASE WHEN w.status = 'active' THEN 0 ELSE 1 END, w.created_at",
            base = enriched_worktree_base(),
            status_filter = status_filter,
        );
        query_collect(
            self.conn,
            &sql,
            named_params![":repo_id": repo_id],
            map_enriched_row,
        )
    }

    /// Walk up from `cwd` and return the worktree whose `path` is a prefix of (or equals) `cwd`.
    ///
    /// When multiple worktrees match (nested paths), the one with the longest path wins,
    /// ensuring the most-specific worktree is returned.
    ///
    /// Returns `None` when no registered worktree matches.
    pub fn find_by_cwd(&self, cwd: &Path) -> Result<Option<Worktree>> {
        let worktrees = self.list(None, false)?;
        let found = worktrees
            .into_iter()
            .filter(|wt| cwd.starts_with(Path::new(&wt.path)))
            .max_by_key(|wt| wt.path.len());
        Ok(found)
    }

    pub fn delete(&self, repo_slug: &str, name: &str) -> Result<Worktree> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let worktree = self
            .conn
            .query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE repo_id = :repo_id AND slug = :slug"
                ),
                named_params![":repo_id": repo.id, ":slug": name],
                map_worktree_row,
            )
            .map_err(worktree_not_found(name))?;

        self.delete_internal(&repo, worktree, None)
    }

    pub fn delete_by_id(&self, worktree_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id(worktree_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree, None)
    }

    /// Delete a worktree by ID, enforcing that it belongs to `repo_id`.
    /// Returns `WorktreeNotFound` if the worktree does not exist or belongs to a different repo.
    pub fn delete_by_id_for_repo(&self, id: &str, repo_id: &str) -> Result<Worktree> {
        let worktree = self.get_by_id_for_repo(id, repo_id)?;
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_id(&worktree.repo_id)?;
        self.delete_internal(&repo, worktree, None)
    }

    /// `ticket_closed_hint`: when `Some(true)` the caller already knows the
    /// linked ticket is closed; the per-ticket DB query is skipped.
    fn delete_internal(
        &self,
        repo: &crate::repo::Repo,
        worktree: Worktree,
        ticket_closed_hint: Option<bool>,
    ) -> Result<Worktree> {
        // Determine merged vs abandoned:
        // 1. Check if the linked ticket is closed (covers squash merges that git can't detect)
        // 2. Fall back to git branch --merged (covers cases without a linked ticket)
        let ticket_closed = ticket_closed_hint.unwrap_or_else(|| {
            worktree
                .ticket_id
                .as_ref()
                .map(|tid| {
                    self.conn
                        .query_row(
                            "SELECT state = 'closed' FROM tickets WHERE id = :id",
                            named_params![":id": tid],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        });
        let is_merged = ticket_closed
            || crate::git::is_branch_merged_local(
                &repo.local_path,
                &worktree.branch,
                &repo.default_branch,
            );
        let new_status = if is_merged {
            WorktreeStatus::Merged
        } else {
            WorktreeStatus::Abandoned
        };
        let now = Utc::now().to_rfc3339();

        remove_git_artifacts(&repo.local_path, &worktree.path, &worktree.branch);

        // Soft-delete: update status + completed_at instead of deleting the row
        self.conn.execute(
            "UPDATE worktrees SET status = :status, completed_at = :completed_at WHERE id = :id",
            named_params![":status": new_status.as_str(), ":completed_at": now, ":id": worktree.id],
        )?;

        let deleted_wt = Worktree {
            status: new_status,
            completed_at: Some(now),
            ..worktree
        };

        Ok(deleted_wt)
    }

    /// Remove the git worktree directory and delete the associated branch (best-effort).
    /// Failures are logged but not propagated. Delegates to the module-private
    /// `remove_git_artifacts` to keep the implementation detail encapsulated.
    ///
    /// NOTE: This is a static method by design — it's a cross-manager utility called from
    /// TicketSyncer that doesn't require WorktreeManager instance state (db connection, config).
    /// Making it an instance method would create circular dependency issues.
    pub fn remove_artifacts(repo_path: &str, worktree_path: &str, branch: &str) {
        remove_git_artifacts(repo_path, worktree_path, branch);
    }

    pub fn update_status(&self, worktree_id: &str, status: WorktreeStatus) -> Result<()> {
        let completed_at = if status != WorktreeStatus::Active {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE worktrees SET status = :status, completed_at = :completed_at WHERE id = :id",
            named_params![":status": status.as_str(), ":completed_at": completed_at, ":id": worktree_id],
        )?;
        Ok(())
    }

    /// Set (or clear) the per-worktree default model.
    /// Pass `None` to clear the override and fall back to the global config.
    pub fn set_model(&self, repo_slug: &str, name: &str, model: Option<&str>) -> Result<()> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;
        let updated = self.conn.execute(
            "UPDATE worktrees SET model = :model WHERE repo_id = :repo_id AND slug = :slug",
            named_params![":model": model, ":repo_id": repo.id, ":slug": name],
        )?;
        if updated == 0 {
            return Err(ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            });
        }
        Ok(())
    }

    /// Set (or clear) the worktree's base branch.
    /// Pass `None` to reset to the repo default branch (skips git validation).
    /// When `opts.rebase` is false and the new base is not an ancestor of HEAD, returns an error.
    /// When `opts.rebase` is true, rebases the worktree branch onto the new base (blocked if dirty).
    pub fn set_base_branch(
        &self,
        repo_slug: &str,
        name: &str,
        base_branch: Option<&str>,
        opts: SetBaseBranchOptions,
    ) -> Result<()> {
        if let Some(new_base) = base_branch {
            if new_base.starts_with('-') {
                return Err(ConductorError::InvalidInput(format!(
                    "Invalid branch name '{new_base}': branch names must not start with '-'"
                )));
            }
        }

        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        if let Some(new_base) = base_branch {
            let worktree = self.get_by_slug(&repo.id, name)?;
            let wt_path = std::path::Path::new(&worktree.path);

            // Fetch the remote ref so the ancestor check is current.
            let _ = Command::new("git")
                .args(["fetch", "origin", "--", new_base])
                .current_dir(wt_path)
                .status();

            let base_ref = format!("origin/{new_base}");
            if !Self::is_ancestor(wt_path, &base_ref)? {
                if !opts.rebase {
                    return Err(ConductorError::InvalidInput(format!(
                        "'{new_base}' is not an ancestor of the worktree HEAD. \
                         The branch was forked from a different base. \
                         Rebase the worktree onto the new base before updating the recorded base branch."
                    )));
                }

                // Dirty check before rebase.
                let status_out = check_output(
                    Command::new("git")
                        .args(["status", "--porcelain"])
                        .current_dir(wt_path),
                )?;
                if !String::from_utf8_lossy(&status_out.stdout)
                    .trim()
                    .is_empty()
                {
                    return Err(ConductorError::InvalidInput(
                        "Worktree has uncommitted changes. Stash or commit them before rebasing."
                            .into(),
                    ));
                }

                check_output(
                    Command::new("git")
                        .args(["rebase", &base_ref])
                        .current_dir(wt_path),
                )?;
            }
        }

        let updated = self.conn.execute(
            "UPDATE worktrees SET base_branch = :base_branch WHERE repo_id = :repo_id AND slug = :slug",
            named_params![":base_branch": base_branch, ":repo_id": repo.id, ":slug": name],
        )?;
        if updated == 0 {
            return Err(ConductorError::WorktreeNotFound {
                slug: name.to_string(),
            });
        }
        Ok(())
    }

    /// Returns true if `base_ref` is an ancestor of HEAD in the given worktree directory.
    fn is_ancestor(wt_path: &std::path::Path, base_ref: &str) -> Result<bool> {
        let status = Command::new("git")
            .args(["merge-base", "--is-ancestor", base_ref, "HEAD"])
            .current_dir(wt_path)
            .status()
            .map_err(|e| {
                ConductorError::Git(crate::error::SubprocessFailure::from_message(
                    "git merge-base",
                    format!("failed to spawn: {e}"),
                ))
            })?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            Some(code) => Err(ConductorError::Git(crate::error::SubprocessFailure {
                command: "git merge-base --is-ancestor".into(),
                exit_code: Some(code),
                stderr: format!("unexpected exit code {code} for ref '{base_ref}'"),
                stdout: String::new(),
            })),
            None => Err(ConductorError::Git(
                crate::error::SubprocessFailure::from_message(
                    "git merge-base --is-ancestor",
                    "terminated by signal".into(),
                ),
            )),
        }
    }

    /// Push the worktree branch to origin.
    pub fn push(&self, repo_slug: &str, name: &str) -> Result<String> {
        let (_repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        check_output(git_in(&worktree.path).args(["push", "-u", "origin", &worktree.branch]))?;

        Ok(format!(
            "Pushed {} to origin/{}",
            worktree.slug, worktree.branch
        ))
    }

    /// Create a pull request for the worktree branch using `gh`.
    pub fn create_pr(&self, repo_slug: &str, name: &str, draft: bool) -> Result<String> {
        let (repo, worktree) = self.get_active_worktree(repo_slug, name)?;

        let base = worktree.effective_base(&repo.default_branch);
        let mut args = vec![
            "pr",
            "create",
            "--fill",
            "--head",
            &worktree.branch,
            "--base",
            base,
        ];
        if draft {
            args.push("--draft");
        }

        let output = check_gh_output(Command::new("gh").args(&args).current_dir(&worktree.path))?;

        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(url)
    }

    /// Look up a repo and its active worktree by slugs.
    fn get_active_worktree(
        &self,
        repo_slug: &str,
        wt_slug: &str,
    ) -> Result<(crate::repo::Repo, Worktree)> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let worktree = self.get_by_slug(&repo.id, wt_slug)?;

        if !worktree.is_active() {
            return Err(ConductorError::InvalidInput(format!(
                "worktree '{}' is not active (status: {})",
                wt_slug, worktree.status
            )));
        }

        Ok((repo, worktree))
    }

    /// Reap stale worktrees whose status is `merged` or `abandoned` but whose
    /// filesystem artifacts still exist. For each stale worktree:
    /// 1. Remove git worktree directory and branch (best-effort)
    /// 2. Run `git worktree prune` on the parent repo
    /// 3. Backfill `completed_at` if NULL
    ///
    /// Returns the number of worktrees cleaned up.
    pub fn reap_stale_worktrees(&self) -> Result<usize> {
        let stale: Vec<(String, String, String, String, Option<String>)> = query_collect(
            self.conn,
            "SELECT w.id, r.local_path, w.path, w.branch, w.completed_at
             FROM worktrees w
             JOIN repos r ON r.id = w.repo_id
             WHERE w.status IN ('merged', 'abandoned')",
            [],
            |row| {
                Ok((
                    row.get("id")?,
                    row.get("local_path")?,
                    row.get("path")?,
                    row.get("branch")?,
                    row.get("completed_at")?,
                ))
            },
        )?;

        let mut reaped = 0;
        let mut pruned_repos = std::collections::HashSet::new();

        for (wt_id, repo_path, wt_path, branch, completed_at) in &stale {
            if !Path::new(wt_path).exists() {
                // Backfill completed_at if missing even when path is already gone
                if completed_at.is_none() {
                    self.backfill_completed_at(wt_id)?;
                    reaped += 1;
                }
                continue;
            }

            remove_git_artifacts(repo_path, wt_path, branch);
            pruned_repos.insert(repo_path.clone());

            // Backfill completed_at if NULL
            if completed_at.is_none() {
                self.backfill_completed_at(wt_id)?;
            }

            reaped += 1;
        }

        // Run git worktree prune on each affected repo
        for repo_path in &pruned_repos {
            let _ = git_in(repo_path).args(["worktree", "prune"]).output();
        }

        Ok(reaped)
    }

    fn backfill_completed_at(&self, wt_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE worktrees SET completed_at = :now WHERE id = :id AND completed_at IS NULL",
            named_params! { ":now": now, ":id": wt_id },
        )?;
        Ok(())
    }

    /// Permanently delete completed (merged/abandoned) worktree records from the database.
    pub fn purge(&self, repo_slug: &str, name: Option<&str>) -> Result<usize> {
        let repo_mgr = RepoManager::new(self.conn, self.config);
        let repo = repo_mgr.get_by_slug(repo_slug)?;

        let count = if let Some(slug) = name {
            self.conn.execute(
                "DELETE FROM worktrees WHERE repo_id = :repo_id AND slug = :slug AND status != 'active'",
                named_params! { ":repo_id": repo.id, ":slug": slug },
            )?
        } else {
            self.conn.execute(
                "DELETE FROM worktrees WHERE repo_id = :repo_id AND status != 'active'",
                named_params! { ":repo_id": repo.id },
            )?
        };

        Ok(count)
    }

    /// Scan all active worktrees for merged PRs and clean them up.
    ///
    /// For each active worktree whose PR has been merged:
    /// 1. Mark status as `merged` with `completed_at`
    /// 2. Remove local git artifacts (worktree dir + local branch)
    /// 3. Delete the remote branch (best-effort)
    /// 4. Auto-close orphaned features
    ///
    /// When `repo_slug` is `Some`, only worktrees for that repo are checked.
    /// Returns the number of worktrees cleaned up.
    pub fn cleanup_merged_worktrees(&self, repo_slug: Option<&str>) -> Result<usize> {
        self.cleanup_merged_worktrees_with_merge_check(
            repo_slug,
            crate::github::merged_branches_for_repo,
            pull_ff_only,
        )
    }

    pub(crate) fn cleanup_merged_worktrees_with_merge_check(
        &self,
        repo_slug: Option<&str>,
        // Returns branch → mergedAt (ISO 8601). Empty string means "unknown time, always clean up".
        merge_check: impl Fn(&str, &[String]) -> std::collections::HashMap<String, String>,
        pull_fn: impl Fn(&str, &str) -> std::result::Result<(), String>,
    ) -> Result<usize> {
        let base_query =
            "SELECT w.id, w.branch, w.path, r.local_path, r.remote_url, w.repo_id, w.base_branch, w.created_at
                 FROM worktrees w
                 JOIN repos r ON r.id = w.repo_id
                 WHERE w.status = 'active'";
        let query = match repo_slug {
            Some(_) => format!("{base_query} AND r.slug = :slug"),
            None => base_query.to_string(),
        };

        let mapper = |row: &rusqlite::Row| -> rusqlite::Result<[String; 8]> {
            Ok([
                row.get("id")?,
                row.get("branch")?,
                row.get("path")?,
                row.get("local_path")?,
                row.get("remote_url")?,
                row.get("repo_id")?,
                row.get::<_, Option<String>>("base_branch")?
                    .unwrap_or_default(),
                row.get::<_, Option<String>>("created_at")?
                    .unwrap_or_default(),
            ])
        };
        let rows: Vec<[String; 8]> = match repo_slug {
            Some(slug) => {
                query_collect(self.conn, &query, named_params! { ":slug": slug }, mapper)?
            }
            None => query_collect(self.conn, &query, [], mapper)?,
        };

        // Group branches by remote_url and batch-check merged status per repo.
        let mut branches_by_remote: std::collections::HashMap<&str, Vec<String>> =
            std::collections::HashMap::new();
        for row in &rows {
            branches_by_remote
                .entry(row[4].as_str())
                .or_default()
                .push(row[1].clone());
        }
        // branch → mergedAt (ISO 8601); empty string = unknown, always clean up.
        let mut merged_branches: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (remote_url, branches) in &branches_by_remote {
            merged_branches.extend(merge_check(remote_url, branches));
        }

        let now = Utc::now().to_rfc3339();
        let mut cleaned = 0usize;
        let mut pruned_repos: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // Track (repo_id, base_branch) pairs already pulled to avoid redundant subprocesses
        let mut pulled_bases: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();

        for row in &rows {
            let [wt_id, branch, wt_path, repo_path, _remote_url, repo_id, base_branch, wt_created_at] =
                row;
            let Some(merged_at) = merged_branches.get(branch) else {
                continue;
            };

            // If the worktree was created AFTER the PR was merged, the branch name is being
            // reused for a new worktree — do not clean it up.
            if !merged_at.is_empty()
                && !wt_created_at.is_empty()
                && wt_created_at.as_str() > merged_at.as_str()
            {
                tracing::debug!(
                    worktree = %wt_id,
                    branch = %branch,
                    wt_created_at = %wt_created_at,
                    merged_at = %merged_at,
                    "skipping cleanup: worktree was created after PR was merged (branch reuse)"
                );
                continue;
            }

            tracing::info!(
                worktree = %wt_id,
                branch = %branch,
                "merged PR detected — cleaning up worktree"
            );

            // Mark as merged
            self.conn.execute(
                "UPDATE worktrees SET status = 'merged', completed_at = :now WHERE id = :id",
                named_params! { ":now": now, ":id": wt_id },
            )?;

            // Remove local git artifacts
            remove_git_artifacts(repo_path, wt_path, branch);

            // Delete remote branch (best-effort)
            delete_remote_branch(repo_path, branch);

            pruned_repos.insert(repo_path.as_str());

            // Auto-pull base branch worktree if tracked and active
            let pull_key = (repo_id.clone(), base_branch.clone());
            if !base_branch.is_empty() && !pulled_bases.contains(&pull_key) {
                match self.get_by_branch(repo_id, base_branch) {
                    Ok(base_wt) if base_wt.status == WorktreeStatus::Active => {
                        if !base_wt.path.is_empty() {
                            if let Err(e) = pull_fn(&base_wt.path, base_branch) {
                                tracing::warn!(
                                    base_branch = %base_branch,
                                    error = %e,
                                    "auto-pull of base branch failed after sub-PR merge"
                                );
                            }
                        }
                        pulled_bases.insert(pull_key);
                    }
                    Ok(_) => {
                        // base worktree exists but is not active — skip, but record to avoid
                        // repeated DB lookups if other sub-PRs target the same base
                        pulled_bases.insert(pull_key);
                    }
                    Err(ConductorError::WorktreeNotFound { .. }) => {} // not tracked — skip
                    Err(e) => {
                        tracing::warn!(error = %e, base_branch = %base_branch, "failed to look up base branch worktree");
                    }
                }
            }

            cleaned += 1;
        }

        // Run git worktree prune once per unique repo path
        for repo_path in &pruned_repos {
            let _ = git_in(repo_path).args(["worktree", "prune"]).output();
        }

        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::create_test_conn;

    fn insert_ticket(conn: &Connection, id: &str, repo_id: &str, source_id: &str, raw_json: &str) {
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES (:id, :repo_id, 'vantage', :source_id, 'Test', '', 'open', '[]', '', '2024-01-01T00:00:00Z', :raw_json)",
            rusqlite::named_params! { ":id": id, ":repo_id": repo_id, ":source_id": source_id, ":raw_json": raw_json },
        ).unwrap();
    }

    fn insert_worktree_with_ticket(
        conn: &Connection,
        id: &str,
        repo_id: &str,
        ticket_id: &str,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, ticket_id, created_at) \
             VALUES (:id, :repo_id, :id, 'feat/dep', '/tmp/dep', :status, :ticket_id, '2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":id": id, ":repo_id": repo_id, ":status": status, ":ticket_id": ticket_id },
        ).unwrap();
    }

    fn insert_repo(conn: &Connection) {
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r1','test-repo','/tmp/repo','https://github.com/x/y.git','/tmp/ws','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
    }

    fn insert_wt(conn: &Connection, id: &str, slug: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES (:id, 'r1', :slug, 'feat/test', '/tmp/ws', 'active', :created_at)",
            rusqlite::named_params! { ":id": id, ":slug": slug, ":created_at": created_at },
        )
        .unwrap();
    }

    #[test]
    fn list_pagination_limit_truncates_results() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr.list_paginated(Some("test-repo"), false, 2, 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn list_pagination_offset_skips_results() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        // Offset 2 should return only the last row (ordered by active-first, then created_at)
        let results = mgr.list_paginated(Some("test-repo"), false, 10, 2).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn list_pagination_limit_offset_second_page() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");
        insert_wt(&conn, "wt4", "slug-d", "2024-01-04T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let page1 = mgr.list_paginated(Some("test-repo"), false, 2, 0).unwrap();
        let page2 = mgr.list_paginated(Some("test-repo"), false, 2, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        // Pages must not overlap
        let ids1: Vec<_> = page1.iter().map(|w| w.id.clone()).collect();
        let ids2: Vec<_> = page2.iter().map(|w| w.id.clone()).collect();
        assert!(ids1.iter().all(|id| !ids2.contains(id)));
    }

    #[test]
    fn list_no_pagination_returns_all() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr.list(Some("test-repo"), false).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn list_pagination_no_repo_slug_uses_all_repos() {
        let conn = crate::test_helpers::create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "slug-a", "2024-01-01T00:00:00Z");
        insert_wt(&conn, "wt2", "slug-b", "2024-01-02T00:00:00Z");
        insert_wt(&conn, "wt3", "slug-c", "2024-01-03T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr.list_paginated(None, false, 2, 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn resolve_parent_branch_returns_none_for_non_vantage_ticket() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES ('t1', 'r1', 'github', '42', 'Issue', '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            [],
        ).unwrap();
        assert!(resolve_parent_branch(&conn, "t1", "r1").is_none());
    }

    #[test]
    fn resolve_parent_branch_returns_none_when_no_dependencies() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        insert_ticket(&conn, "t1", "r1", "D-001", r#"{"id":"D-001"}"#);
        assert!(resolve_parent_branch(&conn, "t1", "r1").is_none());
    }

    #[test]
    fn resolve_parent_branch_finds_active_parent_worktree() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        // Parent ticket (dep D-000) with an active worktree
        insert_ticket(&conn, "parent", "r1", "D-000", r#"{"id":"D-000"}"#);
        insert_worktree_with_ticket(&conn, "wt-parent", "r1", "parent", "active");
        // Child ticket depending on D-000
        insert_ticket(
            &conn,
            "child",
            "r1",
            "D-001",
            r#"{"id":"D-001","dependencies":["D-000"]}"#,
        );
        let branch = resolve_parent_branch(&conn, "child", "r1");
        assert_eq!(branch, Some("feat/dep".to_string()));
    }

    #[test]
    fn resolve_parent_branch_returns_none_when_parent_worktree_not_active() {
        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) VALUES ('r1','repo','/p','u','/w','2024-01-01T00:00:00Z')",
            [],
        ).unwrap();
        insert_ticket(&conn, "parent", "r1", "D-000", r#"{"id":"D-000"}"#);
        insert_worktree_with_ticket(&conn, "wt-parent", "r1", "parent", "merged");
        insert_ticket(
            &conn,
            "child",
            "r1",
            "D-001",
            r#"{"id":"D-001","dependencies":["D-000"]}"#,
        );
        assert!(resolve_parent_branch(&conn, "child", "r1").is_none());
    }

    // ── cleanup_merged_worktrees pull_fn tests ──────────────────────────────

    /// Returns a merge_check closure that marks only "feat/sub-task" as merged.
    fn merged_sub_task_check(
    ) -> impl Fn(&str, &[String]) -> std::collections::HashMap<String, String> {
        |_remote_url: &str, _branches: &[String]| {
            let mut map = std::collections::HashMap::new();
            map.insert("feat/sub-task".to_string(), String::new());
            map
        }
    }

    fn insert_worktree_with_base(
        conn: &Connection,
        id: &str,
        branch: &str,
        path: &str,
        status: &str,
        base_branch: &str,
    ) {
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, base_branch, created_at) \
             VALUES (:id, 'r1', :id, :branch, :path, :status, :base_branch, '2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":id": id, ":branch": branch, ":path": path, ":status": status, ":base_branch": base_branch },
        ).unwrap();
    }

    #[test]
    fn test_cleanup_pulls_base_branch_worktree() {
        use std::sync::{Arc, Mutex};

        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree (will be detected as merged)
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/epic",
        );

        // Base worktree (active, should be pulled)
        insert_worktree_with_base(&conn, "wt-base", "feat/epic", "/tmp/epic", "active", "");

        let pulled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let pulled_clone = pulled.clone();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            move |path, branch| {
                pulled_clone
                    .lock()
                    .unwrap()
                    .push((path.to_string(), branch.to_string()));
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        let calls = pulled.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "/tmp/epic");
        assert_eq!(calls[0].1, "feat/epic");
    }

    #[test]
    fn test_cleanup_skips_pull_when_base_not_tracked() {
        use std::sync::{Arc, Mutex};

        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree with a base_branch that has no worktree entry in DB
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/untracked-epic",
        );

        let pulled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let pulled_clone = pulled.clone();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            move |path, branch| {
                pulled_clone
                    .lock()
                    .unwrap()
                    .push((path.to_string(), branch.to_string()));
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // pull_fn must never have been called
        assert!(pulled.lock().unwrap().is_empty());
    }

    #[test]
    fn test_cleanup_pull_failure_does_not_block() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/epic",
        );

        // Base worktree (active)
        insert_worktree_with_base(&conn, "wt-base", "feat/epic", "/tmp/epic", "active", "");

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            |_path, _branch| Err("simulated pull failure".to_string()),
        );

        // Cleanup must still succeed and report 1 worktree cleaned
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // Sub worktree must be marked merged
        let status: String = conn
            .query_row(
                "SELECT status FROM worktrees WHERE id = 'wt-sub'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "merged");
    }

    #[test]
    fn test_cleanup_skips_pull_when_base_worktree_not_active() {
        // Verifies the Ok(_) arm: base worktree is in DB but not active → pull_fn must not fire.
        use std::sync::{Arc, Mutex};

        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);

        // Sub-PR worktree targeting a base that exists but is already merged
        insert_worktree_with_base(
            &conn,
            "wt-sub",
            "feat/sub-task",
            "/tmp/sub",
            "active",
            "feat/epic",
        );

        // Base worktree present in DB but status = merged (not active)
        insert_worktree_with_base(&conn, "wt-base", "feat/epic", "/tmp/epic", "merged", "");

        let pulled: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let pulled_clone = pulled.clone();

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.cleanup_merged_worktrees_with_merge_check(
            Some("test-repo"),
            merged_sub_task_check(),
            move |path, branch| {
                pulled_clone
                    .lock()
                    .unwrap()
                    .push((path.to_string(), branch.to_string()));
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // pull_fn must never have been called — base worktree is not active
        assert!(pulled.lock().unwrap().is_empty());
    }

    // ── create_from_dep_graph unit tests ───────────────────────────────────

    fn insert_ticket_full(
        conn: &Connection,
        id: &str,
        repo_id: &str,
        source_id: &str,
        title: &str,
    ) {
        conn.execute(
            "INSERT INTO tickets (id, repo_id, source_type, source_id, title, body, state, labels, url, synced_at, raw_json) \
             VALUES (:id, :repo_id, 'github', :source_id, :title, '', 'open', '[]', '', '2024-01-01T00:00:00Z', '{}')",
            rusqlite::named_params! { ":id": id, ":repo_id": repo_id, ":source_id": source_id, ":title": title },
        ).unwrap();
    }

    fn insert_dep(conn: &Connection, from_id: &str, to_id: &str) {
        conn.execute(
            "INSERT INTO ticket_dependencies (from_ticket_id, to_ticket_id, dep_type) VALUES (:from_id, :to_id, 'blocks')",
            rusqlite::named_params! { ":from_id": from_id, ":to_id": to_id },
        ).unwrap();
    }

    #[test]
    fn test_create_from_dep_graph_ticket_not_found() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        let mgr = WorktreeManager::new(&conn, &config);

        let result =
            mgr.create_from_dep_graph("test-repo", "main", &["nonexistent-ticket-id".to_string()]);
        assert!(matches!(result, Err(ConductorError::TicketNotFound { .. })));
    }

    #[test]
    fn test_create_from_dep_graph_cycle_detected() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_ticket_full(&conn, "t1", "r1", "101", "feat: ticket 101");
        insert_ticket_full(&conn, "t2", "r1", "102", "feat: ticket 102");
        insert_dep(&conn, "t1", "t2"); // t1 blocks t2
        insert_dep(&conn, "t2", "t1"); // t2 blocks t1 → cycle
        let mgr = WorktreeManager::new(&conn, &config);

        let result =
            mgr.create_from_dep_graph("test-repo", "main", &["t1".to_string(), "t2".to_string()]);
        assert!(
            matches!(result, Err(ConductorError::InvalidInput(ref msg)) if msg.contains("cycle")),
            "expected cycle error, got: {result:?}"
        );
    }

    #[test]
    fn test_create_from_dep_graph_ambiguous_parent() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        // t3 is blocked by both t1 and t2 → ambiguous stacking
        insert_ticket_full(&conn, "t1", "r1", "101", "feat: ticket 101");
        insert_ticket_full(&conn, "t2", "r1", "102", "feat: ticket 102");
        insert_ticket_full(&conn, "t3", "r1", "103", "feat: ticket 103");
        insert_dep(&conn, "t1", "t3");
        insert_dep(&conn, "t2", "t3");
        let mgr = WorktreeManager::new(&conn, &config);

        let result = mgr.create_from_dep_graph(
            "test-repo",
            "main",
            &["t1".to_string(), "t2".to_string(), "t3".to_string()],
        );
        assert!(
            matches!(result, Err(ConductorError::InvalidInput(ref msg)) if msg.contains("ambiguous")),
            "expected ambiguous error, got: {result:?}"
        );
    }

    #[test]
    fn test_create_from_dep_graph_empty_ticket_ids() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        let mgr = WorktreeManager::new(&conn, &config);

        let result = mgr.create_from_dep_graph("test-repo", "main", &[]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_create_from_dep_graph_source_id_fallback() {
        // Passing a source_id ("101") instead of ULID should resolve correctly.
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_ticket_full(&conn, "t1", "r1", "101", "feat: ticket 101");
        let mgr = WorktreeManager::new(&conn, &config);

        // Will fail at git execution, but the ticket lookup must succeed first.
        let result = mgr.create_from_dep_graph("test-repo", "main", &["101".to_string()]);
        // Expect a git error (not a TicketNotFound), confirming lookup succeeded.
        assert!(
            !matches!(result, Err(ConductorError::TicketNotFound { .. })),
            "source_id fallback should resolve ticket 101"
        );
    }

    /// Happy-path integration test: verifies that `create_from_dep_graph` creates
    /// worktrees with the correct branch hierarchy when dependencies are present.
    ///
    /// t1 has no deps → branches from root_branch ("main")
    /// t2 depends on (is blocked by) t1 → branches from t1's worktree branch
    #[test]
    fn test_create_from_dep_graph_happy_path() {
        use std::process::Command;
        use tempfile::TempDir;

        // Set up a real git repo so git branch / worktree commands succeed.
        let repo_dir = TempDir::new().unwrap();
        let ws_dir = TempDir::new().unwrap();
        let repo_path = repo_dir.path().to_str().unwrap().to_string();
        let ws_path = ws_dir.path().to_str().unwrap().to_string();

        // Init repo with an initial commit so "main" branch exists.
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo_path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(&repo_path)
            .output()
            .expect("initial commit");

        let conn = create_test_conn();
        let config = crate::config::Config::default();

        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('repo-hp','hp-repo',:repo_path,'', :ws_path,'2024-01-01T00:00:00Z')",
            rusqlite::named_params! { ":repo_path": repo_path, ":ws_path": ws_path },
        )
        .unwrap();

        insert_ticket_full(&conn, "tp1", "repo-hp", "201", "feat: first ticket");
        insert_ticket_full(&conn, "tp2", "repo-hp", "202", "feat: second ticket");
        insert_dep(&conn, "tp1", "tp2"); // t1 blocks t2

        let mgr = WorktreeManager::new(&conn, &config);
        let results = mgr
            .create_from_dep_graph("hp-repo", "main", &["tp1".to_string(), "tp2".to_string()])
            .expect("create_from_dep_graph should succeed");

        assert_eq!(results.len(), 2, "should create two worktrees");

        let (wt1, _) = results
            .iter()
            .find(|(w, _)| w.branch.contains("201"))
            .unwrap();
        let (wt2, _) = results
            .iter()
            .find(|(w, _)| w.branch.contains("202"))
            .unwrap();

        // t1 has no in-set blockers → base_branch must be root_branch "main"
        assert_eq!(
            wt1.base_branch.as_deref(),
            Some("main"),
            "ticket with no deps should base off root_branch"
        );

        // t2 is blocked by t1 → base_branch must be t1's branch
        assert_eq!(
            wt2.base_branch.as_deref(),
            Some(wt1.branch.as_str()),
            "dependent ticket should base off its blocker's branch"
        );
    }

    /// Verify set_upstream_tracking writes branch.<branch>.remote and branch.<branch>.merge
    /// into the git config, which prevents bare `git push` from landing commits on the wrong branch.
    #[test]
    fn test_set_upstream_tracking_writes_config() {
        use std::process::Command;
        use tempfile::TempDir;

        let repo_dir = TempDir::new().unwrap();
        let repo_path = repo_dir.path();

        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo_path)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(repo_path)
            .output()
            .expect("initial commit");

        set_upstream_tracking(repo_path, "feat/my-branch").unwrap();

        let remote_val = Command::new("git")
            .args(["config", "--get", "branch.feat/my-branch.remote"])
            .current_dir(repo_path)
            .output()
            .expect("git config --get remote");
        assert_eq!(
            String::from_utf8_lossy(&remote_val.stdout).trim(),
            "origin",
            "branch.feat/my-branch.remote should be 'origin'"
        );

        let merge_val = Command::new("git")
            .args(["config", "--get", "branch.feat/my-branch.merge"])
            .current_dir(repo_path)
            .output()
            .expect("git config --get merge");
        assert_eq!(
            String::from_utf8_lossy(&merge_val.stdout).trim(),
            "refs/heads/feat/my-branch",
            "branch.feat/my-branch.merge should be 'refs/heads/feat/my-branch'"
        );
    }

    // ── set_base_branch unit tests ─────────────────────────────────────────

    fn setup_git_repo_with_worktree() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        String,
        String,
        rusqlite::Connection,
    ) {
        use std::process::Command;
        use tempfile::TempDir;

        let repo_dir = TempDir::new().unwrap();
        let ws_dir = TempDir::new().unwrap();
        let repo_path = repo_dir.path().to_str().unwrap().to_string();
        let ws_path = ws_dir.path().to_str().unwrap().to_string();

        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(&repo_path)
            .output()
            .unwrap();

        // Create a feature branch from main (branch only, don't check it out — stay on main so worktree add works)
        Command::new("git")
            .args(["branch", "feat/test"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        // Add a commit on feat/test via the worktree (created below), but first create the worktree
        let wt_path = format!("{ws_path}/feat-test");
        let out = Command::new("git")
            .args(["worktree", "add", &wt_path, "feat/test"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Make a commit in the worktree so feat/test has a commit ahead of main
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "feat"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        let conn = create_test_conn();
        conn.execute(
            "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
             VALUES ('r1','test-repo',:local_path,'https://github.com/x/y.git',:ws,'2024-01-01T00:00:00Z')",
            rusqlite::params![repo_path, ws_path],
        ).unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
             VALUES ('wt1','r1','feat-test','feat/test',:path,'active','2024-01-01T00:00:00Z')",
            rusqlite::params![wt_path],
        )
        .unwrap();

        (repo_dir, ws_dir, repo_path, wt_path, conn)
    }

    /// Creates `branch_name` from current HEAD in `repo_path` and exposes it as
    /// `refs/remotes/origin/<branch_name>` so ancestry checks work without a real remote.
    fn setup_remote_branch(repo_path: &str, branch_name: &str) {
        use std::process::Command;
        Command::new("git")
            .args(["checkout", "-b", branch_name])
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", branch_name])
            .current_dir(repo_path)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "update-ref",
                &format!("refs/remotes/origin/{branch_name}"),
                branch_name,
            ])
            .current_dir(repo_path)
            .output()
            .unwrap();
    }

    #[test]
    fn test_set_base_branch_skips_validation_on_clear() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "feat-test", "2024-01-01T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        // Clearing to None should succeed without touching git
        let result = mgr.set_base_branch(
            "test-repo",
            "feat-test",
            None,
            SetBaseBranchOptions::default(),
        );
        assert!(
            result.is_ok(),
            "clearing base_branch should always succeed: {result:?}"
        );
    }

    #[test]
    fn test_set_base_branch_rejects_non_ancestor() {
        let (_repo_dir, _ws_dir, repo_path, wt_path, conn) = setup_git_repo_with_worktree();
        let config = crate::config::Config::default();

        // Create a divergent branch (not an ancestor of feat/test) and expose it as origin/other
        setup_remote_branch(&repo_path, "other");

        let _ = wt_path; // path is registered in DB

        let mgr = WorktreeManager::new(&conn, &config);
        // origin/other exists but is a divergent commit — not an ancestor of feat/test HEAD.
        let result = mgr.set_base_branch(
            "test-repo",
            "feat-test",
            Some("other"),
            SetBaseBranchOptions::default(),
        );
        assert!(
            matches!(result, Err(ConductorError::InvalidInput(_))),
            "expected InvalidInput for non-ancestor base, got: {result:?}"
        );
    }

    #[test]
    fn test_set_base_branch_accepts_ancestor() {
        let (_repo_dir, _ws_dir, repo_path, _wt_path, conn) = setup_git_repo_with_worktree();
        let config = crate::config::Config::default();

        // Make main an "origin/main" local ref by cloning logic:
        // Since there's no real remote, we simulate it by creating origin/main ref.
        std::process::Command::new("git")
            .args(["update-ref", "refs/remotes/origin/main", "main"])
            .current_dir(&repo_path)
            .output()
            .unwrap();
        // Also set this in the worktree (which is a linked worktree sharing the git dir)
        // The worktree shares the same .git directory, so the ref should be visible.

        let mgr = WorktreeManager::new(&conn, &config);
        // "main" IS an ancestor of feat/test (feat/test was branched off main)
        let result = mgr.set_base_branch(
            "test-repo",
            "feat-test",
            Some("main"),
            SetBaseBranchOptions::default(),
        );
        assert!(
            result.is_ok(),
            "main is an ancestor of feat/test: {result:?}"
        );

        // Verify DB was updated
        let base: Option<String> = conn
            .query_row(
                "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(base.as_deref(), Some("main"));
    }

    #[test]
    fn test_set_base_branch_rebase_onto_non_ancestor() {
        let (_repo_dir, _ws_dir, repo_path, wt_path, conn) = setup_git_repo_with_worktree();
        let config = crate::config::Config::default();

        // Create a new branch "newbase" from main with a distinct commit and expose as origin/newbase.
        setup_remote_branch(&repo_path, "newbase");
        // Set upstream tracking in worktree so rebase can run.
        std::process::Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(&wt_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "T"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        // "newbase" is NOT an ancestor of feat/test; with rebase=true the worktree should be rebased.
        let result = mgr.set_base_branch(
            "test-repo",
            "feat-test",
            Some("newbase"),
            SetBaseBranchOptions { rebase: true },
        );
        assert!(
            result.is_ok(),
            "rebase onto non-ancestor should succeed: {result:?}"
        );

        // Verify DB was updated.
        let base: Option<String> = conn
            .query_row(
                "SELECT base_branch FROM worktrees WHERE slug = 'feat-test'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(base.as_deref(), Some("newbase"));
    }

    #[test]
    fn test_set_base_branch_rejects_dash_branch_name() {
        let conn = create_test_conn();
        let config = crate::config::Config::default();
        insert_repo(&conn);
        insert_wt(&conn, "wt1", "feat-test", "2024-01-01T00:00:00Z");

        let mgr = WorktreeManager::new(&conn, &config);
        let result = mgr.set_base_branch(
            "test-repo",
            "feat-test",
            Some("--upload-pack=cmd"),
            SetBaseBranchOptions::default(),
        );
        assert!(
            matches!(result, Err(ConductorError::InvalidInput(ref msg)) if msg.contains("must not start with")),
            "expected InvalidInput for dash-prefixed branch name, got: {result:?}"
        );
    }

    #[test]
    fn test_set_base_branch_rebase_dirty_rejected() {
        let (_repo_dir, _ws_dir, repo_path, wt_path, conn) = setup_git_repo_with_worktree();
        let config = crate::config::Config::default();

        // Create a divergent branch and expose it as origin/newbase-dirty.
        setup_remote_branch(&repo_path, "newbase-dirty");

        // Create an uncommitted change in the worktree so dirty check fires.
        let dirty_file = format!("{wt_path}/dirty.txt");
        std::fs::write(&dirty_file, "dirty").unwrap();
        std::process::Command::new("git")
            .args(["add", "dirty.txt"])
            .current_dir(&wt_path)
            .output()
            .unwrap();

        let mgr = WorktreeManager::new(&conn, &config);
        // origin/newbase-dirty is NOT an ancestor of feat/test; rebase=true → dirty check fires.
        let result = mgr.set_base_branch(
            "test-repo",
            "feat-test",
            Some("newbase-dirty"),
            SetBaseBranchOptions { rebase: true },
        );
        assert!(
            matches!(result, Err(ConductorError::InvalidInput(ref msg)) if msg.contains("uncommitted")),
            "expected uncommitted-changes error for dirty rebase, got: {result:?}"
        );
    }
}
