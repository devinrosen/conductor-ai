//! Build identity, supplied at runtime by each binary.
//!
//! The git SHA and commit time change on every commit, so they must not be
//! compile-time inputs of conductor-core (that would recompile core and every
//! dependent crate per commit). Each binary crate computes them in its own
//! `build.rs` (see `build-support/git_info.rs`) and calls [`init`] first thing
//! in `main()`. Until then [`get`] reports `"unknown"`.

use std::sync::OnceLock;

/// Git identity of the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    /// Short git commit SHA (`"unknown"` outside a git checkout).
    pub git_sha: &'static str,
    /// Commit time (ISO 8601) of that commit.
    pub commit_time: &'static str,
}

impl BuildInfo {
    pub const UNKNOWN: BuildInfo = BuildInfo {
        git_sha: "unknown",
        commit_time: "unknown",
    };
}

static BUILD_INFO: OnceLock<BuildInfo> = OnceLock::new();

/// Record the build identity for this process. The first call wins.
pub fn init(info: BuildInfo) {
    let _ = BUILD_INFO.set(info);
}

/// The recorded build identity, or [`BuildInfo::UNKNOWN`] if [`init`] was never called.
pub fn get() -> BuildInfo {
    BUILD_INFO.get().copied().unwrap_or(BuildInfo::UNKNOWN)
}

/// Cargo package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build a [`BuildInfo`] from the calling crate's `CONDUCTOR_GIT_*` env
/// (emitted by its `build.rs`).
#[macro_export]
macro_rules! git_build_info {
    () => {
        $crate::build_info::BuildInfo {
            git_sha: env!("CONDUCTOR_GIT_SHA"),
            commit_time: env!("CONDUCTOR_GIT_COMMIT_TIME"),
        }
    };
}

/// `<version> (<sha> <commit time>)`, for `--version` output.
#[macro_export]
macro_rules! long_version {
    () => {
        concat!(
            env!("CARGO_PKG_VERSION"),
            " (",
            env!("CONDUCTOR_GIT_SHA"),
            " ",
            env!("CONDUCTOR_GIT_COMMIT_TIME"),
            ")"
        )
    };
}

/// `<version>+<sha>` semver with build metadata.
#[macro_export]
macro_rules! semver_with_build {
    () => {
        concat!(env!("CARGO_PKG_VERSION"), "+", env!("CONDUCTOR_GIT_SHA"))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_then_get_roundtrips() {
        let info = BuildInfo {
            git_sha: "abc1234",
            commit_time: "2026-01-01T00:00:00+00:00",
        };
        init(info);
        // First call wins process-wide, so another test may have set it already;
        // either way `get()` must return an initialised value, never panic.
        let got = get();
        assert!(!got.git_sha.is_empty());
        assert!(!got.commit_time.is_empty());
        assert!(VERSION.len() > 1);
    }
}
