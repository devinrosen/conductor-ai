//! Reading a workflow step's agent log: resolve which log belongs to a step, then
//! render it as the final result, a bounded tail, or the full file.

use std::path::PathBuf;

use rusqlite::Connection;

use super::output_excerpt::{flow_output_excerpt, step_has_output_error};
use super::placeholder_scan::PROMPT_WARNING_PREFIX;
use super::WorkflowRunStep;
use crate::agent::{final_result_text, AgentManager};
use crate::error::{ConductorError, Result};

const DEFAULT_TAIL_LINES: usize = 20;
const MAX_TAIL_LINES: usize = 200;
/// Longest a single tail line may be before it is cut.
const MAX_LINE_BYTES: usize = 4 * 1024;
/// Cap on the whole tail output; older lines are dropped first.
const MAX_TAIL_BYTES: usize = 100 * 1024;

/// How much of a step's log to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepLogMode {
    /// Only the agent's final result message, with a short header.
    Result,
    /// The last `lines` log messages, bounded in size.
    Tail { lines: usize },
    /// The whole log file.
    Full,
    /// The prompt the agent was given, as stored on its run, with its length.
    Prompt,
}

impl StepLogMode {
    /// Parse the `mode` and `lines` arguments. `mode` defaults to `result`; `lines`
    /// defaults to 20 and is clamped to 1..=200 (it only matters for `tail`).
    pub fn parse(mode: Option<&str>, lines: Option<usize>) -> Result<Self> {
        let lines = lines.unwrap_or(DEFAULT_TAIL_LINES).clamp(1, MAX_TAIL_LINES);
        match mode.unwrap_or("result") {
            "result" => Ok(Self::Result),
            "tail" => Ok(Self::Tail { lines }),
            "full" => Ok(Self::Full),
            "prompt" => Ok(Self::Prompt),
            other => Err(ConductorError::Workflow(format!(
                "Invalid mode '{other}': expected one of 'result', 'tail', 'full', 'prompt'"
            ))),
        }
    }
}

/// Where a step's log comes from.
#[derive(Debug, Clone)]
pub enum StepLogSource {
    /// An agent step: the log of its child agent run.
    Agent {
        child_run_id: String,
        log_path: PathBuf,
        /// The agent run's stored result text, used when the log has no result event.
        agent_result_text: Option<String>,
        /// The prompt the agent run was started with: its first `prompt` event. `None`
        /// for runs that stored none (older runs, other runtimes, a failed spawn).
        prompt: Option<String>,
        /// Run-time warnings about the prompt (`Prompt warning: …` system events).
        prompt_warnings: Vec<String>,
    },
    /// A script step: the capped stderr log runkon wrote for the attempt, recorded in
    /// the step row's `output_file`. `None` when the attempt left no log.
    Script { log_path: Option<PathBuf> },
}

/// The step a step-log request resolved to, and where its log lives.
#[derive(Debug, Clone)]
pub struct StepLogTarget {
    pub run_id: String,
    pub step: WorkflowRunStep,
    pub source: StepLogSource,
    /// The step failed on its FLOW_OUTPUT, so rendering adds an excerpt of the block.
    /// The excerpt is built at render time: it needs the log, and resolving only
    /// touches the database.
    pub output_error: bool,
}

