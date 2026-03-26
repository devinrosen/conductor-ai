use anyhow::Result;
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::feature::FeatureManager;

use crate::commands::FeatureCommands;
use crate::helpers::parse_ticket_ids;

pub fn handle_feature(command: FeatureCommands, conn: &Connection, config: &Config) -> Result<()> {
    match command {
        FeatureCommands::Create {
            repo,
            name,
            from,
            tickets,
        } => {
            let ticket_ids = parse_ticket_ids(tickets.as_deref().unwrap_or(""));

            let mgr = FeatureManager::new(conn, config);
            let feature = mgr.create(&repo, &name, from.as_deref(), &ticket_ids)?;
            println!("Created feature: {} ({})", feature.name, feature.branch);
            println!("  Base: {}", feature.base_branch);
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
        FeatureCommands::Close { repo, name } => {
            let mgr = FeatureManager::new(conn, config);
            mgr.close(&repo, &name)?;
            println!("Feature '{name}' closed.");
        }
    }
    Ok(())
}
