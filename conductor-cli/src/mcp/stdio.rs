//! Stdio isolation for `conductor mcp serve` (#3093).
//!
//! Workflows started through MCP run inside the server process, and the
//! `claude` and `sh` children they spawn inherit fd 0 and fd 1 — the JSON-RPC
//! request and response pipes. `claude` reads a non-TTY stdin, so it can swallow
//! a request such as `conductor_get_run` and the client never gets an answer.
//! A child writing to fd 1 can equally corrupt the response stream.
//!
//! `isolate_stdio` moves the protocol onto private close-on-exec fds and points
//! fd 0 at `/dev/null` and fd 1 at stderr, so everything spawned afterwards
//! inherits harmless stdio.

use std::fs::File;
use std::io;
use std::os::unix::io::{FromRawFd, RawFd};

fn with_context(e: io::Error, what: impl std::fmt::Display) -> io::Error {
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// Duplicate `fd` to the lowest free fd >= 3 with close-on-exec set.
fn dup_cloexec(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: fcntl with F_DUPFD_CLOEXEC has no memory-safety requirements; the
    // return value is checked below.
    let new_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if new_fd < 0 {
        Err(with_context(
            io::Error::last_os_error(),
            format!("failed to duplicate fd {fd} for MCP transport"),
        ))
    } else {
        Ok(new_fd)
    }
}

fn dup2_checked(src: RawFd, dst: RawFd) -> io::Result<()> {
    // SAFETY: dup2 has no memory-safety requirements; the return value is checked.
    if unsafe { libc::dup2(src, dst) } < 0 {
        Err(with_context(
            io::Error::last_os_error(),
            format!("failed to redirect fd {dst}"),
        ))
    } else {
        Ok(())
    }
}

/// Move the MCP transport off fd 0/1. Returns `(reader, writer)` for the
/// JSON-RPC streams. Call once, before any child process is spawned.
pub fn isolate_stdio() -> io::Result<(File, File)> {
    let in_fd = dup_cloexec(0)?;
    // SAFETY: in_fd was just created by us and is exclusively owned here.
    let reader = unsafe { File::from_raw_fd(in_fd) };
    let out_fd = dup_cloexec(1)?;
    // SAFETY: out_fd was just created by us and is exclusively owned here.
    let writer = unsafe { File::from_raw_fd(out_fd) };

    let devnull =
        File::open("/dev/null").map_err(|e| with_context(e, "failed to open /dev/null"))?;
    {
        use std::os::unix::io::AsRawFd;
        dup2_checked(devnull.as_raw_fd(), 0)?;
    }
    drop(devnull);
    dup2_checked(2, 1)?;

    Ok((reader, writer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};

    const CHILD_ENV: &str = "CONDUCTOR_TEST_STDIO_CHILD";

    /// Re-exec pattern: isolating stdio inside the test harness process would
    /// break the harness, so the real work runs in a child copy of this test.
    #[test]
    fn test_isolate_stdio_hides_transport_from_children() {
        if std::env::var_os(CHILD_ENV).is_some() {
            let (reader, mut writer) = isolate_stdio().expect("isolate_stdio");
            // fd 0 is /dev/null now, so cat exits at once; fd 1 is stderr.
            let status = Command::new("sh")
                .args(["-c", "cat; echo child-done"])
                .status()
                .expect("spawn sh");
            assert!(status.success());
            let mut line = String::new();
            BufReader::new(reader).read_line(&mut line).expect("read");
            write!(writer, "got:{line}").expect("write");
            writer.flush().expect("flush");
            return;
        }

        let exe = std::env::current_exe().expect("current_exe");
        let mut child = Command::new(exe)
            .args([
                "--exact",
                "mcp::stdio::tests::test_isolate_stdio_hides_transport_from_children",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn child test");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"ping\n")
            .expect("write ping");
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .expect("read stdout");
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .expect("read stderr");
        let status = child.wait().expect("wait");
        assert!(status.success(), "child failed: {stdout}\n{stderr}");

        assert!(stdout.contains("got:ping"), "transport lost: {stdout}");
        assert!(
            !stdout.contains("child-done"),
            "child output leaked into transport stdout: {stdout}"
        );
        assert!(
            stderr.contains("child-done"),
            "child stdout should land on stderr: {stderr}"
        );
        assert_eq!(
            stdout.matches("ping").count(),
            1,
            "cat must not have consumed or echoed the request: {stdout}"
        );
    }
}
