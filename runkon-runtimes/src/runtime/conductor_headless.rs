//! Conductor-headless CLI argv builder for `ClaudeRuntime`.
//!
//! This module owns the `conductor agent run` argv shape — not a generic
//! primitive. Future runtimes construct their own argv (or HTTP body, etc.)
//! without inheriting this conductor-CLI-specific shape.

use std::borrow::Cow;

use crate::headless::{spawn_headless, HeadlessHandle};
use crate::permission::PermissionMode;

pub const DEFAULT_AGENT_ERROR_MSG: &str = "Claude reported an error";

/// Maximum number of CLI arguments produced by `build_headless_agent_args`.
const AGENT_ARGS_CAPACITY: usize = 20;

fn push_optional_agent_flags(
    args: &mut Vec<Cow<'static, str>>,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    permission_mode: Option<&PermissionMode>,
    extra_cli_args: &[(Cow<'static, str>, Cow<'static, str>)],
    extra_plugin_dirs: &[String],
) {
    if let Some(id) = resume_session_id {
        args.push(Cow::Borrowed("--resume"));
        args.push(Cow::Owned(id.to_string()));
    }
    if let Some(m) = model {
        args.push(Cow::Borrowed("--model"));
        args.push(Cow::Owned(m.to_string()));
    }
    if let Some(mode) = permission_mode {
        if let Some(val) = mode.cli_flag_value() {
            args.push(Cow::Borrowed("--permission-mode"));
            args.push(Cow::Owned(val.to_string()));
        }
    }
    for (flag, val) in extra_cli_args {
        args.push(flag.clone());
        args.push(val.clone());
    }
    for dir in extra_plugin_dirs {
        args.push(Cow::Borrowed("--plugin-dir"));
        args.push(Cow::Owned(dir.clone()));
    }
}

/// Write `prompt` to a temp file with mode 0o600 (Unix) and return the path.
fn write_prompt_file(
    run_id: &str,
    prompt: &str,
) -> std::result::Result<std::path::PathBuf, String> {
    let prompt_file_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&prompt_file_path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(prompt.as_bytes())
            })
            .map_err(|e| {
                format!(
                    "Failed to write prompt file '{}': {e}",
                    prompt_file_path.display()
                )
            })?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(&prompt_file_path, prompt).map_err(|e| {
            format!(
                "Failed to write prompt file '{}': {e}",
                prompt_file_path.display()
            )
        })?;
    }

    Ok(prompt_file_path)
}

/// Parameters for spawning a headless agent subprocess.
pub struct SpawnHeadlessParams<'a> {
    pub run_id: &'a str,
    pub working_dir: &'a str,
    pub prompt: &'a str,
    pub resume_session_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub extra_cli_args: &'a [(Cow<'static, str>, Cow<'static, str>)],
    pub permission_mode: Option<&'a PermissionMode>,
    pub plugin_dirs: &'a [String],
}

/// Build headless args and spawn the conductor subprocess in one step.
#[cfg(unix)]
pub fn try_spawn_headless_run(
    params: &SpawnHeadlessParams<'_>,
    binary_path: &str,
) -> std::result::Result<(HeadlessHandle, std::path::PathBuf), String> {
    let (args, pf) = build_headless_agent_args(params).map_err(|e| {
        format!(
            "failed to prepare agent args for run {} (working_dir={}): {e}",
            params.run_id, params.working_dir
        )
    })?;
    let h = spawn_headless(&args, std::path::Path::new(params.working_dir), binary_path).map_err(
        |e| {
            let _ = std::fs::remove_file(&pf);
            format!(
                "spawn failed for run {} (working_dir={}): {e}",
                params.run_id, params.working_dir
            )
        },
    )?;
    Ok((h, pf))
}

