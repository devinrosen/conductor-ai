use anyhow::Result;
use clap::Parser;

use conductor_core::config::{ensure_dirs, load_config};
use conductor_core::db::{open_database, open_database_compat};

#[cfg(unix)]
mod background;
mod commands;
mod handlers;
mod helpers;
mod mcp;
mod setup;

use commands::{AgentCommands, Cli, Commands};

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
    // Headless agent subprocesses use compat mode so they tolerate a DB schema
    // that is ahead of this binary (e.g. after an implement step applied a new
    // migration and rebuilt the binary, but the subprocess was spawned with the
    // old binary). All other interactive commands use strict open_database so
    // users still get the "please rebuild" prompt when running an outdated binary.
    let use_compat = matches!(
        &cli.command,
        Commands::Agent {
            command: AgentCommands::Run { .. }
        }
    );
    let conn = if use_compat {
        open_database_compat(&db_path)?
    } else {
        open_database(&db_path)?
    };

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
        Commands::Workflow { command } => {
            handlers::workflow::handle_workflow(command, &conn, &config)?
        }
        Commands::Setup { command } => handlers::setup::handle_setup(command)?,
        Commands::Mcp { command } => handlers::mcp::handle_mcp(command)?,
        Commands::Dev { command } => handlers::dev::handle_dev(command)?,
        Commands::Notifications { command } => {
            handlers::notifications::handle_notifications(command, &config)?
        }
        Commands::Conversation { command } => {
            handlers::conversation::handle_conversation(command, &conn, &config)?
        }
    }

    Ok(())
}
