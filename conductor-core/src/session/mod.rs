//! Interactive Claude Code session tracking (RFC 021).

pub mod hook;
pub mod ingest;
pub mod launch;
pub mod spool;
mod transcript;

pub use ingest::{IngestStats, TranscriptIngester, WEB_TICK_BYTE_BUDGET};
