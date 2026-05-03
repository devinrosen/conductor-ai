use anyhow::{Context, Result};

use conductor_core::Conductor;

use crate::commands::DevCommands;

pub fn handle_dev(command: DevCommands) -> Result<()> {
    match command {
        DevCommands::Seed { reset } => {
            let db_path = Conductor::db_path();
            if reset && db_path.exists() {
                std::fs::remove_file(&db_path).context("failed to remove existing database")?;
                println!("Removed {}", db_path.display());
            }
            let conductor = Conductor::open()?;
            conductor_core::db::seed::seed_database(&conductor.conn)?;
            println!("Seeded database at {}", db_path.display());
        }
    }
    Ok(())
}
