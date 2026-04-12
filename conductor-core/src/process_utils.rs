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

/// Returns the OS-recorded start time of the given process, or `None` if the
/// information is unavailable (sysctl error, process not found, or non-macOS).
///
/// Uses `sysctl CTL_KERN / KERN_PROC / KERN_PROC_PID` — macOS only.
///
/// # Layout note
/// `kinfo_proc.kp_proc` is an `extern_proc` whose first field is `p_un`, a union
/// whose `p_starttime` variant (`timeval`) lives at byte offset 0. Reading a
/// `timeval` from the start of the sysctl buffer gives the process start time
/// without needing the full (unexported) `kinfo_proc` struct definition.
#[cfg(target_os = "macos")]
pub fn process_started_at(pid: u32) -> Option<std::time::SystemTime> {
    use std::time::{Duration, SystemTime};

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as libc::c_int,
    ];

    // First call with a null buffer to obtain the required buffer size.
    let mut size: libc::size_t = 0;
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 || size < std::mem::size_of::<libc::timeval>() {
        return None;
    }

    // Second call to populate the buffer.
    let mut buf = vec![0u8; size];
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 || size < std::mem::size_of::<libc::timeval>() {
        return None;
    }

    // The first bytes of kinfo_proc are kp_proc.p_un.p_starttime (a timeval at offset 0).
    // SAFETY: buf has at least size_of::<timeval>() bytes, allocated above.
    let tv: libc::timeval =
        unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const libc::timeval) };

    // tv_sec == 0 indicates the kernel returned a zeroed struct (non-existent PID).
    if tv.tv_sec == 0 {
        return None;
    }

    let secs = tv.tv_sec as u64;
    let nanos = (tv.tv_usec as u32).saturating_mul(1_000);
    Some(SystemTime::UNIX_EPOCH + Duration::new(secs, nanos))
}

/// Returns `true` if the process with the given PID appears to have been recycled by the OS
/// after the original subprocess (recorded at `run_started_at`) exited.
///
/// Compares the OS-recorded process start time against `run_started_at`. If they differ by
/// more than 60 seconds the PID was almost certainly reused for a different process.
///
/// Always returns `false` on non-macOS platforms (only macOS exposes `process_started_at`).
/// Returns `false` if `run_started_at` cannot be parsed as RFC 3339 (logs a warning).
/// Returns `false` if the OS start time is unavailable.
#[cfg(target_os = "macos")]
pub fn pid_was_recycled(pid: u32, run_started_at: &str) -> bool {
    let proc_start = match process_started_at(pid) {
        Some(t) => t,
        None => return false,
    };
    let run_start = match chrono::DateTime::parse_from_rfc3339(run_started_at) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "pid_was_recycled: failed to parse run started_at {:?}: {e}",
                run_started_at
            );
            return false;
        }
    };
    let proc_secs = proc_start
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (proc_secs - run_start.timestamp()).abs() > 60
}

/// Always returns `false` on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn pid_was_recycled(_pid: u32, _run_started_at: &str) -> bool {
    false
}

/// Send SIGTERM to the process group of `pid`, wait up to 5 seconds for a
/// graceful exit, then escalate to SIGKILL if the process is still alive.
///
/// Pass `pid` as returned by [`std::process::Child::id`]. The function signals
/// the **process group** (`-pid`) so that any subprocesses spawned by the child
/// (e.g. shell scripts) also receive the signal.
///
/// # Safety / guard
/// `pid == 0` is rejected — `kill(0, sig)` and `kill(-0, sig)` both signal the
/// current process group and would kill conductor itself.
#[cfg(unix)]
pub fn cancel_subprocess(pid: u32) {
    cancel_subprocess_with_grace(pid, std::time::Duration::from_secs(5));
}

/// Inner implementation of [`cancel_subprocess`] with a configurable grace period.
/// Exposed as `pub(crate)` for unit-test coverage of the SIGKILL escalation path.
#[cfg(unix)]
pub(crate) fn cancel_subprocess_with_grace(pid: u32, grace_period: std::time::Duration) {
    if pid == 0 {
        // kill(-0, sig) and kill(0, sig) both signal the current process group —
        // refusing here prevents conductor from accidentally killing itself.
        tracing::warn!("cancel_subprocess: pid 0 is invalid, refusing to signal process group");
        return;
    }

    // Step 1: SIGTERM to entire process group.
    let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("cancel_subprocess: SIGTERM to -{pid} failed: {err}");
    }

    // Step 2: Poll up to grace_period for graceful exit.
    let deadline = std::time::Instant::now() + grace_period;
    loop {
        if !pid_is_alive(pid) {
            return; // Clean exit — done.
        }
        if std::time::Instant::now() >= deadline {
            break; // Timed out — escalate to SIGKILL.
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Step 3: SIGKILL — process group still alive after grace_period.
    tracing::warn!(
        "cancel_subprocess: pid {pid} still alive after {}ms, sending SIGKILL",
        grace_period.as_millis()
    );
    let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("cancel_subprocess: SIGKILL to -{pid} failed: {err}");
    }
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
    #[cfg(target_os = "macos")]
    fn process_started_at_current_process() {
        // The current process must have a recorded start time strictly before now.
        let start = super::process_started_at(std::process::id()).expect("start time unavailable");
        assert!(
            start < std::time::SystemTime::now(),
            "process start time should be in the past"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn process_started_at_dead_process() {
        // Spawn a short-lived child, wait for it to exit, then confirm that
        // process_started_at returns None for the now-dead PID.
        // Note: there is a theoretical PID-reuse race, but it is negligible in practice.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // Give the OS a moment to fully reap the child.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            super::process_started_at(pid).is_none(),
            "process_started_at should return None for a dead PID"
        );
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

    // -----------------------------------------------------------------------
    // cancel_subprocess tests
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn test_cancel_subprocess_terminates_process() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .args(["100"])
            .process_group(0)
            .spawn()
            .expect("failed to spawn sleep");

        let pid = child.id();
        assert!(super::pid_is_alive(pid), "process should be alive before cancel");

        super::cancel_subprocess(pid);

        let _ = child.wait();
        assert!(!super::pid_is_alive(pid), "process should be dead after cancel");
    }

    #[test]
    #[cfg(unix)]
    fn test_cancel_subprocess_pid_zero_is_noop() {
        super::cancel_subprocess(0);
        assert!(
            super::pid_is_alive(std::process::id()),
            "test process should still be alive"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_cancel_subprocess_sigkill_escalation() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 100"])
            .process_group(0)
            .spawn()
            .expect("failed to spawn SIGTERM-ignoring process");

        let pid = child.id();
        assert!(super::pid_is_alive(pid), "process should be alive before cancel");

        super::cancel_subprocess_with_grace(pid, std::time::Duration::from_millis(300));

        let _ = child.wait();
        assert!(
            !super::pid_is_alive(pid),
            "process should be dead after SIGKILL escalation"
        );
    }
}
