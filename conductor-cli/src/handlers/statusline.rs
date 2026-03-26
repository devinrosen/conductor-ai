use anyhow::Result;

use crate::commands::StatuslineCommands;

pub fn handle_statusline(command: StatuslineCommands) -> Result<()> {
    match command {
        StatuslineCommands::Install => crate::statusline::install()?,
        StatuslineCommands::Uninstall => crate::statusline::uninstall()?,
    }
    Ok(())
}
