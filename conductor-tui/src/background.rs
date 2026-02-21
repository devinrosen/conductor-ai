use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use conductor_core::config::{db_path, load_config};
use conductor_core::db::open_database;
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::RepoManager;
use conductor_core::session::SessionTracker;
use conductor_core::tickets::TicketSyncer;
use conductor_core::worktree::WorktreeManager;

use crate::action::Action;
use crate::event::Event;

/// Spawn the DB poller thread. Polls every `interval` and sends DataRefreshed events.
pub fn spawn_db_poller(tx: Sender<Event>, interval: Duration) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        if let Some(action) = poll_data() {
            if tx.send(Event::Background(action)).is_err() {
                break;
            }
        }
    });
}

/// Poll all data from the database. Returns a DataRefreshed action if successful.
pub fn poll_data() -> Option<Action> {
    let db = db_path();
    let conn = open_database(&db).ok()?;
    let config = load_config().ok()?;

    let repo_mgr = RepoManager::new(&conn, &config);
    let wt_mgr = WorktreeManager::new(&conn, &config);
    let ticket_syncer = TicketSyncer::new(&conn);
    let session_tracker = SessionTracker::new(&conn);

    let repos = repo_mgr.list().ok()?;
    let worktrees = wt_mgr.list(None).ok()?;
    let tickets = ticket_syncer.list(None).ok()?;
    let session = session_tracker.current().ok()?;
    let session_worktrees = if let Some(ref s) = session {
        session_tracker.get_worktrees(&s.id).unwrap_or_default()
    } else {
        Vec::new()
    };

    Some(Action::DataRefreshed {
        repos,
        worktrees,
        tickets,
        session,
        session_worktrees,
    })
}

/// Spawn the ticket sync timer. Syncs all repos every `interval`.
pub fn spawn_ticket_sync(tx: Sender<Event>, interval: Duration) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        sync_all_tickets(&tx);
    });
}

fn sync_all_tickets(tx: &Sender<Event>) {
    let db = db_path();
    let Ok(conn) = open_database(&db) else { return };
    let Ok(config) = load_config() else { return };

    let repo_mgr = RepoManager::new(&conn, &config);
    let Ok(repos) = repo_mgr.list() else { return };

    let syncer = TicketSyncer::new(&conn);
    let source_mgr = IssueSourceManager::new(&conn);

    for repo in repos {
        let sources = source_mgr.list(&repo.id).unwrap_or_default();

        if sources.is_empty() {
            // Backward compat: auto-detect GitHub from remote_url
            if let Some((owner, name)) = github::parse_github_remote(&repo.remote_url) {
                let action = sync_github_repo(&syncer, &repo.id, &repo.slug, &owner, &name);
                if tx.send(Event::Background(action)).is_err() {
                    return;
                }
            }
        } else {
            for source in sources {
                match source.source_type.as_str() {
                    "github" => {
                        let action = match serde_json::from_str::<GitHubConfig>(&source.config_json)
                        {
                            Ok(cfg) => sync_github_repo(
                                &syncer, &repo.id, &repo.slug, &cfg.owner, &cfg.repo,
                            ),
                            Err(e) => Action::TicketSyncFailed {
                                repo_slug: repo.slug.clone(),
                                error: format!("invalid github config: {e}"),
                            },
                        };
                        if tx.send(Event::Background(action)).is_err() {
                            return;
                        }
                    }
                    "jira" => {
                        let action = match serde_json::from_str::<JiraConfig>(&source.config_json) {
                            Ok(cfg) => {
                                sync_jira_repo(&syncer, &repo.id, &repo.slug, &cfg.jql, &cfg.url)
                            }
                            Err(e) => Action::TicketSyncFailed {
                                repo_slug: repo.slug.clone(),
                                error: format!("invalid jira config: {e}"),
                            },
                        };
                        if tx.send(Event::Background(action)).is_err() {
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Sync Jira issues for a single repo, returning the appropriate Action.
fn sync_jira_repo(
    syncer: &TicketSyncer,
    repo_id: &str,
    repo_slug: &str,
    jql: &str,
    base_url: &str,
) -> Action {
    match jira_acli::sync_jira_issues_acli(jql, base_url) {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    let _ = syncer.close_missing_tickets(repo_id, "jira", &synced_ids);
                    Action::TicketSyncComplete {
                        repo_slug: repo_slug.to_string(),
                        count,
                    }
                }
                Err(e) => Action::TicketSyncFailed {
                    repo_slug: repo_slug.to_string(),
                    error: e.to_string(),
                },
            }
        }
        Err(e) => Action::TicketSyncFailed {
            repo_slug: repo_slug.to_string(),
            error: e.to_string(),
        },
    }
}

/// Sync GitHub issues for a single repo, returning the appropriate Action.
fn sync_github_repo(
    syncer: &TicketSyncer,
    repo_id: &str,
    repo_slug: &str,
    owner: &str,
    name: &str,
) -> Action {
    match github::sync_github_issues(owner, name) {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    let _ = syncer.close_missing_tickets(repo_id, "github", &synced_ids);
                    Action::TicketSyncComplete {
                        repo_slug: repo_slug.to_string(),
                        count,
                    }
                }
                Err(e) => Action::TicketSyncFailed {
                    repo_slug: repo_slug.to_string(),
                    error: e.to_string(),
                },
            }
        }
        Err(e) => Action::TicketSyncFailed {
            repo_slug: repo_slug.to_string(),
            error: e.to_string(),
        },
    }
}

/// Spawn a one-shot background operation for blocking tasks.
#[allow(dead_code)]
pub fn spawn_blocking(tx: Sender<Event>, f: impl FnOnce() -> Action + Send + 'static) {
    thread::spawn(move || {
        let action = f();
        let _ = tx.send(Event::Background(action));
    });
}
