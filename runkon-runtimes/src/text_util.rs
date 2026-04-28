use crate::error::{Result, RuntimeError};

/// Validate that a `run_id` is safe to use as a filesystem path component.
///
/// Rejects empty strings, path separators, and any character outside
/// `[A-Za-z0-9\-_]` to prevent path-traversal attacks.
pub fn validate_run_id(run_id: &str) -> Result<()> {
    if !run_id.is_empty()
        && run_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(RuntimeError::InvalidInput(format!(
            "invalid run_id '{run_id}': must be non-empty and contain only alphanumeric characters, hyphens, or underscores"
        )))
    }
}
