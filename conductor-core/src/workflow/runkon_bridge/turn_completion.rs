//! Detect an agent turn that ended before the agent finished its step (#3177),
//! and resume the session to finish it.
//!
//! A subprocess agent step ends when the agent's turn ends. If the agent starts
//! a background task and then ends its turn ("nothing to do until the tests
//! finish"), or leaves its edits uncommitted, the step would otherwise be
//! recorded `completed`. Two signals mark such a turn as unfinished:
//!
//! - Claude Code's own `background_tasks_changed` stream-json event, last seen
//!   before the final `result`, still lists running tasks.
//! - a `can_commit` agent left tracked changes on a tree that was clean before
//!   the step.
//!
//! The loop here resumes the same Claude session with a finish-your-steps
//! prompt, up to [`MAX_FINISH_ATTEMPTS`] times, and fails the step if the turn
//! is still unfinished. Missing `FLOW_OUTPUT` alone is not a trigger (see
//! runkon/runkon#105), but is mentioned in the prompt when a resume happens.

use std::collections::HashMap;
use std::path::Path;

use runkon_flow::constants::metadata_keys as k;
use runkon_flow::engine_error::EngineError;
use runkon_flow::output_schema::OutputSchema;
use runkon_flow::traits::action_executor::ActionOutput;
use runkon_flow_executors::correction::AgentRunOutcome;
use runkon_runtimes::RunHandle;

use crate::agent::log_parsing::{running_background_tasks, BackgroundTask};

/// Maximum number of continuation runs per step.
pub(crate) const MAX_FINISH_ATTEMPTS: u32 = 2;

/// Metadata key recording how many finish-your-steps continuations ran.
pub(crate) const FINISH_TURN_ATTEMPTS: &str = "finish_turn_attempts";

const MAX_LISTED: usize = 10;

/// Tracked files with uncommitted changes in `dir`, or `None` when git cannot
/// tell (the check is then skipped rather than failing the step).
pub(crate) fn tracked_changes(dir: &Path) -> Option<Vec<String>> {
    let Some(path) = dir.to_str() else {
        tracing::warn!(
            "turn completion: working dir {} is not valid UTF-8; skipping tree check",
            dir.display()
        );
        return None;
    };
    match crate::worktree::tracked_changes(path) {
        Ok(files) => Some(files),
        Err(e) => {
            tracing::warn!(
                "turn completion: cannot read git status in {}: {e}; skipping tree check",
                dir.display()
            );
            None
        }
    }
}

/// Why a finished-looking turn is not actually finished.
#[derive(Debug, Clone, Default)]
pub(crate) struct UnfinishedTurn {
    pub background: Vec<BackgroundTask>,
    pub uncommitted: Vec<String>,
}

impl UnfinishedTurn {
    pub(crate) fn is_unfinished(&self) -> bool {
        !self.background.is_empty() || !self.uncommitted.is_empty()
    }

    pub(crate) fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.background.is_empty() {
            let items: Vec<String> = self
                .background
                .iter()
                .take(MAX_LISTED)
                .map(|t| {
                    if t.description.is_empty() {
                        t.task_type.clone()
                    } else {
                        t.description.clone()
                    }
                })
                .collect();
            parts.push(format!(
                "{} background task(s) still running ({})",
                self.background.len(),
                with_more(items, self.background.len())
            ));
        }
        if !self.uncommitted.is_empty() {
            let items: Vec<String> = self.uncommitted.iter().take(MAX_LISTED).cloned().collect();
            parts.push(format!(
                "uncommitted changes in {} tracked file(s) ({})",
                self.uncommitted.len(),
                with_more(items, self.uncommitted.len())
            ));
        }
        parts.join("; ")
    }
}

fn with_more(listed: Vec<String>, total: usize) -> String {
    let mut s = listed.join(", ");
    if total > listed.len() {
        s.push_str(&format!(" and {} more", total - listed.len()));
    }
    s
}

