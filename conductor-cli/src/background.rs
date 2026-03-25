//! Fork-based background workflow execution for Unix systems.
//!
//! When `conductor workflow run --background` is used, the CLI forks a child
//! process that detaches from the controlling terminal (via `setsid`) and drives
//! the workflow to completion. The parent reads the workflow run ID from a pipe,
//! prints it, and exits immediately.
//!
//! # Process boundary
//!
//! - **Parent**: reads the run ID from the pipe (blocking), prints it, returns.
//! - **Child**: calls `setsid()`, redirects stdio to `/dev/null`, opens a fresh
//!   DB connection (via `execute_workflow_standalone`), runs the workflow, and
//!   exits.
//!
//! The child never inherits the parent's `rusqlite::Connection` -- it opens its
//! own. SQLite WAL mode ensures concurrent readers/writers are safe at the
//! database level.

use std::io::{BufRead, BufReader};
use std::os::unix::io::FromRawFd;
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};

use conductor_core::workflow::{execute_workflow_standalone, WorkflowExecStandalone};

/// Fork the current process. The child detaches and runs the workflow in the
/// background. The parent blocks until the child signals the run ID (or an
/// error), then returns the run ID string.
pub fn fork_and_run_workflow(params: WorkflowExecStandalone) -> Result<String> {
    // Create a pipe for the child to send the run ID back to the parent.
    let mut fds = [0i32; 2];
    // SAFETY: `pipe` writes two valid file descriptors into `fds`. We check the
    // return value before using them.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        anyhow::bail!("pipe() failed: {}", std::io::Error::last_os_error());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);

    // SAFETY: We call `fork()` before spawning any threads in the child. After
    // fork only async-signal-safe functions are called until we reach safe Rust
    // code in `child_main`. The parent merely reads from the pipe and returns.
    let pid = unsafe { libc::fork() };

    match pid {
        -1 => {
            // Fork failed -- close pipe fds and report.
            // SAFETY: both fds are valid (pipe succeeded above).
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            anyhow::bail!("fork() failed: {}", std::io::Error::last_os_error());
        }
        0 => {
            // ---------- CHILD PROCESS ----------
            // This function never returns.
            child_main(params, read_fd, write_fd);
        }
        _child_pid => {
            // ---------- PARENT PROCESS ----------
            // Close the write end; we only read.
            // SAFETY: write_fd is a valid fd from pipe().
            unsafe {
                libc::close(write_fd);
            }

            // Wrap the read end in a File so Rust manages its lifetime.
            // SAFETY: read_fd is a valid fd from pipe(); ownership transfers to File.
            let read_file = unsafe { std::fs::File::from_raw_fd(read_fd) };
            let mut reader = BufReader::new(read_file);

            // Read the first line -- either the run ID or an error prefixed
            // with "ERROR:". A ULID is 26 chars; errors are short strings. No
            // need for an explicit length guard.
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .context("Failed to read run ID from background process")?;

            let line = line.trim().to_string();

            if line.is_empty() {
                anyhow::bail!(
                    "Background process exited without sending a run ID. \
                     Check logs for errors."
                );
            }

            if let Some(err_msg) = line.strip_prefix("ERROR:") {
                anyhow::bail!("Background workflow failed to start: {}", err_msg.trim());
            }

            Ok(line)
        }
    }
}

/// Child process entry point. Detaches from the terminal, runs the workflow,
/// and writes the run ID to the pipe.
///
/// This function never returns -- it calls `std::process::exit`.
fn child_main(mut params: WorkflowExecStandalone, read_fd: i32, write_fd: i32) -> ! {
    // Detach from the controlling terminal so the workflow survives if the
    // parent's terminal closes.
    // SAFETY: setsid() is async-signal-safe and has no preconditions beyond
    // "the calling process is not a process group leader", which holds because
    // we just forked.
    unsafe {
        libc::setsid();
    }

    // Close the read end of the pipe; the child only writes.
    // SAFETY: read_fd is a valid fd from pipe().
    unsafe {
        libc::close(read_fd);
    }

    // Set up the run_id_notify mechanism. When execute_workflow creates the run
    // record, it writes the ID into the Mutex and signals the Condvar -- all
    // synchronously within execute_workflow before any steps execute. We use a
    // background thread to drive execution so we can wait on the condvar from
    // the main thread and write to the pipe promptly.
    let notify_pair: Arc<(Mutex<Option<String>>, Condvar)> =
        Arc::new((Mutex::new(None), Condvar::new()));
    params.run_id_notify = Some(Arc::clone(&notify_pair));

    // Slot to capture early startup errors (before the run record is created).
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_slot_bg = Arc::clone(&error_slot);
    let notify_pair_bg = Arc::clone(&notify_pair);

    // Spawn the workflow execution in a background thread. WorkflowExecStandalone
    // is Send (all owned types + Arc), and execute_workflow_standalone opens its
    // own DB connection.
    let exec_handle = std::thread::spawn(move || {
        if let Err(e) = execute_workflow_standalone(&params) {
            *error_slot_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            // Wake the main thread so it can surface the error.
            notify_pair_bg.1.notify_one();
        }
    });

    // Wait (up to 30 s) for either the run ID or a startup error.
    let (lock, cvar) = notify_pair.as_ref();
    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let (guard, _timed_out) = cvar
        .wait_timeout_while(guard, std::time::Duration::from_secs(30), |v| {
            v.is_none()
                && error_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
        })
        .unwrap_or_else(|e| e.into_inner());

    // Build the message to send through the pipe.
    let message = if let Some(err) = error_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        format!("ERROR:{}\n", err)
    } else {
        match guard.as_ref() {
            Some(id) => format!("{}\n", id),
            None => "ERROR:Timed out waiting for workflow run ID (30s)\n".to_string(),
        }
    };
    // Drop the guard before writing to avoid holding the lock.
    drop(guard);

    // Write the message to the pipe.
    // SAFETY: write_fd is a valid fd from pipe(). We use libc::write which is
    // async-signal-safe, though at this point we are well past the fork danger zone.
    let msg_bytes = message.as_bytes();
    unsafe {
        libc::write(
            write_fd,
            msg_bytes.as_ptr() as *const libc::c_void,
            msg_bytes.len(),
        );
        libc::close(write_fd);
    }

    // If we sent an error, exit immediately.
    if message.starts_with("ERROR:") {
        std::process::exit(1);
    }

    // Redirect stdio to /dev/null now that the parent has the run ID.
    redirect_stdio_to_devnull();

    // Wait for the workflow execution thread to finish.
    let _ = exec_handle.join();

    std::process::exit(0);
}

/// Redirect stdin, stdout, and stderr to `/dev/null`.
///
/// Called after the run ID has been sent through the pipe so the detached child
/// does not hold the terminal's file descriptors open.
fn redirect_stdio_to_devnull() {
    // SAFETY: We open /dev/null (always available on Unix) and dup2 it onto the
    // standard file descriptors. This is standard Unix daemonization practice.
    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull < 0 {
            return; // Best effort -- don't crash the workflow over this.
        }
        libc::dup2(devnull, 0); // stdin
        libc::dup2(devnull, 1); // stdout
        libc::dup2(devnull, 2); // stderr
        if devnull > 2 {
            libc::close(devnull);
        }
    }
}
