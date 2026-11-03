//! Build identity embedded at compile time by `conductor-core/build.rs`.

/// Short git commit SHA the binary was built from (`"unknown"` outside a git checkout).
pub const GIT_SHA: &str = env!("CONDUCTOR_GIT_SHA");
/// Commit time (ISO 8601) of that commit.
pub const COMMIT_TIME: &str = env!("CONDUCTOR_GIT_COMMIT_TIME");
/// Cargo package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// `<version> (<sha> <commit time>)`, for `--version` output.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CONDUCTOR_GIT_SHA"),
    " ",
    env!("CONDUCTOR_GIT_COMMIT_TIME"),
    ")"
);
/// `<version>+<sha>` semver with build metadata.
pub const SEMVER_WITH_BUILD: &str =
    concat!(env!("CARGO_PKG_VERSION"), "+", env!("CONDUCTOR_GIT_SHA"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consts_are_non_empty() {
        assert!(!GIT_SHA.is_empty());
        assert!(!COMMIT_TIME.is_empty());
        assert!(LONG_VERSION.starts_with(VERSION));
        assert!(SEMVER_WITH_BUILD.contains('+'));
    }
}
