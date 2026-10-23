//! Excerpts of the FLOW_OUTPUT block an agent produced, shown next to output
//! parse and validation errors so the reader can see what the parser was given.

use rusqlite::Connection;

use super::{WorkflowRunStep, WorkflowStepStatus};
use crate::agent::{final_result_text, AgentManager};

/// How many characters of the extracted block an excerpt shows.
const EXCERPT_CHARS: usize = 200;

/// Describe the FLOW_OUTPUT block runkon's parser extracts from `result_text`: the
/// 1-based line where the block starts and its first 200 characters. Returns `None`
/// when the text has no complete block.
///
/// Uses the same lookup as the parser, so the excerpt is the text that was parsed.
pub fn flow_output_excerpt(result_text: &str) -> Option<String> {
    let block = runkon_flow::helpers::find_flow_output_block(result_text)?;
    // `block` is a subslice of `result_text`, so its offset is a pointer difference.
    let offset = (block.as_ptr() as usize).checked_sub(result_text.as_ptr() as usize)?;
    // Report the line of the opening marker, which precedes the (trimmed) block.
    let before = result_text.get(..offset)?;
    let marker = before.rfind("<<<FLOW_OUTPUT>>>").unwrap_or(offset);
    let line = before[..marker].matches('\n').count() + 1;

    let mut shown: String = block.chars().take(EXCERPT_CHARS).collect();
    let cut = block.chars().nth(EXCERPT_CHARS).is_some();
    shown = shown.replace('\n', "⏎");
    if cut {
        shown.push('…');
    }
    Some(format!(
        "parsed FLOW_OUTPUT block (line {line} of result): {shown}"
    ))
}

/// Fragments of runkon's output parse and validation errors. runkon reports these as
/// plain strings (`structured output validation: ...`, `Invalid JSON in FLOW_OUTPUT:
/// ...`), with no typed variant to match on, so these must track its wording. The
/// `predicate_matches_runkon_errors` test pins them.
const OUTPUT_ERROR_FRAGMENTS: [&str; 2] = ["structured output validation", "FLOW_OUTPUT"];

/// True when `err` reads like a FLOW_OUTPUT parse or validation failure.
pub fn is_output_parse_error(err: &str) -> bool {
    OUTPUT_ERROR_FRAGMENTS.iter().any(|f| err.contains(f))
}

/// True when `step` failed on its FLOW_OUTPUT: its error (or, for a failed step with
/// no error, its result text) is an output parse or validation error.
pub fn step_has_output_error(step: &WorkflowRunStep) -> bool {
    step.step_error
        .as_deref()
        .or_else(|| {
            step.result_text
                .as_deref()
                .filter(|_| step.status == WorkflowStepStatus::Failed)
        })
        .is_some_and(is_output_parse_error)
}

