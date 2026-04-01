use anyhow::Result;
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager};
use conductor_core::repo::{derive_local_path, derive_slug_from_url, RepoManager};

use crate::commands::{RepoCommands, SourceCommands};

pub fn handle_repo(command: RepoCommands, conn: &Connection, config: &Config) -> Result<()> {
    match command {
        RepoCommands::Register {
            remote_url,
            slug,
            local_path,
            workspace,
        } => {
            let slug = slug.unwrap_or_else(|| derive_slug_from_url(&remote_url));

            let local = local_path.unwrap_or_else(|| derive_local_path(config, &slug));

            let mgr = RepoManager::new(conn, config);
            let repo = mgr.register(&slug, &local, &remote_url, workspace.as_deref())?;
            println!("Registered repo: {} ({})", repo.slug, repo.remote_url);
        }
        RepoCommands::List => {
            let mgr = RepoManager::new(conn, config);
            let repos = mgr.list()?;
            if repos.is_empty() {
                println!("No repos registered. Use `conductor repo register` to register one.");
            } else {
                for repo in repos {
                    println!("  {}  {}", repo.slug, repo.remote_url);
                }
            }
        }
        RepoCommands::Discover { owner } => {
            if let Some(ref owner_str) = owner {
                // List repos for a specific owner (org or personal via "")
                let owner_opt = if owner_str.is_empty() {
                    None
                } else {
                    Some(owner_str.as_str())
                };
                let discovered = github::discover_github_repos(owner_opt)?;
                if discovered.is_empty() {
                    println!("No repos found for {}.", owner_str);
                } else {
                    let mgr = RepoManager::new(conn, config);
                    let registered = mgr.list()?;
                    for repo in &discovered {
                        let is_registered = registered.iter().any(|r| {
                            r.remote_url == repo.clone_url || r.remote_url == repo.ssh_url
                        });
                        let marker = if is_registered { " [registered]" } else { "" };
                        let privacy = if repo.private { " (private)" } else { "" };
                        println!("  {}{}{}", repo.full_name, privacy, marker);
                        if !repo.description.is_empty() {
                            println!("    {}", repo.description);
                        }
                    }
                    let unregistered = discovered
                        .iter()
                        .filter(|r| {
                            !registered.iter().any(|reg| {
                                reg.remote_url == r.clone_url || reg.remote_url == r.ssh_url
                            })
                        })
                        .count();
                    println!(
                        "\n{} repo(s) found, {} not yet registered.",
                        discovered.len(),
                        unregistered
                    );
                    println!("Use `conductor repo register <url>` to register a repo.");
                }
            } else {
                // No owner given: list orgs (+ personal)
                let orgs = github::list_github_orgs()?;
                println!("  Personal (your repos)  →  conductor repo discover \"\"");
                for org in &orgs {
                    println!("  {org}  →  conductor repo discover {org}");
                }
                if orgs.is_empty() {
                    println!("  (no organizations found)");
                }
            }
        }
        RepoCommands::Unregister { slug } => {
            let mgr = RepoManager::new(conn, config);
            mgr.unregister(&slug)?;
            println!("Unregistered repo: {slug}");
        }
        RepoCommands::SetModel { slug, model } => {
            let mgr = RepoManager::new(conn, config);
            mgr.set_model(&slug, model.as_deref())?;
            match model {
                Some(m) => println!("Set model for {slug} to: {m}"),
                None => println!("Cleared model override for {slug} (will use global default)"),
            }
        }
        RepoCommands::AllowAgentIssues { slug, allow } => {
            let mgr = RepoManager::new(conn, config);
            let repo = mgr.get_by_slug(&slug)?;
            mgr.set_allow_agent_issue_creation(&repo.id, allow)?;
            if allow {
                println!("Enabled agent issue creation for {slug}");
            } else {
                println!("Disabled agent issue creation for {slug}");
            }
        }
        RepoCommands::Sources { command } => {
            let repo_mgr = RepoManager::new(conn, config);
            let source_mgr = IssueSourceManager::new(conn);

            match command {
                SourceCommands::Add {
                    slug,
                    source_type,
                    config: config_json,
                } => {
                    let repo = repo_mgr.get_by_slug(&slug)?;

                    let config_str = match (source_type.as_str(), config_json) {
                        ("github", Some(json)) => {
                            // Validate it's valid JSON
                            let _: serde_json::Value = serde_json::from_str(&json)
                                .map_err(|e| anyhow::anyhow!("invalid JSON config: {e}"))?;
                            json
                        }
                        ("github", None) => {
                            // Auto-infer from remote URL
                            let (owner, name) =
                                github::parse_github_remote(&repo.remote_url).ok_or_else(
                                    || {
                                        anyhow::anyhow!(
                                            "cannot infer GitHub config from remote URL: {}. Use --config to specify manually.",
                                            repo.remote_url
                                        )
                                    },
                                )?;
                            serde_json::to_string(&GitHubConfig { owner, repo: name })?
                        }
                        ("jira", Some(json)) => {
                            let _: serde_json::Value = serde_json::from_str(&json)
                                .map_err(|e| anyhow::anyhow!("invalid JSON config: {e}"))?;
                            json
                        }
                        ("jira", None) => {
                            anyhow::bail!(
                                "--config is required for jira sources (e.g. --config '{{\"jql\":\"project = KEY AND status != Done\",\"url\":\"https://...\"}}')");
                        }
                        ("vantage", Some(json)) => {
                            let _: serde_json::Value = serde_json::from_str(&json)
                                .map_err(|e| anyhow::anyhow!("invalid JSON config: {e}"))?;
                            json
                        }
                        ("vantage", None) => {
                            anyhow::bail!(
                                "--config is required for vantage sources (e.g. --config '{{\"project_id\":\"PROJ-012\",\"sdlc_root\":\"/path/to/sdlc\"}}')");
                        }
                        _ => {
                            anyhow::bail!(
                                "unsupported source type: '{}'. Use 'github', 'jira', or 'vantage'.",
                                source_type
                            );
                        }
                    };

                    let source = source_mgr.add(&repo.id, &source_type, &config_str, &slug)?;
                    println!(
                        "Added {} source for {}: {}",
                        source.source_type, slug, source.config_json
                    );
                }
                SourceCommands::List { slug } => {
                    let repo = repo_mgr.get_by_slug(&slug)?;
                    let sources = source_mgr.list(&repo.id)?;
                    if sources.is_empty() {
                        println!("No issue sources configured for {slug}.");
                    } else {
                        for s in sources {
                            println!("  {} — {}", s.source_type, s.config_json);
                        }
                    }
                }
                SourceCommands::Remove { slug, source_type } => {
                    let repo = repo_mgr.get_by_slug(&slug)?;
                    let removed = source_mgr.remove_by_type(&repo.id, &source_type)?;
                    if removed {
                        println!("Removed {source_type} source for {slug}");
                    } else {
                        println!("No {source_type} source found for {slug}");
                    }
                }
            }
        }
    }
    Ok(())
}
