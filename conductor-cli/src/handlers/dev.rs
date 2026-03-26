use anyhow::{Context, Result};

use conductor_core::db::open_database;

use crate::commands::DevCommands;

pub fn handle_dev(command: DevCommands) -> Result<()> {
    match command {
        DevCommands::Seed { reset } => {
            let db_path = conductor_core::config::db_path();
            if reset && db_path.exists() {
                std::fs::remove_file(&db_path).context("failed to remove existing database")?;
                println!("Removed {}", db_path.display());
            }
            let conn = open_database(&db_path)?;
            conductor_core::db::seed::seed_database(&conn)?;
            println!("Seeded database at {}", db_path.display());
        }
    }
    Ok(())
}
