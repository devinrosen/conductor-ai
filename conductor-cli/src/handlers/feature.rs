use anyhow::Result;
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::feature::{build_milestone_source_id, FeatureManager, FeatureStatus};
use conductor_core::github::parse_github_remote;
use conductor_core::repo::RepoManager;

use crate::commands::FeatureCommands;
use crate::helpers::parse_ticket_ids;

pub fn handle_feature(command: FeatureCommands, conn: &Connection, config: &Config) -> Result<()> {
    match command {
        FeatureCommands::Create {
            repo,
            name,
            from,
            tickets,
            milestone,
        } => {
            let ticket_ids = parse_ticket_ids(tickets.as_deref().unwrap_or(""));

            // Derive source_type/source_id from --milestone if provided.
            // Store the full structured source_id so sync_from_milestone can parse it.
            let has_milestone = milestone.is_some();
            let (source_type_opt, source_id_str): (Option<&str>, Option<String>) = if let Some(ms) =
                milestone
            {
                let repo_rec = RepoManager::new(conn, config).get_by_slug(&repo)?;
                let full_source_id = match parse_github_remote(&repo_rec.remote_url) {
                    Some((owner, repo_name)) => build_milestone_source_id(&owner, &repo_name, ms),
                    None => ms.to_string(),
                };
                (Some("github_milestone"), Some(full_source_id))
            } else {
                (None, None)
            };

            let mgr = FeatureManager::new(conn, config);
            let feature = mgr.create(
                &repo,
                &name,
                from.as_deref(),
                source_type_opt,
                source_id_str.as_deref(),
                &ticket_ids,
            )?;
            println!("Created feature: {} ({})", feature.name, feature.branch);
            println!("  Base: {}", feature.base_branch);
            if let Some(ref sid) = source_id_str {
                println!("  Source: {sid}");
            }
            if has_milestone {
                match mgr.sync_from_milestone(&repo, &name) {
                    Ok(result) => println!(
                        "  Synced milestone: +{} added, {} removed",
                        result.added, result.removed
                    ),
                    Err(e) => eprintln!("  Warning: milestone sync failed: {e}"),
                }
            }
            if !ticket_ids.is_empty() {
                println!("  Linked {} ticket(s)", ticket_ids.len());
            }
        }
        FeatureCommands::List { repo } => {
            let mgr = FeatureManager::new(conn, config);
            // Refresh last_commit_at cache before listing
            let _ = mgr.refresh_last_commit_all(&repo);
            let features = mgr.list(&repo)?;
            let stale_days_threshold = config.defaults.stale_feature_days;
            if features.is_empty() {
                println!("No features. Create one with `conductor feature create`.");
            } else {
                println!(
                    "  {:<25} {:<30} {:<10} {:<5} {:<5} STALE",
                    "NAME", "BRANCH", "STATUS", "WTs", "TKTs"
                );
                for f in &features {
                    let stale_label = if FeatureManager::is_stale(f, stale_days_threshold) {
                        match FeatureManager::stale_days(f) {
                            Some(d) => format!("\u{26a0} stale {d}d"),
                            None => "\u{26a0} stale".to_string(),
                        }
                    } else {
                        String::new()
                    };
                    println!(
                        "  {:<25} {:<30} {:<10} {:<5} {:<5} {}",
                        f.name, f.branch, f.status, f.worktree_count, f.ticket_count, stale_label
                    );
                }
            }
        }
        FeatureCommands::Link {
            repo,
            name,
            tickets,
        } => {
            let ticket_ids = parse_ticket_ids(&tickets);
            let mgr = FeatureManager::new(conn, config);
            mgr.link_tickets(&repo, &name, &ticket_ids)?;
            println!("Linked {} ticket(s) to feature '{name}'", ticket_ids.len());
        }
        FeatureCommands::Unlink {
            repo,
            name,
            tickets,
        } => {
            let ticket_ids = parse_ticket_ids(&tickets);
            let mgr = FeatureManager::new(conn, config);
            mgr.unlink_tickets(&repo, &name, &ticket_ids)?;
            println!(
                "Unlinked {} ticket(s) from feature '{name}'",
                ticket_ids.len()
            );
        }
        FeatureCommands::Pr { repo, name, draft } => {
            let mgr = FeatureManager::new(conn, config);
            let url = mgr.create_pr(&repo, &name, draft)?;
            println!("{url}");
        }
        FeatureCommands::Review { repo, name } => {
            let mgr = FeatureManager::new(conn, config);
            mgr.transition(&repo, &name, FeatureStatus::ReadyForReview)?;
            println!("Feature '{name}' marked as ready_for_review.");
        }
        FeatureCommands::Approve { repo, name } => {
            let mgr = FeatureManager::new(conn, config);
            mgr.transition(&repo, &name, FeatureStatus::Approved)?;
            println!("Feature '{name}' approved.");
        }
        FeatureCommands::Close { repo, name } => {
            let mgr = FeatureManager::new(conn, config);
            mgr.close(&repo, &name)?;
            println!("Feature '{name}' closed.");
        }
        FeatureCommands::Delete { repo, name } => {
            let mgr = FeatureManager::new(conn, config);
            mgr.delete(&repo, &name)?;
            println!("Feature '{name}' deleted.");
        }
        FeatureCommands::Sync { repo, name } => {
            let mgr = FeatureManager::new(conn, config);
            let result = mgr.sync_from_milestone(&repo, &name)?;
            println!(
                "Synced feature '{}': +{} added, {} removed",
                name, result.added, result.removed
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use conductor_core::feature::build_milestone_source_id;
    use conductor_core::github::parse_github_remote;

    #[test]
    fn test_milestone_source_id_built_from_github_remote() {
        // Mirrors the logic in the Create handler: parse the remote URL to get
        // owner/repo, then build the canonical source_id via build_milestone_source_id.
        let remote = "git@github.com:myorg/myrepo.git";
        let (owner, repo_name) = parse_github_remote(remote).unwrap();
        let source_id = build_milestone_source_id(&owner, &repo_name, 5);
        assert_eq!(source_id, "github.com/myorg/myrepo/milestones/5");
    }

    #[test]
    fn test_milestone_source_id_https_remote() {
        let remote = "https://github.com/acme/widget.git";
        let (owner, repo_name) = parse_github_remote(remote).unwrap();
        let source_id = build_milestone_source_id(&owner, &repo_name, 42);
        assert_eq!(source_id, "github.com/acme/widget/milestones/42");
    }
}
