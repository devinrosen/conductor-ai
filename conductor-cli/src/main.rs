use anyhow::Result;
use clap::Parser;

use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::open_database;

#[cfg(unix)]
mod background;
mod commands;
mod handlers;
mod helpers;
mod mcp;
mod statusline;

use commands::{Cli, Commands};

fn main() -> Result<()> {
    // Initialize tracing subscriber so workflow engine log events appear on
    // stderr for CLI users.  Respects RUST_LOG; defaults to `info`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let config = load_config()?;
    ensure_dirs(&config)?;

    let db_path = conductor_core::config::db_path();
    let conn = open_database(&db_path)?;

    helpers::check_prerequisites();

    match cli.command {
        Commands::Repo { command } => handlers::repo::handle_repo(command, &conn, &config)?,
        Commands::Worktree { command } => {
            handlers::worktree::handle_worktree(command, &conn, &config)?
        }
        Commands::Agent { command } => handlers::agent::handle_agent(command, &conn, &config)?,
        Commands::Tickets { command } => {
            handlers::tickets::handle_tickets(command, &conn, &config)?
        }
        Commands::Feature { command } => {
            handlers::feature::handle_feature(command, &conn, &config)?
        }
        Commands::Workflow { command } => {
            handlers::workflow::handle_workflow(command, &conn, &config)?
        }
        Commands::Statusline { command } => handlers::statusline::handle_statusline(command)?,
        Commands::Mcp { command } => handlers::mcp::handle_mcp(command)?,
        Commands::Dev { command } => handlers::dev::handle_dev(command)?,
    }

    Ok(())
}
