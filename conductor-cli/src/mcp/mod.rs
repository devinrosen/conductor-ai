//! `conductor mcp serve` — stdio MCP server exposing conductor resources and tools.
//!
//! All DB access runs inside `tokio::task::spawn_blocking` since `rusqlite::Connection`
//! is `!Send`. The `rmcp` library handles the stdio JSON-RPC transport.

#[macro_use]
mod helpers;

mod resources;
mod server;
mod tools;

pub use server::ConductorMcpServer;

/// Start the stdio MCP server and block until the client disconnects.
pub async fn serve() -> anyhow::Result<()> {
    let db_path = conductor_core::config::db_path();
    let server = ConductorMcpServer::new(db_path);
    let service = rmcp::serve_server(server, rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
