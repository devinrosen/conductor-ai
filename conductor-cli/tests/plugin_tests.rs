//! Tests for the Claude Code plugin in `plugin/` (RFC 021): manifest shape,
//! version guard, and the no-op behaviour of the hook wrapper.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod common;

const WRAPPER_COMMAND: &str = "\"${CLAUDE_PLUGIN_ROOT}/hooks/conductor-session-hook\"";

fn repo_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

fn plugin_dir() -> PathBuf {
    repo_root().join("plugin")
}

fn wrapper() -> PathBuf {
    plugin_dir().join("hooks/conductor-session-hook")
}

fn read_json(path: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn marketplace_manifest_shape() {
    let m = read_json(&repo_root().join(".claude-plugin/marketplace.json"));
    assert_eq!(m["name"], "conductor-ai");
    let plugins = m["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["name"], "conductor");
    assert_eq!(plugins[0]["source"], "./plugin");
}

#[test]
fn plugin_manifest_version_matches_workspace() {
    let p = read_json(&plugin_dir().join(".claude-plugin/plugin.json"));
    assert_eq!(p["name"], "conductor");
    assert_eq!(
        p["version"],
        env!("CARGO_PKG_VERSION"),
        "bump plugin/.claude-plugin/plugin.json in the release commit"
    );
    assert!(p.get("mcpServers").is_none());
    assert!(!plugin_dir().join(".mcp.json").exists());
}

#[test]
fn hooks_json_matches_handled_events() {
    let h = read_json(&plugin_dir().join("hooks/hooks.json"));
    let hooks = h["hooks"].as_object().unwrap();
    let mut registered: Vec<&str> = hooks.keys().map(String::as_str).collect();
    registered.sort_unstable();
    let mut handled: Vec<&str> = conductor_core::session::hook::handled_event_names().collect();
    handled.sort_unstable();
    assert_eq!(registered, handled);

    for (event, entries) in hooks {
        let entries = entries.as_array().unwrap();
        assert_eq!(entries.len(), 1, "{event}");
        let needs_matcher = matches!(
            event.as_str(),
            "PermissionRequest" | "PostToolUse" | "PostToolUseFailure"
        );
        if needs_matcher {
            assert_eq!(entries[0]["matcher"], "*", "{event}");
        }
        let cmds = entries[0]["hooks"].as_array().unwrap();
        assert_eq!(cmds.len(), 1, "{event}");
        assert_eq!(cmds[0]["type"], "command", "{event}");
        assert_eq!(cmds[0]["command"], WRAPPER_COMMAND, "{event}");
        assert_eq!(cmds[0]["timeout"], 5, "{event}");
    }
}

#[test]
fn wrapper_is_executable() {
    let mode = std::fs::metadata(wrapper()).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "wrapper must be executable");
}

// ---------------------------------------------------------------------------
// Wrapper behaviour
// ---------------------------------------------------------------------------

/// Write an executable script `name` into `dir`.
fn write_script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run the wrapper with a clean environment, feeding `stdin` from a thread.
fn run_wrapper(home: &Path, path_dirs: &[&Path], attended: Option<&str>, stdin: Vec<u8>) -> Output {
    let mut cmd = Command::new("/bin/sh");
    common::apply_fixed_env(&mut cmd, home, path_dirs);
    cmd.arg(wrapper())
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(a) = attended {
        cmd.env("CLAUDE_CODE_SESSION_ATTENDED", a);
    }
    let mut child = cmd.spawn().unwrap();
    let mut sin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || sin.write_all(&stdin));
    let out = child.wait_with_output().unwrap();
    writer
        .join()
        .unwrap()
        .expect("wrapper must drain stdin without a broken pipe");
    out
}

fn assert_silent_success(out: &Output) {
    assert!(out.status.success(), "exit status: {:?}", out.status);
    assert!(out.stdout.is_empty(), "stdout: {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "stderr: {:?}", out.stderr);
}

fn small_payload() -> Vec<u8> {
    br#"{"hook_event_name":"SessionStart","session_id":"s","cwd":"/nowhere"}"#.to_vec()
}

#[test]
fn wrapper_skips_unattended_and_unset_without_starting_binary() {
    assert!(!Path::new("/bin/conductor").exists() && !Path::new("/usr/bin/conductor").exists());
    for attended in [Some("0"), None] {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let marker = home.path().join("ran");
        write_script(
            bin.path(),
            "conductor",
            &format!("touch '{}'", marker.display()),
        );
        let out = run_wrapper(home.path(), &[bin.path()], attended, small_payload());
        assert_silent_success(&out);
        assert!(!marker.exists(), "binary started for attended={attended:?}");
    }
}

#[test]
fn wrapper_exits_silently_without_conductor_on_path() {
    assert!(!Path::new("/bin/conductor").exists() && !Path::new("/usr/bin/conductor").exists());
    let home = tempfile::tempdir().unwrap();
    let out = run_wrapper(home.path(), &[], Some("1"), small_payload());
    assert_silent_success(&out);
}

#[test]
fn wrapper_swallows_old_binary_failure() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    write_script(
        bin.path(),
        "conductor",
        "cat >/dev/null; echo 'usage: conductor' ; echo 'error: unknown subcommand' >&2; exit 2",
    );
    let out = run_wrapper(home.path(), &[bin.path()], Some("1"), small_payload());
    assert_silent_success(&out);
}

#[test]
fn wrapper_drains_large_stdin_on_early_exit() {
    let home = tempfile::tempdir().unwrap();
    let out = run_wrapper(home.path(), &[], Some("0"), vec![b'x'; 4 * 1024 * 1024]);
    assert_silent_success(&out);
}

// ---------------------------------------------------------------------------
// Wrapper + real binary
// ---------------------------------------------------------------------------

fn real_bin_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_conductor"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn session_start(cwd: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-1",
        "cwd": cwd,
    }))
    .unwrap()
}

#[test]
fn wrapper_with_real_binary_ignores_cwd_outside_worktree() {
    let home = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(home.path());
    let bin = real_bin_dir();
    let out = run_wrapper(
        home.path(),
        &[&bin],
        Some("1"),
        session_start("/tmp/elsewhere"),
    );
    assert_silent_success(&out);
    assert_eq!(
        conductor_core::test_helpers::count_rows(&conn, "agent_runs"),
        0
    );
}

#[test]
fn wrapper_with_real_binary_forwards_to_active_worktree() {
    let home = tempfile::tempdir().unwrap();
    let conn = conductor_core::test_helpers::seed_hook_db(home.path());
    let bin = real_bin_dir();
    let out = run_wrapper(
        home.path(),
        &[&bin],
        Some("1"),
        session_start("/tmp/ws/hook-wt"),
    );
    assert_silent_success(&out);
    assert_eq!(
        conductor_core::test_helpers::count_rows(&conn, "agent_runs"),
        1
    );
}
