use crate::state::{info_row, repo_info_row, Modal, WorktreeDetailFocus};

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
        let body = if ev.kind == "tool_error" {
            // Show the full error text from metadata if available
            if let Some(ref meta) = ev.metadata {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(meta) {
                    if let Some(error_text) = parsed.get("error_text").and_then(|v| v.as_str()) {
                        format!("{}\n\n--- Error Details ---\n{}", ev.summary, error_text)
                    } else {
                        ev.summary.clone()
                    }
                } else {
                    ev.summary.clone()
                }
            } else {
                ev.summary.clone()
            }
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

    pub(super) fn handle_worktree_detail_copy(&mut self) {
        match self.state.worktree_detail_focus {
            WorktreeDetailFocus::LogPanel => {
                self.handle_copy_last_code_block();
            }
            WorktreeDetailFocus::InfoPanel => {
                let wt = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id));
                let Some(wt) = wt else {
                    return;
                };
                let row = self.state.worktree_detail_selected_row;
                let repo_slug = self
                    .state
                    .data
                    .repo_slug_map
                    .get(&wt.repo_id)
                    .cloned()
                    .unwrap_or_else(|| "?".to_string());
                let value = match row {
                    info_row::SLUG => wt.slug.clone(),
                    info_row::REPO => repo_slug,
                    info_row::BRANCH => wt.branch.clone(),
                    info_row::BASE => wt
                        .base_branch
                        .clone()
                        .unwrap_or_else(|| "(repo default)".to_string()),
                    info_row::PATH => wt.path.clone(),
                    info_row::STATUS => wt.status.to_string(),
                    info_row::MODEL => wt.model.clone().unwrap_or_else(|| "(not set)".to_string()),
                    info_row::CREATED => wt.created_at.clone(),
                    info_row::TICKET => {
                        let url = wt
                            .ticket_id
                            .as_ref()
                            .and_then(|tid| self.state.data.ticket_map.get(tid))
                            .map(|t| t.url.clone())
                            .unwrap_or_default();
                        if url.is_empty() {
                            self.state.status_message =
                                Some("No ticket linked to this worktree".to_string());
                            return;
                        }
                        url
                    }
                    _ => {
                        self.state.status_message = Some("Nothing to copy on this row".to_string());
                        return;
                    }
                };
                self.copy_text_to_clipboard(value);
            }
        }
    }

    pub(super) fn handle_worktree_detail_open(&mut self) {
        if self.state.worktree_detail_focus != WorktreeDetailFocus::InfoPanel {
            return;
        }
        let row = self.state.worktree_detail_selected_row;
        match row {
            info_row::PATH => {
                let Some(path) = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                    .map(|wt| wt.path.clone())
                else {
                    return;
                };
                self.open_terminal_at_path(&path);
            }
            info_row::TICKET => {
                // Ticket row: open the ticket URL in the default browser
                let url = self
                    .state
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| self.state.data.worktrees.iter().find(|w| &w.id == id))
                    .and_then(|wt| wt.ticket_id.as_ref())
                    .and_then(|tid| self.state.data.ticket_map.get(tid))
                    .map(|t| t.url.clone());
                match url {
                    Some(ref u) if !u.is_empty() => {
                        let u = u.clone();
                        self.open_url(&u, "ticket");
                    }
                    _ => {
                        self.state.status_message =
                            Some("No ticket linked to this worktree".to_string());
                    }
                }
            }
            _ => {
                self.state.status_message =
                    Some("No action for this row (try Path or Ticket row)".to_string());
            }
        }
    }

    pub(super) fn handle_repo_detail_info_open(&mut self) {
        let row = self.state.repo_detail_info_row;
        let repo = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id));
        let Some(repo) = repo else { return };
        match row {
            repo_info_row::SLUG | repo_info_row::REMOTE => match self.repo_web_url() {
                Some(url) => self.open_url(&url, "repo"),
                None => {
                    self.state.status_message =
                        Some("No GitHub URL found for this repo".to_string());
                }
            },
            repo_info_row::PATH => {
                let path = repo.local_path.clone();
                self.open_terminal_at_path(&path);
            }
            repo_info_row::WORKTREES_DIR => {
                let path = repo.workspace_dir.clone();
                self.open_terminal_at_path(&path);
            }
            _ => {
                self.state.status_message = Some("No action for this row".to_string());
            }
        }
    }

    pub(super) fn handle_repo_detail_info_copy(&mut self) {
        let row = self.state.repo_detail_info_row;
        let repo = self
            .state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id));
        let Some(repo) = repo else { return };
        let text = match row {
            repo_info_row::SLUG => repo.slug.clone(),
            repo_info_row::REMOTE => repo.remote_url.clone(),
            repo_info_row::BRANCH => repo.default_branch.clone(),
            repo_info_row::PATH => repo.local_path.clone(),
            repo_info_row::WORKTREES_DIR => repo.workspace_dir.clone(),
            repo_info_row::MODEL => repo
                .model
                .clone()
                .unwrap_or_else(|| "(not set)".to_string()),
            _ => return,
        };
        self.copy_text_to_clipboard(text);
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