/// The continuation prompt sent into the resumed session.
pub(crate) fn finish_turn_prompt(t: &UnfinishedTurn, missing_flow_output: bool) -> String {
    let mut p = String::from(
        "You ended your turn before finishing this step. The workflow step only ends \
         when your turn does; anything unfinished is lost.\n\n",
    );
    if !t.background.is_empty() {
        p.push_str("Background tasks were still running when your turn ended:\n");
        for task in t.background.iter().take(MAX_LISTED) {
            p.push_str(&format!("- {} ({})\n", task.description, task.task_type));
        }
        p.push_str(
            "Those tasks belonged to the previous process. Their output is no longer \
             available to you, and they may still be running. Re-run any command whose \
             result you need in the **foreground** and wait for it. Do not start background \
             tasks.\n\n",
        );
    }
    if !t.uncommitted.is_empty() {
        p.push_str("These tracked files have uncommitted changes:\n");
        for f in t.uncommitted.iter().take(MAX_LISTED) {
            p.push_str(&format!("- {f}\n"));
        }
        p.push_str(
            "Your instructions require you to commit. Finish the remaining steps and commit.\n\n",
        );
    }
    if missing_flow_output {
        p.push_str("End with the `<<<FLOW_OUTPUT>>>` block your instructions ask for.\n\n");
    }
    p.push_str(
        "Complete every remaining step of your original instructions now. Do not redo \
         work that is already done.",
    );
    p
}

/// Collect the unfinished-turn signals after a run.
///
/// `tree_clean_before` is `Some(true)` only for a `can_commit` agent whose tree
/// was verified clean before the step; the tree check is skipped otherwise.
pub(crate) fn assess(
    log_path: &Path,
    working_dir: &Path,
    tree_clean_before: bool,
) -> UnfinishedTurn {
    let uncommitted = if tree_clean_before {
        tracked_changes(working_dir).unwrap_or_default()
    } else {
        Vec::new()
    };
    UnfinishedTurn {
        background: running_background_tasks(log_path),
        uncommitted,
    }
}

fn add_metric<T: std::str::FromStr + std::ops::Add<Output = T> + ToString>(
    metadata: &mut HashMap<String, String>,
    key: &str,
    new: Option<T>,
) {
    let Some(new) = new else { return };
    let total = match metadata.get(key).and_then(|v| v.parse::<T>().ok()) {
        Some(old) => old + new,
        None => new,
    };
    metadata.insert(key.to_string(), total.to_string());
}

fn add_run_metrics(metadata: &mut HashMap<String, String>, run: &RunHandle) {
    add_metric(metadata, k::COST_USD, run.cost_usd);
    add_metric(metadata, k::NUM_TURNS, run.num_turns);
    add_metric(metadata, k::DURATION_MS, run.duration_ms);
    add_metric(metadata, k::INPUT_TOKENS, run.input_tokens);
    add_metric(metadata, k::OUTPUT_TOKENS, run.output_tokens);
    add_metric(
        metadata,
        k::CACHE_READ_INPUT_TOKENS,
        run.cache_read_input_tokens,
    );
    add_metric(
        metadata,
        k::CACHE_CREATION_INPUT_TOKENS,
        run.cache_creation_input_tokens,
    );
}

fn step_err(step: &str, msg: impl std::fmt::Display) -> EngineError {
    EngineError::Workflow(format!("step '{step}': {msg}"))
}

fn unfinished_error(step: &str, t: &UnfinishedTurn, attempts: u32) -> EngineError {
    let suffix = if attempts > 0 {
        format!(" (after {attempts} finish attempt(s))")
    } else {
        String::new()
    };
    step_err(
        step,
        format_args!(
            "agent ended its turn before finishing: {}{suffix}",
            t.describe()
        ),
    )
}