/// Build `conductor agent run` args for a headless launch.
pub fn build_headless_agent_args(
    params: &SpawnHeadlessParams<'_>,
) -> std::result::Result<(Vec<Cow<'static, str>>, std::path::PathBuf), String> {
    crate::text_util::validate_run_id(params.run_id).map_err(|e| e.to_string())?;

    let run_id = params.run_id;
    let working_dir = params.working_dir;
    let prompt = params.prompt;
    let resume_session_id = params.resume_session_id;
    let model = params.model;
    let extra_cli_args = params.extra_cli_args;
    let permission_mode = params.permission_mode;
    let extra_plugin_dirs = params.plugin_dirs;

    let prompt_file_path = write_prompt_file(run_id, prompt)?;

    let mut args: Vec<Cow<'static, str>> = Vec::with_capacity(AGENT_ARGS_CAPACITY + 2);
    args.push(Cow::Borrowed("agent"));
    args.push(Cow::Borrowed("run"));
    args.push(Cow::Borrowed("--run-id"));
    args.push(Cow::Owned(run_id.to_string()));
    args.push(Cow::Borrowed("--worktree-path"));
    args.push(Cow::Owned(working_dir.to_string()));
    args.push(Cow::Borrowed("--prompt-file"));
    args.push(Cow::Owned(prompt_file_path.to_string_lossy().into_owned()));

    push_optional_agent_flags(
        &mut args,
        resume_session_id,
        model,
        permission_mode,
        extra_cli_args,
        extra_plugin_dirs,
    );

    Ok((args, prompt_file_path))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    fn assert_file_prompt(args: &[Cow<'static, str>], expected_content: &str, expected_path: &str) {
        let file_idx = args
            .iter()
            .position(|a| a == "--prompt-file")
            .expect("--prompt-file flag missing");
        let file_path: &str = args[file_idx + 1].as_ref();
        assert_eq!(file_path, expected_path, "prompt file path mismatch");
        assert!(
            std::path::Path::new(file_path).exists(),
            "prompt file should have been written"
        );
        assert_eq!(
            std::fs::read_to_string(file_path).unwrap(),
            expected_content
        );
        assert!(
            !args.iter().any(|a| a == "--prompt"),
            "--prompt should not appear"
        );
    }

    fn make_params<'a>(
        run_id: &'a str,
        prompt: &'a str,
        resume_session_id: Option<&'a str>,
        model: Option<&'a str>,
        extra_cli_args: &'a [(Cow<'static, str>, Cow<'static, str>)],
    ) -> super::SpawnHeadlessParams<'a> {
        super::SpawnHeadlessParams {
            run_id,
            working_dir: "/tmp/wt",
            prompt,
            resume_session_id,
            model,
            extra_cli_args,
            permission_mode: None,
            plugin_dirs: &[],
        }
    }

    #[test]
    fn build_agent_args_short_prompt_uses_file() {
        let run_id = "run-short-1";
        let prompt = "short prompt";
        let (args, _) =
            super::build_headless_agent_args(&make_params(run_id, prompt, None, None, &[]))
                .unwrap();
        let expected_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));
        assert_file_prompt(&args, prompt, expected_path.to_str().unwrap());
        let _ = std::fs::remove_file(&expected_path);
    }

    #[test]
    fn build_agent_args_long_prompt_uses_file() {
        let run_id = "run-long-99";
        let prompt = "x".repeat(513);
        let (args, _) =
            super::build_headless_agent_args(&make_params(run_id, &prompt, None, None, &[]))
                .unwrap();
        let expected_path = std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt"));
        assert_file_prompt(&args, &prompt, expected_path.to_str().unwrap());
        let _ = std::fs::remove_file(&expected_path);
    }

    #[test]
    fn build_agent_args_with_resume_sets_flag() {
        let run_id = "run-resume-sets-flag";
        let (args, _) = super::build_headless_agent_args(&make_params(
            run_id,
            "short prompt",
            Some("sess-abc"),
            None,
            &[],
        ))
        .unwrap();
        let resume_idx = args
            .iter()
            .position(|a| a == "--resume")
            .expect("--resume flag missing");
        assert_eq!(args[resume_idx + 1], "sess-abc");
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_model_override() {
        let run_id = "run-model-override";
        let (args, _) = super::build_headless_agent_args(&make_params(
            run_id,
            "prompt",
            None,
            Some("claude-sonnet-4-6"),
            &[],
        ))
        .unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--model")
            .expect("expected --model flag");
        assert_eq!(args[idx + 1], "claude-sonnet-4-6");
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[test]
    fn build_agent_args_with_extra_cli_args() {
        let run_id = "run-extra-cli-args-01";
        let extra = [(
            Cow::Borrowed("--custom-flag"),
            Cow::Owned("custom-value".to_string()),
        )];
        let (args, _) =
            super::build_headless_agent_args(&make_params(run_id, "prompt", None, None, &extra))
                .unwrap();
        let idx = args
            .iter()
            .position(|a| a == "--custom-flag")
            .expect("expected --custom-flag flag");
        assert_eq!(args[idx + 1], "custom-value");
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("conductor-prompt-{run_id}.txt")),
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_agent_args_prompt_file_mode_0o600() {
        use std::os::unix::fs::MetadataExt;
        let run_id = "run-perm-600-01";
        let (args, _) = super::build_headless_agent_args(&make_params(
            run_id,
            "secret prompt",
            None,
            None,
            &[],
        ))
        .unwrap();
        let file_idx = args
            .iter()
            .position(|a| a == "--prompt-file")
            .expect("--prompt-file flag missing");
        let file_path = std::path::Path::new(args[file_idx + 1].as_ref());
        let mode = std::fs::metadata(file_path)
            .expect("prompt file must exist")
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "prompt file must have mode 0o600, got {:#o}",
            mode & 0o777
        );
        let _ = std::fs::remove_file(file_path);
    }
}
