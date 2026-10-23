//! Reading a workflow step's agent log: resolve which log belongs to a step, then
//! render it as the final result, a bounded tail, or the full file.

use std::path::PathBuf;

use rusqlite::Connection;

use super::output_excerpt::step_output_error_excerpt;
use super::WorkflowRunStep;
use crate::agent::{read_log_result, AgentManager};
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
}

impl StepLogMode {
    /// Parse the `mode` and `lines` arguments. `mode` defaults to `result`; `lines`
    /// defaults to 20 and is clamped to 1..=200 (it only matters for `tail`).
    pub fn parse(mode: Option<&str>, lines: Option<usize>) -> Result<Self> {
        let lines = lines
            .unwrap_or(DEFAULT_TAIL_LINES)
            .clamp(1, MAX_TAIL_LINES);
        match mode.unwrap_or("result") {
            "result" => Ok(Self::Result),
            "tail" => Ok(Self::Tail { lines }),
            "full" => Ok(Self::Full),
            other => Err(ConductorError::Workflow(format!(
                "Invalid mode '{other}': expected one of 'result', 'tail', 'full'"
            ))),
        }
    }
}

/// The step and agent run a step-log request resolved to.
#[derive(Debug, Clone)]
pub struct StepLogTarget {
    pub run_id: String,
    pub step: WorkflowRunStep,
    pub child_run_id: String,
    pub log_path: PathBuf,
    /// The agent run's stored result text, used when the log has no result event.
    pub agent_result_text: Option<String>,
    /// Excerpt of the parsed FLOW_OUTPUT block, when the step failed on its output.
    pub output_excerpt: Option<String>,
}

/// Find the step named `step_name` in `run_id` (last iteration wins) and the log of
/// its agent run.
pub fn resolve_step_log_target(
    conn: &Connection,
    run_id: &str,
    step_name: &str,
) -> Result<StepLogTarget> {
    super::get_workflow_run(conn, run_id)?
        .ok_or_else(|| ConductorError::WorkflowRunNotFound { id: run_id.into() })?;

    let step = super::get_workflow_steps(conn, run_id)?
        .into_iter()
        .filter(|s| s.step_name == step_name)
        .max_by_key(|s| s.iteration)
        .ok_or_else(|| {
            ConductorError::Workflow(format!("Step '{step_name}' not found in run {run_id}"))
        })?;

    // Gate and skipped steps have no child_run_id.
    let child_run_id = step.child_run_id.clone().ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Step '{step_name}' has no agent run (gate steps and skipped steps do not produce logs)"
        ))
    })?;

    // Resolves the path from the run, respecting its log_file override.
    let mgr = AgentManager::new(conn);
    let log_path = mgr.log_path_for_run(&child_run_id)?;
    let agent_result_text = mgr
        .get_run(&child_run_id)
        .ok()
        .flatten()
        .and_then(|r| r.result_text);
    let output_excerpt = step_output_error_excerpt(conn, &step);

    Ok(StepLogTarget {
        run_id: run_id.to_string(),
        step,
        child_run_id,
        log_path,
        agent_result_text,
        output_excerpt,
    })
}

/// Render the target's log in the requested mode. Does file I/O only.
pub fn render_step_log(target: &StepLogTarget, mode: StepLogMode) -> Result<String> {
    match mode {
        StepLogMode::Full => read_log(target),
        StepLogMode::Tail { lines } => Ok(format!(
            "{}{}",
            header(target),
            render_tail(&read_log(target)?, lines)
        )),
        StepLogMode::Result => {
            // The last result event is the final attempt: output-correction resumes
            // append to the same log. Prefer it over the stored text, which some
            // failure paths overwrite with an error message.
            let result = read_log_result(&target.log_path)
                .and_then(|r| r.result_text)
                .or_else(|| target.agent_result_text.clone());
            match result {
                Some(text) => Ok(format!("{}--- result ---\n{text}\n", header(target))),
                None => {
                    let tail = render_tail(&read_log(target)?, DEFAULT_TAIL_LINES);
                    Ok(format!(
                        "{}No result message yet (step still running or killed); showing the log tail.\n{tail}",
                        header(target)
                    ))
                }
            }
        }
    }
}

fn read_log(target: &StepLogTarget) -> Result<String> {
    std::fs::read_to_string(&target.log_path).map_err(|e| {
        ConductorError::Workflow(format!(
            "Log file not found for step '{}' of run {} (agent run {}) at '{}': {e}",
            target.step.step_name,
            target.run_id,
            target.child_run_id,
            target.log_path.display()
        ))
    })
}

fn header(target: &StepLogTarget) -> String {
    let step = &target.step;
    let mut out = format!(
        "step: {} (iteration {})\nstatus: {}\nagent run: {}\n",
        step.step_name, step.iteration, step.status, target.child_run_id
    );
    if let Some(err) = step.step_error.as_deref() {
        out.push_str(&format!("step error: {err}\n"));
    }
    if let Some(excerpt) = target.output_excerpt.as_deref() {
        out.push_str(excerpt);
        out.push('\n');
    }
    out
}

/// Last `n` non-empty lines, each cut to ~4 KB, the whole capped at ~100 KB by
/// dropping the oldest lines.
fn render_tail(contents: &str, n: usize) -> String {
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
        "--- last {} of {total} messages ---\n{}\n",
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
            child_run_id: "c1".into(),
            log_path: path.clone(),
            agent_result_text: db_result.map(String::from),
            output_excerpt: None,
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
        let err = StepLogMode::parse(Some("bogus"), None).unwrap_err().to_string();
        assert!(err.contains("result") && err.contains("tail") && err.contains("full"));
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
        assert!(out.contains("{\"i\":9}") && out.contains("{\"i\":7}"), "{out}");
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
        assert_eq!(render_step_log(&t, StepLogMode::Full).unwrap(), "line1\nline2\n");
        std::fs::remove_file(&path).ok();
        t.log_path = path;
        let err = render_step_log(&t, StepLogMode::Full).unwrap_err().to_string();
        assert!(err.contains("Log file not found") && err.contains("plan"), "{err}");
    }
}
