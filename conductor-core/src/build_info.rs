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
    set_in(&BUILD_INFO, info);
}

fn set_in(lock: &OnceLock<BuildInfo>, info: BuildInfo) {
    let _ = lock.set(info);
}

fn get_from(lock: &OnceLock<BuildInfo>) -> BuildInfo {
    lock.get().copied().unwrap_or(BuildInfo::UNKNOWN)
}

/// The recorded build identity, or [`BuildInfo::UNKNOWN`] if [`init`] was never called.
pub fn get() -> BuildInfo {
    get_from(&BUILD_INFO)
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

    const A: BuildInfo = BuildInfo {
        git_sha: "abc1234",
        commit_time: "2026-01-01T00:00:00+00:00",
    };
    const B: BuildInfo = BuildInfo {
        git_sha: "def5678",
        commit_time: "2026-02-02T00:00:00+00:00",
    };

    #[test]
    fn uninitialised_lock_reports_unknown() {
        let lock = OnceLock::new();
        assert_eq!(get_from(&lock), BuildInfo::UNKNOWN);
    }

    #[test]
    fn set_then_get_returns_exact_values() {
        let lock = OnceLock::new();
        set_in(&lock, A);
        assert_eq!(get_from(&lock), A);
    }

    #[test]
    fn first_set_wins_and_second_does_not_override() {
        let lock = OnceLock::new();
        set_in(&lock, A);
        set_in(&lock, B);
        assert_eq!(get_from(&lock), A);
    }

    #[test]
    fn public_init_and_get_are_consistent() {
        // Process-global: another test may have initialised it first, so only
        // assert that `get()` matches whichever value won.
        init(A);
        let got = get();
        assert_ne!(got, BuildInfo::UNKNOWN);
        init(B);
        assert_eq!(get(), got);
        assert!(VERSION.len() > 1);
    }
}
