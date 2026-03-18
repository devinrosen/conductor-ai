use std::process::Command;

use crate::error::{ConductorError, Result};

/// Return a `Command` for `git` rooted at `dir`.
pub(crate) fn git_in(dir: impl AsRef<std::path::Path>) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd
}

/// Run `cmd`, returning its `Output` on success or a `ConductorError::Git` on non-zero exit.
pub(crate) fn check_output(cmd: &mut Command) -> Result<std::process::Output> {
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(ConductorError::Git(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(output)
}
