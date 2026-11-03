//! Interactive Claude Code session tracking (RFC 021).

pub mod hook;
pub mod ingest;
pub mod spool;
mod transcript;

pub use ingest::{IngestStats, TranscriptIngester};
