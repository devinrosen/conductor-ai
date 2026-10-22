//! Background workflow execution for Unix systems.
//!
//! When `conductor workflow run --background` is used, the CLI starts a fresh
//! copy of itself (same arguments, minus `--background`, plus a hidden
//! `--background-notify-fd <fd>`) in a new session, and reads the workflow run
//! ID back through a pipe. The parent prints the ID and exits immediately.
//!
//! # Process boundary
//!
//! - **Parent** ([`spawn_background_workflow`]): starts the child, reads the run
//!   ID (or an `ERROR:` line) from the pipe, returns.
//! - **Child** ([`run_as_background_child`]): resolves the same workflow, starts
//!   it, writes the run ID to the pipe, redirects stdio to `/dev/null`, drives the
//!   workflow to completion and exits.
//!
//! # Why not `fork()`
//!
//! The parent has the SQLite database open when it starts the child. After a
//! `fork()` the child inherits SQLite's per-process state for that file (inode
//! and WAL shared-memory bookkeeping) without the POSIX locks it describes, and
//! a "fresh" connection opened in the child reuses that state. With other
//! processes active in the WAL, the child's writes then fail with
//! `SQLITE_PROTOCOL` ("locking protocol"). See #3051. Starting a new process
//! with exec gives the child a clean SQLite state; SQLite opens its files with
//! `O_CLOEXEC`, so the parent's database descriptors are not inherited.

use std::io::{BufRead, BufReader};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};

use conductor_core::agent::AgentManager;
use conductor_core::workflow::{execute_workflow_standalone, WorkflowExecStandalone};

/// Hidden flag that marks the re-executed child and names its pipe fd.
pub const NOTIFY_FD_FLAG: &str = "--background-notify-fd";

/// The arguments for the background child: the parent's own arguments without
/// `--background`, plus [`NOTIFY_FD_FLAG`] and the pipe's write fd.
fn child_args(
    parent_args: impl IntoIterator<Item = std::ffi::OsString>,
    write_fd: i32,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = parent_args
        .into_iter()
        .filter(|a| a != "--background")
        .collect();
    args.push(NOTIFY_FD_FLAG.into());
    args.push(write_fd.to_string().into());
    args
}

/// Start this same `conductor workflow run` invocation as a detached background
/// process and wait for it to report the run ID. Returns the run ID string.
pub fn spawn_background_workflow() -> Result<String> {
    // Create a pipe for the child to send the run ID back to the parent.
    let mut fds = [0i32; 2];
    // SAFETY: `pipe` writes two valid file descriptors into `fds`. We check the
    // return value before using them.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 {
        anyhow::bail!("pipe() failed: {}", std::io::Error::last_os_error());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    // The child only needs the write end; keep the read end out of it.
    // SAFETY: read_fd is a valid fd from pipe().
    unsafe {
        libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let exe = std::env::current_exe().context("Failed to locate the conductor executable")?;
    let mut cmd = Command::new(exe);
    cmd.args(child_args(std::env::args_os().skip(1), write_fd))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach from the controlling terminal so the workflow survives if the
    // parent's terminal closes.
    // SAFETY: setsid() is async-signal-safe, which is all pre_exec allows.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let spawned = cmd.spawn();

    // Close the write end in the parent whether or not the spawn worked, so the
    // read below sees EOF once the child closes its copy.
    // SAFETY: write_fd is a valid fd from pipe().
    unsafe {
        libc::close(write_fd);
    }
    if let Err(e) = spawned {
        // SAFETY: read_fd is a valid fd from pipe().
        unsafe {
            libc::close(read_fd);
        }
        anyhow::bail!("Failed to start background workflow process: {e}");
    }

    // Wrap the read end in a File so Rust manages its lifetime.
    // SAFETY: read_fd is a valid fd from pipe(); ownership transfers to File.
    let read_file = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut reader = BufReader::new(read_file);

    // Read the first line -- either the run ID or an error prefixed with
    // "ERROR:". A ULID is 26 chars; errors are short strings.
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

/// Entry point for the re-executed background child: starts the workflow,
/// writes the run ID (or an error) to `write_fd`, then drives the workflow to
/// completion.
///
/// This function never returns -- it calls `std::process::exit`.
pub fn run_as_background_child(mut params: WorkflowExecStandalone, write_fd: i32) -> ! {
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
    //
    // Returns Ok(()) on success or Err(error_string) so the main thread can
    // detect post-notification failures and clean up the DB.
    let exec_handle = std::thread::spawn(move || -> std::result::Result<(), String> {
        match execute_workflow_standalone(&params) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                *error_slot_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                // Wake the main thread so it can surface the error.
                notify_pair_bg.1.notify_one();
                Err(msg)
            }
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
    let exec_result = exec_handle.join();

    // Check if the exec thread returned an error (or panicked) AFTER we
    // already sent the run ID to the parent. In that case the workflow_run
    // row exists in the DB but may still be in pending/running status. We
    // must mark it (and the parent agent run) as failed.
    let exec_failed = match &exec_result {
        Ok(Ok(())) => false,
        Ok(Err(_)) => true, // exec thread returned an error
        Err(_) => true,     // exec thread panicked
    };

    if exec_failed {
        let error_msg = match &exec_result {
            Ok(Err(e)) => e.clone(),
            Err(_) => "Background workflow thread panicked".to_string(),
            _ => unreachable!(),
        };

        // Extract the run ID from the notify pair (it was set before notification).
        let run_id = notify_pair
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        if let Some(run_id) = run_id {
            // Best-effort cleanup: open a fresh DB connection and mark the
            // workflow run + parent agent run as failed. Errors here are
            // silently ignored -- we cannot do anything useful with them in
            // a detached background process.
            let _ = cleanup_failed_run(&run_id, &error_msg);
        }

        std::process::exit(1);
    }

    std::process::exit(0);
}

/// Best-effort cleanup when a background workflow fails after the run ID
/// was already sent to the parent process.
///
/// Opens a fresh DB connection (the exec thread's connection may be in an
/// inconsistent state) and marks both the workflow run and its parent agent
/// run as failed.
fn cleanup_failed_run(workflow_run_id: &str, error_msg: &str) -> std::result::Result<(), ()> {
    let db_path = conductor_core::config::db_path();
    let conn = conductor_core::db::open_database(&db_path).map_err(|_| ())?;

    // Fail the workflow run and get the parent run ID
    if let Ok(parent_run_id) =
        conductor_core::workflow::fail_workflow_run(&conn, workflow_run_id, error_msg)
    {
        // Best-effort: update the parent agent run
        let agent_mgr = AgentManager::new(&conn);
        let _ = agent_mgr.update_run_failed(&parent_run_id, error_msg);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn child_args_drops_background_and_adds_notify_fd() {
        let args = child_args(
            os(&[
                "workflow",
                "run",
                "review-pr",
                "repo",
                "wt",
                "--background",
                "--input",
                "k=v",
            ]),
            7,
        );
        assert_eq!(
            args,
            os(&[
                "workflow",
                "run",
                "review-pr",
                "repo",
                "wt",
                "--input",
                "k=v",
                NOTIFY_FD_FLAG,
                "7"
            ])
        );
    }

    #[test]
    fn child_args_keeps_values_that_merely_contain_background() {
        let args = child_args(
            os(&["workflow", "run", "x", "--input", "mode=--background-ish"]),
            3,
        );
        assert!(args.contains(&OsString::from("mode=--background-ish")));
        assert!(!args.contains(&OsString::from("--background")));
    }
}
