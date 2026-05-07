//! Test-only helpers that prevent the unit-test suite from touching the
//! developer's real `~/.conductor` directory.
//!
//! `conductor_core::config::conductor_dir()` resolves the global data dir
//! once via `OnceLock`, honoring `CONDUCTOR_HOME` if it's set when the
//! lookup happens. Several `App` handler tests (settings → runtimes,
//! workflow exec config, etc.) construct in-memory fixtures that, when
//! exercised, eventually call `save_config` or read paths derived from
//! `conductor_dir()`. Without isolation those writes land in the
//! developer's real `~/.conductor/config.toml`.
//!
//! [`isolate_conductor_home`] sets `CONDUCTOR_HOME` to a per-process temp
//! directory exactly once. Every `make_app`-style test helper calls it
//! before constructing `Config::default()` (which is the first call that
//! triggers the `OnceLock` resolution), so the global is never observed.

use std::sync::Once;

static INIT: Once = Once::new();

/// Point `CONDUCTOR_HOME` at a temp dir for the rest of the test process.
/// Idempotent — safe to call from every test helper.
pub(crate) fn isolate_conductor_home() {
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("conductor-tui-tests-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("CONDUCTOR_HOME", dir);
    });
}