/// Find the step named `step_name` in `run_id` (last iteration wins; among script
/// retry attempts of one iteration, the latest attempt wins) and its log. `mode` is
/// the mode the target will be rendered in: only `Prompt` loads the stored prompt.
pub fn resolve_step_log_target(
    conn: &Connection,
    run_id: &str,
    step_name: &str,
    mode: StepLogMode,
) -> Result<StepLogTarget> {
    super::get_workflow_run(conn, run_id)?
        .ok_or_else(|| ConductorError::WorkflowRunNotFound { id: run_id.into() })?;

    let step = super::get_workflow_steps(conn, run_id)?
        .into_iter()
        .filter(|s| s.step_name == step_name)
        .max_by(|a, b| {
            (a.iteration, a.retry_count, &a.started_at, &a.id).cmp(&(
                b.iteration,
                b.retry_count,
                &b.started_at,
                &b.id,
            ))
        })
        .ok_or_else(|| {
            ConductorError::Workflow(format!("Step '{step_name}' not found in run {run_id}"))
        })?;

    if step.role == runkon_flow::constants::STEP_ROLE_SCRIPT {
        let output_error = step_has_output_error(&step);
        let log_path = step.output_file.as_deref().map(PathBuf::from);
        return Ok(StepLogTarget {
            run_id: run_id.to_string(),
            step,
            source: StepLogSource::Script { log_path },
            output_error,
        });
    }

    // Gate and skipped steps have no child_run_id.
    let child_run_id = step.child_run_id.clone().ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Step '{step_name}' has no agent run (gate steps and skipped steps do not produce logs)"
        ))
    })?;

    // `log_path` respects the run's log_file override.
    let run = AgentManager::new(conn)
        .get_run(&child_run_id)?
        .ok_or_else(|| ConductorError::Agent(format!("agent run not found: {child_run_id}")))?;
    let log_path = run.log_path()?;
    let output_error = step_has_output_error(&step);
    // Only `Prompt` mode shows the prompt or its warnings, so only it reads the events.
    let (prompt, prompt_warnings) = if mode == StepLogMode::Prompt {
        let events = AgentManager::new(conn)
            .list_events_for_run(&child_run_id)
            .map_err(|e| {
                ConductorError::Agent(format!(
                    "failed to read events for agent run {child_run_id}: {e}"
                ))
            })?;
        let prompt = events
            .iter()
            .find(|e| e.kind == crate::agent::types::EVENT_KIND_PROMPT)
            .map(|e| e.summary.clone());
        let warnings = events
            .iter()
            .filter(|e| {
                e.kind == crate::agent::types::EVENT_KIND_SYSTEM
                    && e.summary.starts_with(PROMPT_WARNING_PREFIX)
            })
            .map(|e| e.summary.clone())
            .collect();
        (prompt, warnings)
    } else {
        (None, Vec::new())
    };

    Ok(StepLogTarget {
        run_id: run_id.to_string(),
        step,
        source: StepLogSource::Agent {
            child_run_id,
            log_path,
            agent_result_text: run.result_text,
            prompt,
            prompt_warnings,
        },
        output_error,
    })
}

/// Render the target's log in the requested mode. Does file I/O only.
pub fn render_step_log(target: &StepLogTarget, mode: StepLogMode) -> Result<String> {
    if let StepLogSource::Script { log_path } = &target.source {
        return render_script_log(target, log_path.as_deref(), mode);
    }
    match mode {
        StepLogMode::Full => read_log(target),
        StepLogMode::Prompt => Ok(render_prompt(target)),
        StepLogMode::Tail { lines } => {
            let excerpt = output_excerpt(target, || final_text(target));
            Ok(format!(
                "{}{}",
                header(target, excerpt.as_deref()),
                render_tail(&read_log(target)?, lines)
            ))
        }
        StepLogMode::Result => {
            let result = final_text(target);
            let excerpt = output_excerpt(target, || result.clone());
            let header = header(target, excerpt.as_deref());
            match result {
                Some(text) => Ok(format!("{header}--- result ---\n{text}\n")),
                None => {
                    let tail = render_tail(&read_log(target)?, DEFAULT_TAIL_LINES);
                    Ok(format!(
                        "{header}No result message yet (step still running or killed); showing the log tail.\n{tail}"
                    ))
                }
            }
        }
    }
}

/// Render the stored prompt with its length. This is the prompt conductor rendered
/// for the agent, before the session's startup context is prepended.
fn render_prompt(target: &StepLogTarget) -> String {
    let head = header(target, None);
    let StepLogSource::Agent {
        prompt,
        prompt_warnings,
        ..
    } = &target.source
    else {
        return head;
    };
    let mut out = head;
    for w in prompt_warnings {
        out.push_str(&format!("{w}\n"));
    }
    match prompt {
        Some(text) => out.push_str(&format!(
            "--- prompt ({} chars, {} lines; as rendered, before the startup context) ---\n{text}\n",
            text.chars().count(),
            text.lines().count()
        )),
        None => out.push_str("No stored prompt for this run.\n"),
    }
    out
}

