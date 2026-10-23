//! Excerpts of the FLOW_OUTPUT block an agent produced, shown next to output
//! parse and validation errors so the reader can see what the parser was given.

use rusqlite::Connection;

use super::{WorkflowRunStep, WorkflowStepStatus};
use crate::agent::{read_log_result, AgentManager};

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

/// True when `err` reads like a FLOW_OUTPUT parse or validation failure.
pub fn is_output_parse_error(err: &str) -> bool {
    err.contains("structured output validation") || err.contains("FLOW_OUTPUT")
}

/// For a step that failed on its output, the excerpt of the block its agent produced.
/// Returns `None` for other steps, or when the agent run or its result can't be found:
/// this only decorates error display, so lookups never fail loudly.
pub fn step_output_error_excerpt(conn: &Connection, step: &WorkflowRunStep) -> Option<String> {
    let err = step.step_error.as_deref().or_else(|| {
        step.result_text
            .as_deref()
            .filter(|_| step.status == WorkflowStepStatus::Failed)
    })?;
    if !is_output_parse_error(err) {
        return None;
    }
    let mgr = AgentManager::new(conn);
    let run = mgr.get_run(step.child_run_id.as_deref()?).ok()??;
    // Prefer the log: some failure paths overwrite `result_text` with an error message.
    let from_log = run
        .log_path()
        .ok()
        .and_then(|p| read_log_result(&p))
        .and_then(|r| r.result_text);
    flow_output_excerpt(&from_log.or(run.result_text)?)
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
}
