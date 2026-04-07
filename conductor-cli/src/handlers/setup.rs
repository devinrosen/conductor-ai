use anyhow::Result;

use crate::commands::SetupCommands;

pub fn handle_setup(command: SetupCommands) -> Result<()> {
    match command {
        SetupCommands::Install => crate::setup::install()?,
        SetupCommands::Uninstall => crate::setup::uninstall()?,
    }
    Ok(())
}
