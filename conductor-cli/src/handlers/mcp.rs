use anyhow::{Context, Result};

use crate::commands::McpCommands;

pub fn handle_mcp(command: McpCommands) -> Result<()> {
    match command {
        McpCommands::Serve => {
            let rt = tokio::runtime::Runtime::new()
                .context("failed to create tokio runtime for MCP server")?;
            rt.block_on(crate::mcp::serve())?;
        }
    }
    Ok(())
}
