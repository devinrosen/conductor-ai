use crate::state::Modal;

use super::App;

impl App {
    pub(super) fn handle_copy_last_code_block(&mut self) {
        let run = self.selected_worktree_run();

        let log_path = run.and_then(|r| r.log_file.as_deref());
        let Some(log_path) = log_path else {
            self.state.status_message = Some("No agent log available".to_string());
            return;
        };

        let file = match std::fs::File::open(log_path) {
            Ok(f) => f,
            Err(e) => {
                self.state.status_message = Some(format!("Failed to read log: {e}"));
                return;
            }
        };
        let reader = std::io::BufReader::new(file);

        let Some(code_block) = extract_last_code_block(reader) else {
            self.state.status_message = Some("No code block found in log".to_string());
            return;
        };

        self.copy_text_to_clipboard(code_block);
    }

    pub(super) fn handle_expand_agent_event(&mut self) {
        let selected = self.state.agent_list_state.borrow().selected().unwrap_or(0);

        let Some(ev) = self.state.data.event_at_visual_index(selected) else {
            return;
        };

        let summary_prefix = truncate_to_char_boundary(&ev.summary, 60);
        let title = format!("[{}] {}", ev.kind, summary_prefix);
        let body = if let Some(error_text) = ev.error_detail_text() {
            format!("{}\n\n--- Error Details ---\n{}", ev.summary, error_text)
        } else {
            ev.summary.clone()
        };
        let line_count = body.lines().count();

        self.state.modal = Modal::EventDetail {
            title,
            body,
            line_count,
            scroll_offset: 0,
            horizontal_offset: 0,
        };
    }
}

/// Extract the last fenced code block (```...```) from a reader (line-by-line streaming).
pub(super) fn extract_last_code_block(reader: impl std::io::BufRead) -> Option<String> {
    let mut last_block: Option<String> = None;
    let mut in_block = false;
    let mut current_block = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim_start().starts_with("```") {
            if in_block {
                // Closing fence — save the block (take avoids clone)
                last_block = Some(std::mem::take(&mut current_block));
                in_block = false;
            } else {
                // Opening fence
                in_block = true;
                current_block.clear();
            }
        } else if in_block {
            if !current_block.is_empty() {
                current_block.push('\n');
            }
            current_block.push_str(&line);
        }
    }

    last_block
}

/// Truncate a string to at most `max_chars` characters at a char boundary.
fn truncate_to_char_boundary(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}
