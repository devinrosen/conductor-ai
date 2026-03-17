use std::process::Command;

use crate::state::{Modal, RepoDetailFocus, View};

use super::App;

impl App {
    /// Resolve the URL of the currently focused ticket, across all contexts.
    pub(super) fn selected_ticket_url(&self) -> Option<String> {
        if let Modal::TicketInfo { ref ticket } = self.state.modal {
            return Some(ticket.url.clone());
        }
        if self.state.view == View::WorktreeDetail {
            return self
                .state
                .selected_worktree_id
                .as_ref()
                .and_then(|wt_id| self.state.data.worktrees.iter().find(|w| &w.id == wt_id))
                .and_then(|wt| wt.ticket_id.as_ref())
                .and_then(|tid| self.state.data.ticket_map.get(tid))
                .map(|t| t.url.clone());
        }
        // Ticket list views: RepoDetail Tickets pane
        let ticket = match self.state.view {
            View::RepoDetail if self.state.repo_detail_focus == RepoDetailFocus::Tickets => self
                .state
                .filtered_detail_tickets
                .get(self.state.detail_ticket_index),
            _ => None,
        };
        ticket.map(|t| t.url.clone())
    }

    /// Open a URL in the default browser, checking the exit code.
    pub(super) fn open_url(&mut self, url: &str, label: &str) {
        match Command::new("open")
            .arg(url)
            .output()
            .or_else(|_| Command::new("xdg-open").arg(url).output())
        {
            Ok(output) if output.status.success() => {
                self.state.status_message = Some(format!("Opened {url}"));
            }
            Ok(output) => {
                let code = output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".to_string());
                self.state.status_message =
                    Some(format!("Failed to open {label} URL (exit {code})"));
            }
            Err(e) => {
                self.state.status_message = Some(format!("Failed to open {label} URL: {e}"));
            }
        }
    }

    pub(super) fn handle_open_ticket_url(&mut self) {
        match self.selected_ticket_url() {
            Some(url) => self.open_url(&url, "ticket"),
            None => {
                self.state.status_message = Some("No ticket URL available".to_string());
            }
        }
    }

    pub(super) fn handle_copy_ticket_url(&mut self) {
        match self.selected_ticket_url() {
            Some(url) => self.copy_text_to_clipboard(url),
            None => {
                self.state.status_message = Some("No ticket URL available".to_string());
            }
        }
    }

    pub(super) fn repo_web_url(&self) -> Option<String> {
        self.state
            .selected_repo_id
            .as_ref()
            .and_then(|id| self.state.data.repos.iter().find(|r| &r.id == id))
            .and_then(|repo| conductor_core::github::parse_github_remote(&repo.remote_url))
            .map(|(owner, repo)| format!("https://github.com/{owner}/{repo}"))
    }

    pub(super) fn handle_open_repo_url(&mut self) {
        match self.repo_web_url() {
            Some(url) => self.open_url(&url, "repo"),
            None => {
                self.state.status_message = Some("No repo GitHub URL available".to_string());
            }
        }
    }

    pub(super) fn handle_copy_repo_url(&mut self) {
        match self.repo_web_url() {
            Some(url) => self.copy_text_to_clipboard(url),
            None => {
                self.state.status_message = Some("No repo GitHub URL available".to_string());
            }
        }
    }

    pub(super) fn selected_pr_url(&self) -> Option<String> {
        self.state
            .detail_prs
            .get(self.state.detail_pr_index)
            .map(|pr| pr.url.clone())
    }

    pub(super) fn handle_open_pr_url(&mut self) {
        match self.selected_pr_url() {
            Some(url) => self.open_url(&url, "PR"),
            None => {
                self.state.status_message = Some("No PR URL available".to_string());
            }
        }
    }

    pub(super) fn handle_copy_pr_url(&mut self) {
        match self.selected_pr_url() {
            Some(url) => self.copy_text_to_clipboard(url),
            None => {
                self.state.status_message = Some("No PR URL available".to_string());
            }
        }
    }

    /// Open a new terminal window/tab at `path`, using the best available method:
    /// 1. Inside tmux → `tmux new-window -c {path}`
    /// 2. TERM_PROGRAM=Apple_Terminal → AppleScript `do script "cd {path}"`
    /// 3. TERM_PROGRAM=iTerm.app → AppleScript create iTerm2 window at path
    /// 4. Fallback → status message with hint
    pub(super) fn open_terminal_at_path(&mut self, path: &str) {
        // 1. tmux: preferred when the TUI is already running inside a tmux session
        if std::env::var("TMUX").is_ok() {
            match Command::new("tmux")
                .args(["new-window", "-c", path])
                .output()
            {
                Ok(out) if out.status.success() => {
                    self.state.status_message = Some(format!("Opened tmux window at {path}"));
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    self.state.status_message = Some(format!("tmux error: {}", stderr.trim()));
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Failed to open tmux window: {e}"));
                }
            }
            return;
        }

        // 2 & 3. AppleScript for macOS terminal apps.
        // Embed the path via an AppleScript variable so `quoted form of` handles
        // all shell-special characters without manual escaping.
        let term = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let script: Option<String> = match term.as_str() {
            "Apple_Terminal" => Some(format!(
                "set p to \"{path}\"\n\
                 tell application \"Terminal\"\n\
                 \tdo script \"cd \" & quoted form of p\n\
                 \tactivate\n\
                 end tell",
                path = path.replace('\\', "\\\\").replace('"', "\\\"")
            )),
            "iTerm.app" | "iTerm2" => Some(format!(
                "set p to \"{path}\"\n\
                 tell application \"iTerm\"\n\
                 \tactivate\n\
                 \tcreate window with default profile\n\
                 \ttell current session of current window\n\
                 \t\twrite text \"cd \" & quoted form of p\n\
                 \tend tell\n\
                 end tell",
                path = path.replace('\\', "\\\\").replace('"', "\\\"")
            )),
            _ => None,
        };

        if let Some(script) = script {
            match Command::new("osascript").arg("-e").arg(&script).output() {
                Ok(out) if out.status.success() => {
                    self.state.status_message = Some(format!("Opened terminal at {path}"));
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    self.state.status_message =
                        Some(format!("Failed to open terminal: {}", stderr.trim()));
                }
                Err(e) => {
                    self.state.status_message = Some(format!("Failed to open terminal: {e}"));
                }
            }
        } else {
            // 4. Unknown environment — guide the user
            let hint = if term.is_empty() {
                "Run inside tmux or set TERM_PROGRAM".to_string()
            } else {
                format!("Terminal '{term}' not supported — run inside tmux")
            };
            self.state.status_message = Some(hint);
        }
    }

    /// Copy arbitrary text to the system clipboard via pbcopy/xclip/xsel.
    pub(super) fn copy_text_to_clipboard(&mut self, text: String) {
        let copy_result = Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .or_else(|_| {
                Command::new("xclip")
                    .args(["-selection", "clipboard"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
            })
            .or_else(|_| {
                Command::new("xsel")
                    .arg("--clipboard")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
            });

        match copy_result {
            Ok(mut child) => {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    if stdin.write_all(text.as_bytes()).is_err() {
                        self.state.status_message = Some("Clipboard write failed".to_string());
                        return;
                    }
                    drop(stdin);
                }
                // Fire-and-forget: pbcopy/xclip/xsel completes almost instantly.
                drop(child);
                self.state.status_message = Some("Copied to clipboard".to_string());
            }
            Err(_) => {
                self.state.status_message =
                    Some("No clipboard tool found (pbcopy/xclip/xsel)".to_string());
            }
        }
    }
}
