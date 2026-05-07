//! Classification policy for env-var keys that hold secrets.
//!
//! The TUI masks values whose keys look secret-bearing by default; users can
//! still toggle reveal with `r`. This is a display-policy concern, not config
//! schema, so it lives next to TUI state — but separated from rendering code
//! so it can be reused and tested without pulling in ratatui.

/// Returns true when `key` looks like it stores a secret and its value should
/// be masked by default. Conservative case-insensitive match on common
/// substrings; users can still toggle reveal with `r`.
pub fn is_secret_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("PASSWD")
        || upper.ends_with("_KEY")
        || upper.ends_with("KEY")
}