/// The parsed FLOW_OUTPUT excerpt to show in the header; only for steps whose output failed to parse.
/// `result` is only evaluated when the gate is open, so healthy steps never read the log for it.
fn output_excerpt(
    target: &StepLogTarget,
    result: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if !target.output_error {
        return None;
    }
    result().and_then(|t| flow_output_excerpt(&t))
}

/// The step's final result message: the log's last result event, else the stored text.
fn final_text(target: &StepLogTarget) -> Option<String> {
    match &target.source {
        StepLogSource::Agent {
            log_path,
            agent_result_text,
            ..
        } => final_result_text(log_path, agent_result_text.as_deref()),
        StepLogSource::Script { .. } => None,
    }
}

fn read_log(target: &StepLogTarget) -> Result<String> {
    let StepLogSource::Agent {
        child_run_id,
        log_path,
        ..
    } = &target.source
    else {
        return Err(ConductorError::Workflow(format!(
            "Step '{}' is not an agent step",
            target.step.step_name
        )));
    };
    std::fs::read_to_string(log_path).map_err(|e| {
        ConductorError::Workflow(format!(
            "Log file not found for step '{}' of run {} (agent run {}) at '{}': {e}",
            target.step.step_name,
            target.run_id,
            child_run_id,
            log_path.display()
        ))
    })
}

/// Render a script step's log. `log_path` is the step row's `output_file`.
fn render_script_log(
    target: &StepLogTarget,
    log_path: Option<&std::path::Path>,
    mode: StepLogMode,
) -> Result<String> {
    let step = &target.step;
    if matches!(mode, StepLogMode::Prompt) {
        return Err(ConductorError::Workflow(format!(
            "Step '{}' is a script step and has no prompt",
            step.step_name
        )));
    }
    let mut head = format!(
        "step: {} (iteration {}, attempt {})\nstatus: {}\n",
        step.step_name,
        step.iteration,
        step.retry_count + 1,
        step.status
    );
    if let Some(err) = step.step_error.as_deref() {
        head.push_str(&format!("step error: {err}\n"));
    }
    let flow_output = flow_output_section(step);
    let Some(path) = log_path else {
        let mut out = format!(
            "{head}Script step '{}' left no log: it wrote nothing to stderr or outside FLOW_OUTPUT, or ran before runkon 0.10.\n",
            step.step_name
        );
        if let Some(section) = flow_output {
            out.push_str(&section);
        }
        return Ok(out);
    };
    let contents = std::fs::read_to_string(path).map_err(|e| {
        ConductorError::Workflow(format!(
            "Log file not found for script step '{}' of run {} at '{}': {e}",
            step.step_name,
            target.run_id,
            path.display()
        ))
    })?;
    let tail_section = flow_output.unwrap_or_default();
    match mode {
        // "Full" is the raw log file; callers may pipe it.
        StepLogMode::Full => Ok(contents),
        StepLogMode::Tail { lines } => Ok(format!(
            "{head}{}{tail_section}",
            render_tail_of(&contents, lines, "lines")
        )),
        StepLogMode::Prompt => unreachable!("rejected above"),
        // runkon caps the log at about 64 KiB, so the whole of it is the result.
        StepLogMode::Result => Ok(format!("{head}--- log ---\n{contents}{tail_section}")),
    }
}

/// The FLOW_OUTPUT the engine stored on a script step's row (`context_out`, `markers_out`),
/// rendered as a section. `None` when the step emitted neither context nor markers.
fn flow_output_section(step: &WorkflowRunStep) -> Option<String> {
    let context = step.context_out.as_deref().filter(|c| !c.is_empty());
    let raw_markers = step.markers_out.as_deref().map(str::trim);
    let markers: Option<String> = match raw_markers {
        None | Some("") => None,
        Some(raw) => match serde_json::from_str::<Vec<String>>(raw) {
            Ok(list) if list.is_empty() => None,
            Ok(list) => Some(list.join(", ")),
            Err(_) => Some(raw.to_string()),
        },
    };
    if context.is_none() && markers.is_none() {
        return None;
    }
    let mut out = format!(
        "--- FLOW_OUTPUT ---\nmarkers: {}\n",
        markers.as_deref().unwrap_or("(none)")
    );
    if let Some(c) = context {
        out.push_str("context:\n");
        out.push_str(c);
        if !c.ends_with('\n') {
            out.push('\n');
        }
    }
    Some(out)
}