/// If the agent's turn ended unfinished, resume its session until it finishes
/// or [`MAX_FINISH_ATTEMPTS`] is reached, folding each continuation into
/// `output`. Fails the step when the turn is still unfinished, or when there is
/// no session to resume.
///
/// - `assess` re-collects the signals.
/// - `session_lookup` finds the session id of the finished run; consulted only
///   when the turn is unfinished.
/// - `resume` runs the session with a prompt and the session id.
pub(crate) fn finish_unfinished_turn(
    step: &str,
    output: &mut ActionOutput,
    schema: Option<&OutputSchema>,
    mut assess: impl FnMut() -> UnfinishedTurn,
    session_lookup: impl FnOnce() -> Option<String>,
    mut resume: impl FnMut(&str, &str) -> Result<AgentRunOutcome, String>,
) -> Result<(), EngineError> {
    let mut turn = assess();
    if !turn.is_unfinished() {
        return Ok(());
    }
    let Some(mut session_id) = session_lookup() else {
        return Err(unfinished_error(step, &turn, 0));
    };

    for attempt in 1..=MAX_FINISH_ATTEMPTS {
        tracing::info!(
            "step '{step}': agent ended its turn early ({}); resuming (attempt {attempt}/{MAX_FINISH_ATTEMPTS})",
            turn.describe()
        );
        let prompt = finish_turn_prompt(&turn, output.metadata.contains_key(k::OUTPUT_WARNING));
        let outcome = resume(&prompt, &session_id).map_err(|e| {
            step_err(
                step,
                format_args!(
                    "finish-turn attempt {attempt} failed: {e}; original: {}",
                    turn.describe()
                ),
            )
        })?;
        if outcome.run.status != runkon_runtimes::RunStatus::Completed {
            return Err(step_err(
                step,
                format_args!(
                    "finish-turn attempt {attempt} ended with status {:?}; original: {}",
                    outcome.run.status,
                    turn.describe()
                ),
            ));
        }

        output.markers = outcome.markers;
        output.context = Some(outcome.context);
        output.structured_output = outcome.structured_output;
        output.result_text = outcome.run.result_text.clone();
        add_run_metrics(&mut output.metadata, &outcome.run);
        if outcome.correction_attempts > 0 {
            add_metric(
                &mut output.metadata,
                k::OUTPUT_CORRECTION_ATTEMPTS,
                Some(outcome.correction_attempts),
            );
        }
        output.metadata.remove(k::OUTPUT_WARNING);
        runkon_flow_executors::output::record_flow_output_warning(
            &mut output.metadata,
            output.result_text.as_deref(),
            schema,
        );
        output
            .metadata
            .insert(FINISH_TURN_ATTEMPTS.to_string(), attempt.to_string());
        if let Some(s) = outcome.run.session_id {
            session_id = s;
        }

        turn = assess();
        if !turn.is_unfinished() {
            return Ok(());
        }
    }
    Err(unfinished_error(step, &turn, MAX_FINISH_ATTEMPTS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_log(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f
    }

    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"s1"}"#;
    const RESULT: &str = r#"{"type":"result","subtype":"success","result":"ending turn"}"#;

    #[test]
    fn default_turn_is_not_unfinished() {
        assert!(!UnfinishedTurn::default().is_unfinished());
    }

    #[test]
    fn describe_caps_file_list() {
        let t = UnfinishedTurn {
            uncommitted: (0..15).map(|i| format!("f{i}.rs")).collect(),
            ..Default::default()
        };
        let d = t.describe();
        assert!(d.contains("15 tracked file(s)"), "{d}");
        assert!(d.contains("and 5 more"), "{d}");
        assert!(!d.contains("f14.rs"), "{d}");
    }

    #[test]
    fn prompt_includes_only_set_sections() {
        let bg = UnfinishedTurn {
            background: vec![BackgroundTask {
                task_id: "a".into(),
                task_type: "local_bash".into(),
                description: "cargo test".into(),
            }],
            ..Default::default()
        };
        let p = finish_turn_prompt(&bg, false);
        assert!(p.contains("cargo test") && p.contains("foreground"));
        assert!(!p.contains("uncommitted") && !p.contains("FLOW_OUTPUT"));

        let dirty = UnfinishedTurn {
            uncommitted: vec!["a.rs".into()],
            ..Default::default()
        };
        let p = finish_turn_prompt(&dirty, true);
        assert!(p.contains("a.rs") && p.contains("FLOW_OUTPUT"));
        assert!(!p.contains("Background tasks"));
    }

    fn git(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(st.success());
    }

    #[test]
    fn tree_check_flags_tracked_changes_only_when_clean_before() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q"]);
        std::fs::write(p.join("a.txt"), "1").unwrap();
        git(p, &["add", "a.txt"]);
        git(p, &["commit", "-q", "-m", "init"]);
        let log = write_log(&[INIT, RESULT]);

        // untracked files are ignored
        std::fs::write(p.join("scratch.txt"), "x").unwrap();
        assert!(!assess(log.path(), p, true).is_unfinished());

        std::fs::write(p.join("a.txt"), "2").unwrap();
        let t = assess(log.path(), p, true);
        assert_eq!(t.uncommitted, vec!["a.txt".to_string()]);
        // dirty before the step: not blamed on the agent
        assert!(!assess(log.path(), p, false).is_unfinished());
    }

    // ── resume loop ────────────────────────────────────────────────────────

    fn handle(session: Option<&str>, cost: f64) -> RunHandle {
        RunHandle {
            id: "r".into(),
            status: runkon_runtimes::RunStatus::Completed,
            subprocess_pid: None,
            runtime: "claude".into(),
            session_id: session.map(String::from),
            result_text: Some(
                "done\n<<<FLOW_OUTPUT>>>\n{\"markers\":[],\"context\":\"c\"}\n<<<END_FLOW_OUTPUT>>>".into(),
            ),
            started_at: String::new(),
            ended_at: None,
            log_file: None,
            model: None,
            cost_usd: Some(cost),
            num_turns: Some(2),
            duration_ms: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }

    fn outcome(cost: f64) -> AgentRunOutcome {
        AgentRunOutcome {
            run: handle(Some("s2"), cost),
            markers: vec!["m".into()],
            context: "ctx".into(),
            structured_output: None,
            correction_attempts: 0,
        }
    }

    fn base_output() -> ActionOutput {
        let mut md = HashMap::new();
        md.insert(k::COST_USD.to_string(), "1.0".to_string());
        md.insert(k::NUM_TURNS.to_string(), "3".to_string());
        md.insert(k::OUTPUT_WARNING.to_string(), "no block".to_string());
        ActionOutput {
            markers: vec![],
            context: None,
            result_text: Some("ending turn".into()),
            structured_output: None,
            metadata: md,
            child_run_id: None,
        }
    }

    fn bg() -> UnfinishedTurn {
        UnfinishedTurn {
            background: vec![BackgroundTask {
                task_id: "a".into(),
                task_type: "local_bash".into(),
                description: "cargo test".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn finished_turn_is_left_alone() {
        let mut out = base_output();
        finish_unfinished_turn(
            "s",
            &mut out,
            None,
            UnfinishedTurn::default,
            || panic!("no lookup"),
            |_, _| panic!("no resume"),
        )
        .unwrap();
        assert!(!out.metadata.contains_key(FINISH_TURN_ATTEMPTS));
    }

    #[test]
    fn first_resume_finishes_and_sums_metrics() {
        let mut out = base_output();
        let mut calls = 0;
        let mut assessments = 0;
        finish_unfinished_turn(
            "s",
            &mut out,
            None,
            || {
                assessments += 1;
                if assessments == 1 {
                    bg()
                } else {
                    UnfinishedTurn::default()
                }
            },
            || Some("s1".into()),
            |prompt, session| {
                calls += 1;
                assert_eq!(session, "s1");
                assert!(prompt.contains("cargo test"));
                Ok(outcome(0.5))
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(out.metadata[FINISH_TURN_ATTEMPTS], "1");
        assert_eq!(out.metadata[k::COST_USD], "1.5");
        assert_eq!(out.metadata[k::NUM_TURNS], "5");
        assert_eq!(out.markers, vec!["m".to_string()]);
        assert!(out.result_text.as_deref().unwrap().starts_with("done"));
        assert!(!out.metadata.contains_key(k::OUTPUT_WARNING));
    }

    #[test]
    fn still_unfinished_fails_after_max_attempts() {
        let mut out = base_output();
        let mut calls = 0;
        let err = finish_unfinished_turn(
            "s",
            &mut out,
            None,
            bg,
            || Some("s1".into()),
            |_, _| {
                calls += 1;
                Ok(outcome(0.1))
            },
        )
        .unwrap_err()
        .to_string();
        assert_eq!(calls, MAX_FINISH_ATTEMPTS);
        assert!(err.contains("1 background task(s)"), "{err}");
        assert!(err.contains("after 2 finish attempt(s)"), "{err}");
    }

    #[test]
    fn no_session_fails_immediately() {
        let mut out = base_output();
        let err = finish_unfinished_turn("s", &mut out, None, bg, || None, |_, _| panic!())
            .unwrap_err()
            .to_string();
        assert!(err.contains("ended its turn before finishing"), "{err}");
    }

    #[test]
    fn resume_error_fails_step() {
        let mut out = base_output();
        let err = finish_unfinished_turn(
            "s",
            &mut out,
            None,
            bg,
            || Some("s1".into()),
            |_, _| Err("boom".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("attempt 1 failed: boom"), "{err}");
    }
}
