//! OS-level process utilities used by multiple modules.

/// Check whether a process with the given PID is still alive.
///
/// Uses `kill(pid, 0)` — sends no signal; returns `true` if the process exists
/// and we have permission to signal it. Returns `false` on `ESRCH` (no such
/// process). Returns `true` on `EPERM` (process exists but unowned — treated
/// conservatively as alive to avoid false-positive reaping).
#[cfg(unix)]
pub fn pid_is_alive(pid: u32) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(unix)]
    fn pid_is_alive_current_process() {
        // The current process must be alive.
        assert!(super::pid_is_alive(std::process::id()));
    }

    #[test]
    #[cfg(unix)]
    fn pid_is_alive_after_child_exits() {
        // Spawn a short-lived child, wait for it to exit, then confirm its PID
        // is no longer alive. Note: there is a theoretical PID-reuse race, but
        // it is negligible in practice under test conditions.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!super::pid_is_alive(pid));
    }
}
