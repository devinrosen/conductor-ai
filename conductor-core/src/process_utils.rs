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