fn header(target: &StepLogTarget, excerpt: Option<&str>) -> String {
    let step = &target.step;
    let child_run_id = match &target.source {
        StepLogSource::Agent { child_run_id, .. } => child_run_id.as_str(),
        StepLogSource::Script { .. } => "",
    };
    let mut out = format!(
        "step: {} (iteration {})\nstatus: {}\nagent run: {}\n",
        step.step_name, step.iteration, step.status, child_run_id
    );
    if let Some(err) = step.step_error.as_deref() {
        out.push_str(&format!("step error: {err}\n"));
    }
    if let Some(excerpt) = excerpt {
        out.push_str(excerpt);
        out.push('\n');
    }
    out
}

/// Last `n` non-empty lines, each cut to ~4 KB, the whole capped at ~100 KB by
/// dropping the oldest lines.
fn render_tail(contents: &str, n: usize) -> String {
    render_tail_of(contents, n, "messages")
}

/// [`render_tail`] with the noun for the line count ("messages" for agent logs, "lines" for text logs).
fn render_tail_of(contents: &str, n: usize, unit: &str) -> String {
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = lines.len();
    let mut kept: Vec<String> = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().rev().take(n) {
        let line = truncate_line(line);
        if bytes + line.len() + 1 > MAX_TAIL_BYTES && !kept.is_empty() {
            break;
        }
        bytes += line.len() + 1;
        kept.push(line);
    }
    kept.reverse();
    format!(
        "--- last {} of {total} {unit} ---\n{}\n",
        kept.len(),
        kept.join("\n")
    )
}

fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_string();
    }
    let mut end = MAX_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated {} bytes]", &line[..end], line.len() - end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(log: &str, db_result: Option<&str>) -> (StepLogTarget, PathBuf) {
        let dir = crate::test_helpers::ensure_agent_log_dir();
        let nonce = ulid::Ulid::new();
        let path = dir.join(format!("test-step-log-{nonce}.log"));
        std::fs::write(&path, log).unwrap();
        let t = StepLogTarget {
            run_id: "r1".into(),
            step: sample_step(),
            source: StepLogSource::Agent {
                child_run_id: "c1".into(),
                log_path: path.clone(),
                agent_result_text: db_result.map(String::from),
                prompt: None,
                prompt_warnings: Vec::new(),
            },
            output_error: false,
        };
        (t, path)
    }

    fn sample_step() -> WorkflowRunStep {
        WorkflowRunStep {
            id: "s1".into(),
            workflow_run_id: "r1".into(),
            step_name: "plan".into(),
            child_run_id: Some("c1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn mode_parse_defaults_and_clamps() {
        assert_eq!(StepLogMode::parse(None, None).unwrap(), StepLogMode::Result);
        assert_eq!(
            StepLogMode::parse(Some("tail"), None).unwrap(),
            StepLogMode::Tail { lines: 20 }
        );
        assert_eq!(
            StepLogMode::parse(Some("tail"), Some(0)).unwrap(),
            StepLogMode::Tail { lines: 1 }
        );
        assert_eq!(
            StepLogMode::parse(Some("tail"), Some(9999)).unwrap(),
            StepLogMode::Tail { lines: 200 }
        );
        assert_eq!(
            StepLogMode::parse(Some("full"), None).unwrap(),
            StepLogMode::Full
        );
        let err = StepLogMode::parse(Some("bogus"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("result") && err.contains("tail") && err.contains("full"));
        assert!(err.contains("prompt"));
        assert_eq!(
            StepLogMode::parse(Some("prompt"), None).unwrap(),
            StepLogMode::Prompt
        );
    }

    #[test]
    fn prompt_mode_shows_stored_prompt_with_length() {
        let (mut t, path) = target("{}\n", None);
        if let StepLogSource::Agent {
            prompt,
            prompt_warnings,
            ..
        } = &mut t.source
        {
            *prompt = Some("héllo\nworld".into());
            prompt_warnings.push(format!("{PROMPT_WARNING_PREFIX} agent a: lines 1-2"));
        }
        let out = render_step_log(&t, StepLogMode::Prompt).unwrap();
        assert!(out.contains("prompt (11 chars, 2 lines"), "{out}");
        assert!(out.contains("héllo\nworld"), "{out}");
        assert!(out.contains("Prompt warning: agent a"), "{out}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn prompt_mode_without_stored_prompt() {
        let (t, path) = target("{}\n", None);
        let out = render_step_log(&t, StepLogMode::Prompt).unwrap();
        assert!(out.contains("No stored prompt"), "{out}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn prompt_mode_on_script_step_errors() {
        let t = StepLogTarget {
            run_id: "r1".into(),
            step: sample_step(),
            source: StepLogSource::Script { log_path: None },
            output_error: false,
        };
        let err = render_step_log(&t, StepLogMode::Prompt)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no prompt"), "{err}");
    }

    #[test]
    fn result_mode_uses_last_result_event() {
        let log = "{\"type\":\"assistant\"}\n{\"type\":\"result\",\"result\":\"first\"}\n{\"type\":\"result\",\"result\":\"final answer\"}\n";
        let (t, path) = target(log, Some("db text"));
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(out.contains("final answer"), "{out}");
        assert!(!out.contains("first"), "{out}");
        assert!(out.contains("agent run: c1"), "{out}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn result_mode_falls_back_to_db_text() {
        let (t, path) = target("{\"type\":\"assistant\"}\n", Some("db text"));
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(out.contains("db text"), "{out}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn result_mode_falls_back_to_tail() {
        let (t, path) = target("{\"type\":\"assistant\",\"n\":1}\n", None);
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(out.contains("No result message yet"), "{out}");
        assert!(out.contains("\"n\":1"), "{out}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let log: String = (0..10).map(|i| format!("{{\"i\":{i}}}\n")).collect();
        let (t, path) = target(&log, None);
        let out = render_step_log(&t, StepLogMode::Tail { lines: 3 }).unwrap();
        assert!(out.contains("last 3 of 10 messages"), "{out}");
        assert!(
            out.contains("{\"i\":9}") && out.contains("{\"i\":7}"),
            "{out}"
        );
        assert!(!out.contains("{\"i\":6}"), "{out}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn tail_truncates_long_lines_and_caps_total() {
        let long = "é".repeat(5000);
        assert!(truncate_line(&long).contains("[truncated"));
        let big = format!("{}\n", "x".repeat(MAX_LINE_BYTES));
        let log = big.repeat(60);
        let out = render_tail(&log, 60);
        assert!(out.len() < MAX_TAIL_BYTES + 1024, "len {}", out.len());
        assert!(out.contains("of 60 messages"));
    }

    #[test]
    fn full_mode_reads_whole_file_and_reports_missing() {
        let (mut t, path) = target("line1\nline2\n", None);
        assert_eq!(
            render_step_log(&t, StepLogMode::Full).unwrap(),
            "line1\nline2\n"
        );
        std::fs::remove_file(&path).ok();
        t.source = StepLogSource::Agent {
            child_run_id: "c1".into(),
            log_path: path,
            agent_result_text: None,
            prompt: None,
            prompt_warnings: Vec::new(),
        };
        let err = render_step_log(&t, StepLogMode::Full)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Log file not found") && err.contains("plan"),
            "{err}"
        );
    }

    // --- resolve_step_log_target ---

    use crate::test_helpers::{seed_step_with_agent_run, setup_db_with_agent_run};
    use crate::workflow::{create_workflow_run, insert_step, WorkflowStepStatus};

    fn db_with_run() -> (Connection, String) {
        let conn = setup_db_with_agent_run();
        let run = create_workflow_run(&conn, "wf", None, "ar1", false, "manual", None).unwrap();
        (conn, run.id.clone())
    }

    #[test]
    fn resolve_prompt_mode_reads_prompt_and_only_warnings() {
        let (conn, run_id) = db_with_run();
        let seeded = seed_step_with_agent_run(
            &conn,
            &run_id,
            "plan",
            0,
            WorkflowStepStatus::Completed,
            None,
            None,
            "",
        );
        let mgr = AgentManager::new(&conn);
        let warning = format!("{PROMPT_WARNING_PREFIX} agent a: lines 1-2");
        for (kind, summary) in [
            (crate::agent::types::EVENT_KIND_SYSTEM, warning.as_str()),
            (crate::agent::types::EVENT_KIND_PROMPT, "first prompt"),
            (crate::agent::types::EVENT_KIND_SYSTEM, "unrelated"),
            (crate::agent::types::EVENT_KIND_PROMPT, "second prompt"),
        ] {
            mgr.create_event(
                &seeded.child_run_id,
                kind,
                summary,
                "2026-01-01T00:00:00Z",
                None,
            )
            .unwrap();
        }
        let t = resolve_step_log_target(&conn, &run_id, "plan", StepLogMode::Prompt).unwrap();
        let StepLogSource::Agent {
            prompt,
            prompt_warnings,
            ..
        } = &t.source
        else {
            panic!("expected an agent source");
        };
        assert_eq!(prompt.as_deref(), Some("first prompt"));
        assert_eq!(prompt_warnings, &vec![warning]);

        // Other modes skip the event read.
        let t = resolve_step_log_target(&conn, &run_id, "plan", StepLogMode::Result).unwrap();
        let StepLogSource::Agent {
            prompt,
            prompt_warnings,
            ..
        } = &t.source
        else {
            panic!("expected an agent source");
        };
        assert!(prompt.is_none() && prompt_warnings.is_empty());
        std::fs::remove_file(seeded.log_path).ok();
    }

    #[test]
    fn resolve_prompt_mode_without_events_is_empty() {
        let (conn, run_id) = db_with_run();
        let seeded = seed_step_with_agent_run(
            &conn,
            &run_id,
            "plan",
            0,
            WorkflowStepStatus::Completed,
            None,
            None,
            "",
        );
        let t = resolve_step_log_target(&conn, &run_id, "plan", StepLogMode::Prompt).unwrap();
        let StepLogSource::Agent {
            prompt,
            prompt_warnings,
            ..
        } = &t.source
        else {
            panic!("expected an agent source");
        };
        assert!(prompt.is_none() && prompt_warnings.is_empty());
        std::fs::remove_file(seeded.log_path).ok();
    }

    #[test]
    fn resolve_unknown_run_errors() {
        let (conn, _) = db_with_run();
        let err = resolve_step_log_target(&conn, "nope", "plan", StepLogMode::Result).unwrap_err();
        assert!(
            matches!(err, ConductorError::WorkflowRunNotFound { .. }),
            "{err}"
        );
    }

    #[test]
    fn resolve_unknown_step_errors() {
        let (conn, run_id) = db_with_run();
        let err = resolve_step_log_target(&conn, &run_id, "missing", StepLogMode::Result)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Step 'missing' not found"), "{err}");
    }

    #[test]
    fn resolve_step_without_agent_run_errors() {
        let (conn, run_id) = db_with_run();
        insert_step(&conn, &run_id, "gate", "actor", false, 0, 0).unwrap();
        let err = resolve_step_log_target(&conn, &run_id, "gate", StepLogMode::Result)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no agent run"), "{err}");
    }

    #[test]
    fn resolve_picks_highest_iteration() {
        let (conn, run_id) = db_with_run();
        let done = WorkflowStepStatus::Completed;
        // Seed the later iteration first so row order can't explain the result.
        let second =
            seed_step_with_agent_run(&conn, &run_id, "plan", 1, done.clone(), None, None, "");
        let first = seed_step_with_agent_run(&conn, &run_id, "plan", 0, done, None, None, "");
        let t = resolve_step_log_target(&conn, &run_id, "plan", StepLogMode::Result).unwrap();
        assert_eq!(t.step.iteration, 1);
        let StepLogSource::Agent {
            child_run_id,
            log_path,
            ..
        } = &t.source
        else {
            panic!("expected an agent source");
        };
        assert_eq!(child_run_id, &second.child_run_id);
        assert_eq!(log_path, &second.log_path);
        assert!(!t.output_error);
        std::fs::remove_file(first.log_path).ok();
        std::fs::remove_file(second.log_path).ok();
    }

    #[test]
    fn resolve_carries_stored_result_and_flags_output_error() {
        let (conn, run_id) = db_with_run();
        let seeded = seed_step_with_agent_run(
            &conn,
            &run_id,
            "plan",
            0,
            WorkflowStepStatus::Failed,
            Some("structured output validation: Invalid JSON in FLOW_OUTPUT: oops"),
            Some("<<<FLOW_OUTPUT>>>\n{bad\n<<<END_FLOW_OUTPUT>>>"),
            "{\"type\":\"assistant\"}\n",
        );
        let t = resolve_step_log_target(&conn, &run_id, "plan", StepLogMode::Result).unwrap();
        assert!(t.output_error);
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(
            out.contains("parsed FLOW_OUTPUT block (line 1 of result): {bad"),
            "{out}"
        );
        let out = render_step_log(&t, StepLogMode::Tail { lines: 5 }).unwrap();
        assert!(out.contains("parsed FLOW_OUTPUT block"), "{out}");
        std::fs::remove_file(seeded.log_path).ok();
    }

    // --- script steps ---

    fn seed_script_step(
        conn: &Connection,
        run_id: &str,
        name: &str,
        retry_count: i64,
        output_file: Option<&std::path::Path>,
    ) {
        let id = crate::workflow::insert_step_running(
            conn,
            run_id,
            name,
            "script",
            false,
            0,
            0,
            retry_count,
        )
        .unwrap();
        conn.execute(
            "UPDATE workflow_run_steps SET output_file = ?1, status = 'failed' WHERE id = ?2",
            rusqlite::params![output_file.map(|p| p.to_string_lossy().into_owned()), id],
        )
        .unwrap();
    }

    fn script_log_file(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("test-script-log-{}.log", ulid::Ulid::new()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn script_step_renders_log_in_every_mode() {
        let (conn, run_id) = db_with_run();
        let log = script_log_file("--- stderr ---\nwarn one\nboom\n");
        seed_script_step(&conn, &run_id, "build", 0, Some(&log));
        let t = resolve_step_log_target(&conn, &run_id, "build", StepLogMode::Result).unwrap();
        assert!(matches!(t.source, StepLogSource::Script { .. }));

        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(out.contains("warn one") && out.contains("boom"), "{out}");
        assert!(out.contains("step: build"), "{out}");

        let out = render_step_log(&t, StepLogMode::Tail { lines: 1 }).unwrap();
        assert!(out.contains("boom") && !out.contains("warn one"), "{out}");

        let out = render_step_log(&t, StepLogMode::Full).unwrap();
        assert_eq!(out, "--- stderr ---\nwarn one\nboom\n");
        std::fs::remove_file(log).ok();
    }

    #[test]
    fn script_step_without_output_file_says_no_log() {
        let (conn, run_id) = db_with_run();
        seed_script_step(&conn, &run_id, "quiet", 0, None);
        let t = resolve_step_log_target(&conn, &run_id, "quiet", StepLogMode::Result).unwrap();
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(out.contains("left no log"), "{out}");
    }

    fn set_flow_output(conn: &Connection, run_id: &str, ctx: Option<&str>, markers: Option<&str>) {
        conn.execute(
            "UPDATE workflow_run_steps SET context_out = ?1, markers_out = ?2 \
             WHERE workflow_run_id = ?3",
            rusqlite::params![ctx, markers, run_id],
        )
        .unwrap();
    }

    #[test]
    fn script_step_no_log_shows_flow_output() {
        let (conn, run_id) = db_with_run();
        seed_script_step(&conn, &run_id, "decide", 0, None);
        set_flow_output(
            &conn,
            &run_id,
            Some("Decisions:\n1. Q\n   Default: D"),
            Some(r#"["needs_decision","x"]"#),
        );
        let t = resolve_step_log_target(&conn, &run_id, "decide", StepLogMode::Result).unwrap();
        for mode in [
            StepLogMode::Result,
            StepLogMode::Tail { lines: 5 },
            StepLogMode::Full,
        ] {
            let out = render_step_log(&t, mode).unwrap();
            assert!(out.contains("left no log"), "{out}");
            assert!(out.contains("--- FLOW_OUTPUT ---"), "{out}");
            assert!(out.contains("markers: needs_decision, x"), "{out}");
            assert!(out.contains("1. Q\n   Default: D"), "{out}");
        }
    }

    #[test]
    fn script_step_no_log_no_flow_output_has_no_section() {
        let (conn, run_id) = db_with_run();
        seed_script_step(&conn, &run_id, "quiet", 0, None);
        set_flow_output(&conn, &run_id, Some(""), Some("[]"));
        let t = resolve_step_log_target(&conn, &run_id, "quiet", StepLogMode::Result).unwrap();
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(!out.contains("--- FLOW_OUTPUT ---"), "{out}");
    }

    #[test]
    fn script_step_log_then_flow_output_but_full_stays_raw() {
        let (conn, run_id) = db_with_run();
        let log = script_log_file("stderr line\n");
        seed_script_step(&conn, &run_id, "build", 0, Some(&log));
        set_flow_output(&conn, &run_id, Some("the context"), Some("[]"));
        let t = resolve_step_log_target(&conn, &run_id, "build", StepLogMode::Result).unwrap();

        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        let log_at = out.find("stderr line").unwrap();
        let section_at = out.find("--- FLOW_OUTPUT ---").unwrap();
        assert!(log_at < section_at, "{out}");
        assert!(out.contains("markers: (none)"), "{out}");
        assert!(out.contains("the context"), "{out}");

        let out = render_step_log(&t, StepLogMode::Tail { lines: 5 }).unwrap();
        assert!(out.contains("the context"), "{out}");

        let out = render_step_log(&t, StepLogMode::Full).unwrap();
        assert_eq!(out, "stderr line\n");
        std::fs::remove_file(log).ok();
    }

    #[test]
    fn script_step_malformed_markers_shown_raw() {
        let (conn, run_id) = db_with_run();
        seed_script_step(&conn, &run_id, "odd", 0, None);
        set_flow_output(&conn, &run_id, None, Some("not-json"));
        let t = resolve_step_log_target(&conn, &run_id, "odd", StepLogMode::Result).unwrap();
        let out = render_step_log(&t, StepLogMode::Result).unwrap();
        assert!(out.contains("markers: not-json"), "{out}");
    }

    #[test]
    fn script_step_with_missing_file_reports_path() {
        let (conn, run_id) = db_with_run();
        let gone = std::env::temp_dir().join("test-script-log-gone-xyz.log");
        seed_script_step(&conn, &run_id, "build", 0, Some(&gone));
        let t = resolve_step_log_target(&conn, &run_id, "build", StepLogMode::Result).unwrap();
        let err = render_step_log(&t, StepLogMode::Result)
            .unwrap_err()
            .to_string();
        assert!(err.contains("test-script-log-gone-xyz.log"), "{err}");
    }

    #[test]
    fn script_step_latest_attempt_wins() {
        let (conn, run_id) = db_with_run();
        let first = script_log_file("first attempt\n");
        let second = script_log_file("second attempt\n");
        // Seed the later attempt first so row order can't explain the result.
        seed_script_step(&conn, &run_id, "build", 1, Some(&second));
        seed_script_step(&conn, &run_id, "build", 0, Some(&first));
        let t = resolve_step_log_target(&conn, &run_id, "build", StepLogMode::Result).unwrap();
        let out = render_step_log(&t, StepLogMode::Full).unwrap();
        assert!(out.contains("second attempt"), "{out}");
        std::fs::remove_file(first).ok();
        std::fs::remove_file(second).ok();
    }
}