/// For a step that failed on its output, the excerpt of the block its agent produced.
/// Returns `None` for other steps, or when the agent run or its result can't be found:
/// this only decorates error display, so lookups never fail loudly.
pub fn step_output_error_excerpt(conn: &Connection, step: &WorkflowRunStep) -> Option<String> {
    if !step_has_output_error(step) {
        return None;
    }
    let run = AgentManager::new(conn)
        .get_run(step.child_run_id.as_deref()?)
        .ok()??;
    let log_path = run.log_path().ok()?;
    flow_output_excerpt(&final_result_text(&log_path, run.result_text.as_deref())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_block_on_first_line() {
        let text = "<<<FLOW_OUTPUT>>>\n{\"markers\": []}\n<<<END_FLOW_OUTPUT>>>";
        let e = flow_output_excerpt(text).unwrap();
        assert!(e.contains("line 1 of result"), "{e}");
        assert!(e.contains("{\"markers\": []}"), "{e}");
    }

    #[test]
    fn excerpt_reports_line_of_block_start() {
        let text = "intro\n\nmore\n<<<FLOW_OUTPUT>>>\n{\"markers\": []}\n<<<END_FLOW_OUTPUT>>>\n";
        let e = flow_output_excerpt(text).unwrap();
        assert!(e.contains("line 4 of result"), "{e}");
    }

    #[test]
    fn excerpt_skips_marker_quoted_in_prose() {
        let text = "Use `<<<FLOW_OUTPUT>>>` to start a block.\nok\n<<<FLOW_OUTPUT>>>\n{\"markers\": [\"x\"]}\n<<<END_FLOW_OUTPUT>>>";
        let e = flow_output_excerpt(text).unwrap();
        assert!(e.contains("line 3 of result"), "{e}");
        assert!(e.contains("\"x\""), "{e}");
    }

    #[test]
    fn excerpt_truncates_on_char_boundary() {
        let body = "é".repeat(300);
        let text = format!("<<<FLOW_OUTPUT>>>\n{{\"context\": \"{body}\"}}\n<<<END_FLOW_OUTPUT>>>");
        let e = flow_output_excerpt(&text).unwrap();
        assert!(e.ends_with('…'), "{e}");
        assert!(e.contains(&"é".repeat(10)), "{e}");
    }

    #[test]
    fn excerpt_none_without_block() {
        assert!(flow_output_excerpt("no block here").is_none());
        assert!(flow_output_excerpt("<<<FLOW_OUTPUT>>>\n{}").is_none());
    }

    #[test]
    fn predicate_matches_runkon_errors() {
        assert!(is_output_parse_error(
            "structured output validation: Invalid JSON in FLOW_OUTPUT: key must be a string at line 1 column 2"
        ));
        assert!(is_output_parse_error("no FLOW_OUTPUT block found"));
        assert!(!is_output_parse_error("process exited with code 1"));
    }

    // --- step_output_error_excerpt ---

    use crate::test_helpers::{seed_step_with_agent_run, setup_db_with_agent_run};
    use crate::workflow::create_workflow_run;

    const PARSE_ERR: &str = "structured output validation: Invalid JSON in FLOW_OUTPUT: oops";

    fn block(body: &str) -> String {
        format!("<<<FLOW_OUTPUT>>>\n{body}\n<<<END_FLOW_OUTPUT>>>")
    }

    fn result_event(text: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type": "result", "result": text})
        )
    }

    /// Seed a failed step (error `err`) whose agent run has the given stored result and log.
    fn failed_step(
        conn: &Connection,
        err: Option<&str>,
        stored: Option<&str>,
        log: &str,
    ) -> (WorkflowRunStep, std::path::PathBuf) {
        let run = create_workflow_run(conn, "wf", None, "ar1", false, "manual", None).unwrap();
        let seeded = seed_step_with_agent_run(
            conn,
            &run.id,
            "plan",
            0,
            WorkflowStepStatus::Failed,
            err,
            stored,
            log,
        );
        let step = crate::workflow::get_workflow_steps(conn, &run.id)
            .unwrap()
            .remove(0);
        (step, seeded.log_path)
    }

    #[test]
    fn step_excerpt_none_for_non_parse_error() {
        let conn = setup_db_with_agent_run();
        let (step, path) = failed_step(
            &conn,
            Some("process exited with code 1"),
            Some(&block("{}")),
            "",
        );
        assert!(step_output_error_excerpt(&conn, &step).is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn step_excerpt_none_without_child_run() {
        let conn = setup_db_with_agent_run();
        let step = WorkflowRunStep {
            step_error: Some(PARSE_ERR.into()),
            child_run_id: None,
            ..Default::default()
        };
        assert!(step_output_error_excerpt(&conn, &step).is_none());
    }

    #[test]
    fn step_excerpt_none_when_agent_run_missing() {
        let conn = setup_db_with_agent_run();
        let step = WorkflowRunStep {
            step_error: Some(PARSE_ERR.into()),
            child_run_id: Some("missing".into()),
            ..Default::default()
        };
        assert!(step_output_error_excerpt(&conn, &step).is_none());
    }

    #[test]
    fn step_excerpt_uses_result_text_error_on_failed_step() {
        let conn = setup_db_with_agent_run();
        let (mut step, path) = failed_step(&conn, None, Some(&block("{stored")), "");
        // No step_error: the failed step's result_text carries the parse error.
        step.step_error = None;
        step.result_text = Some(PARSE_ERR.into());
        let e = step_output_error_excerpt(&conn, &step).unwrap();
        assert!(e.contains("{stored"), "{e}");
        // A non-failed step's result_text is not an error.
        step.status = WorkflowStepStatus::Completed;
        assert!(step_output_error_excerpt(&conn, &step).is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn step_excerpt_prefers_log_result_over_stored_text() {
        let conn = setup_db_with_agent_run();
        let log = result_event(&block("{from-log"));
        let (step, path) = failed_step(&conn, Some(PARSE_ERR), Some(&block("{stored")), &log);
        let e = step_output_error_excerpt(&conn, &step).unwrap();
        assert!(e.contains("{from-log"), "{e}");
        assert!(!e.contains("{stored"), "{e}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn step_excerpt_falls_back_to_stored_text_when_log_has_no_result() {
        let conn = setup_db_with_agent_run();
        let (step, path) = failed_step(
            &conn,
            Some(PARSE_ERR),
            Some(&block("{stored")),
            "{\"type\":\"assistant\"}\n",
        );
        let e = step_output_error_excerpt(&conn, &step).unwrap();
        assert!(e.contains("{stored"), "{e}");
        std::fs::remove_file(path).ok();
    }
}
