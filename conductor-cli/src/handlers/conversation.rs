use anyhow::{bail, Result};
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::conversation::{ConversationManager, ConversationScope};
use conductor_core::error::ConductorError;
use conductor_core::repo::RepoManager;
use conductor_core::worktree::WorktreeManager;

use crate::commands::ConversationCommands;

pub fn handle_conversation(
    command: ConversationCommands,
    conn: &Connection,
    config: &Config,
) -> Result<()> {
    match command {
        ConversationCommands::Clear {
            repo,
            worktree,
            yes,
        } => {
            let repo_mgr = RepoManager::new(conn, config);
            let repo_rec = repo_mgr.get_by_slug(&repo)?;

            let wt_mgr = WorktreeManager::new(conn, config);
            let wt = wt_mgr.get_by_slug(&repo_rec.id, &worktree)?;

            let conv_mgr = ConversationManager::new(conn);

            // Verify a conversation exists before prompting the user.
            let convs = conv_mgr.list(&ConversationScope::Worktree, &wt.id)?;
            if convs.is_empty() {
                bail!("No conversation found for {repo}/{worktree}");
            }

            if !yes {
                eprint!("Clear all conversation history for {repo}/{worktree}? [y/N] ");
                use std::io::BufRead;
                let mut input = String::new();
                std::io::stdin().lock().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    eprintln!("Aborted.");
                    return Ok(());
                }
            }

            match conv_mgr.clear_for_scope(&ConversationScope::Worktree, &wt.id) {
                Ok(()) => {
                    println!("Conversation cleared.");
                }
                Err(ConductorError::ConversationHasActiveRun { .. }) => {
                    bail!("Cannot clear: an agent run is currently active.");
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use conductor_core::agent::AgentManager;
    use conductor_core::config::Config;
    use conductor_core::conversation::{ConversationManager, ConversationScope};
    use conductor_core::db::open_database;
    use conductor_core::repo::RepoManager;

    #[test]
    fn test_clear_no_conversation_returns_error() {
        let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        let conn = open_database(&db_path).unwrap();
        let config = Config::default();

        // Register a repo + worktree so the slug lookups succeed
        let dir = tempfile::tempdir().unwrap();
        let repo_mgr = RepoManager::new(&conn, &config);
        repo_mgr
            .register(
                "test-repo",
                dir.path().to_str().unwrap(),
                "https://github.com/test/repo.git",
                None,
            )
            .unwrap();
        let repo = repo_mgr.get_by_slug("test-repo").unwrap();
        let wt_dir = tempfile::tempdir().unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at)
             VALUES ('wt1', ?1, 'my-wt', 'feat/my-wt', ?2, 'active', '2024-01-01T00:00:00Z')",
            rusqlite::params![repo.id, wt_dir.path().to_str().unwrap()],
        )
        .unwrap();

        let cmd = crate::commands::ConversationCommands::Clear {
            repo: "test-repo".into(),
            worktree: "my-wt".into(),
            yes: true,
        };
        let err = super::handle_conversation(cmd, &conn, &config).unwrap_err();
        assert!(
            err.to_string().contains("No conversation found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_clear_success_with_yes_flag() {
        let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        let conn = open_database(&db_path).unwrap();
        let config = Config::default();

        // Register repo and worktree
        let dir = tempfile::tempdir().unwrap();
        let repo_mgr = RepoManager::new(&conn, &config);
        repo_mgr
            .register(
                "test-repo2",
                dir.path().to_str().unwrap(),
                "https://github.com/test/repo2.git",
                None,
            )
            .unwrap();
        let repo = repo_mgr.get_by_slug("test-repo2").unwrap();
        let wt_dir = tempfile::tempdir().unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at)
             VALUES ('wt2', ?1, 'my-wt2', 'feat/my-wt2', ?2, 'active', '2024-01-01T00:00:00Z')",
            rusqlite::params![repo.id, wt_dir.path().to_str().unwrap()],
        )
        .unwrap();

        // Create a conversation for the worktree
        let conv_mgr = ConversationManager::new(&conn);
        conv_mgr.create(ConversationScope::Worktree, "wt2").unwrap();

        // Verify it exists
        let convs = conv_mgr.list(&ConversationScope::Worktree, "wt2").unwrap();
        assert_eq!(convs.len(), 1);

        let cmd = crate::commands::ConversationCommands::Clear {
            repo: "test-repo2".into(),
            worktree: "my-wt2".into(),
            yes: true,
        };
        super::handle_conversation(cmd, &conn, &config).unwrap();

        // Confirm it was deleted
        let convs_after = conv_mgr.list(&ConversationScope::Worktree, "wt2").unwrap();
        assert!(convs_after.is_empty());
    }

    #[test]
    fn test_clear_fails_when_active_run_exists() {
        let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        let conn = open_database(&db_path).unwrap();
        let config = Config::default();

        let dir = tempfile::tempdir().unwrap();
        let repo_mgr = RepoManager::new(&conn, &config);
        repo_mgr
            .register(
                "test-repo3",
                dir.path().to_str().unwrap(),
                "https://github.com/test/repo3.git",
                None,
            )
            .unwrap();
        let repo = repo_mgr.get_by_slug("test-repo3").unwrap();
        let wt_dir = tempfile::tempdir().unwrap();
        conn.execute(
            "INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at)
             VALUES ('wt3', ?1, 'my-wt3', 'feat/my-wt3', ?2, 'active', '2024-01-01T00:00:00Z')",
            rusqlite::params![repo.id, wt_dir.path().to_str().unwrap()],
        )
        .unwrap();

        // Create a conversation and immediately attach a running agent run.
        let conv_mgr = ConversationManager::new(&conn);
        let conv = conv_mgr.create(ConversationScope::Worktree, "wt3").unwrap();

        let agent_mgr = AgentManager::new(&conn);
        agent_mgr
            .create_run_for_conversation("wt3", "hello", None, None, &conv.id)
            .unwrap();

        // The active run should block the clear.
        let cmd = crate::commands::ConversationCommands::Clear {
            repo: "test-repo3".into(),
            worktree: "my-wt3".into(),
            yes: true,
        };
        let err = super::handle_conversation(cmd, &conn, &config).unwrap_err();
        assert!(
            err.to_string().contains("active"),
            "unexpected error: {err}"
        );

        // Conversation must still exist.
        let convs = conv_mgr.list(&ConversationScope::Worktree, "wt3").unwrap();
        assert_eq!(convs.len(), 1);
    }
}
