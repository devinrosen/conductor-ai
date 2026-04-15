use anyhow::Result;
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::feature::{FeatureManager, FeatureStatus};

use crate::commands::FeatureCommands;
use crate::helpers::parse_ticket_ids;

pub fn handle_feature(command: FeatureCommands, conn: &Connection, config: &Config) -> Result<()> {
    // Check for dangling features before processing any command
    {
        let mgr = conductor_core::feature::FeatureManager::new(conn, config);
        match mgr.reap_dangling_all() {
            Ok(dangling) if !dangling.is_empty() => {
                for f in &dangling {
                    eprintln!("Warning: feature '{}' is dangling (in_progress with no active worktrees and no open PRs)", f.name);
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("Warning: reap_dangling_all failed: {e}"),
        }
    }
    match command {
        FeatureCommands::Create {
            repo,
            name,
            from,
            tickets,
            milestone,
        } => {
            let ticket_ids = parse_ticket_ids(tickets.as_deref().unwrap_or(""));

            let mgr = FeatureManager::new(conn, config);
            let feature = mgr.create(&repo, &name, from.as_deref(), milestone, &ticket_ids)?;
            println!("Created feature: {} ({})", feature.name, feature.branch);
            println!("  Base: {}", feature.base_branch);
            if let Some(ref sid) = feature.source_id {
                println!("  Source: {sid}");
            }
            if milestone.is_some() {
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
        FeatureCommands::Run {
            repo,
            name,
            parallel,
        } => {
            #[cfg(unix)]
            {
                let mgr = FeatureManager::new(conn, config);
                let summary = mgr.run(&repo, &name, parallel)?;
                println!(
                    "Feature '{}' run complete — dispatched: {}, failed: {}",
                    name, summary.dispatched, summary.failed
                );
            }
            #[cfg(not(unix))]
            {
                let _ = (repo, name, parallel);
                return Err(anyhow::anyhow!(
                    "feature run is not supported on this platform"
                ));
            }
        }
    }
    Ok(())
}
