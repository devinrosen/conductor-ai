//! `conductor mcp serve` — stdio MCP server exposing conductor resources and tools.
//!
//! All DB access runs inside `tokio::task::spawn_blocking` since `rusqlite::Connection`
//! is `!Send`. The `rmcp` library handles the stdio JSON-RPC transport.

#[macro_use]
mod helpers;

mod resources;
mod server;
#[cfg(unix)]
mod stdio;
mod subscriptions;
mod tools;

use anyhow::Context as _;

pub use server::ConductorMcpServer;

/// Start the stdio MCP server and block until the client disconnects.
pub async fn serve() -> anyhow::Result<()> {
    let server = ConductorMcpServer::new();
    // On unix, keep the JSON-RPC pipes off fd 0/1 so spawned children can't
    // read requests or write into responses (#3093). See `stdio`.
    #[cfg(unix)]
    let service = {
        let (reader, writer) =
            stdio::isolate_stdio().context("failed to isolate MCP stdio from child processes")?;
        let transport = (
            tokio::fs::File::from_std(reader),
            tokio::fs::File::from_std(writer),
        );
        rmcp::serve_server(server, transport).await?
    };
    #[cfg(not(unix))]
    let service = rmcp::serve_server(server, rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
