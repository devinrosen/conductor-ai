use std::process::Command;

use conductor_core::agent::PlanStep;
use conductor_core::error::ConductorError;
use conductor_core::tickets::TicketInput;

pub(crate) fn check_prerequisites() {
    let mut missing = Vec::new();
    if Command::new("gh").arg("--version").output().is_err() {
        missing.push("  - gh (GitHub CLI): https://cli.github.com");
    }
    if Command::new("tmux").arg("-V").output().is_err() {
        missing.push("  - tmux: https://github.com/tmux/tmux");
    }
    if Command::new("claude").arg("--version").output().is_err() {
        missing
            .push("  - claude (Claude Code CLI): https://docs.anthropic.com/en/docs/claude-code");
    }
    if !missing.is_empty() {
        eprintln!("conductor: missing prerequisites:\n{}", missing.join("\n"));
        eprintln!("Some commands may not work until these are resolved.\n");
    }
}

pub(crate) fn report_workflow_result(result: conductor_core::workflow::WorkflowResult) {
    println!(
        "\nTotal: {} turns, {:.1}s",
        result.total_turns,
        result.total_duration_ms as f64 / 1000.0
    );
    if result.all_succeeded {
        println!("Workflow completed successfully.");
    } else {
        eprintln!("Workflow finished with failures.");
        std::process::exit(1);
    }
}

/// Parse a comma-separated list of ticket IDs, trimming whitespace and dropping empties.
pub(crate) fn parse_ticket_ids(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Generate an implementation plan by asking Claude to produce a JSON step list.
///
/// Returns `None` (non-fatal) if plan generation fails or produces no steps.
pub(crate) fn generate_plan(
    worktree_path: &str,
    prompt: &str,
    config: &conductor_core::config::Config,
) -> Option<Vec<PlanStep>> {
    let plan_prompt = format!(
        "You are planning a software development task. \
         Generate a concise implementation plan as a JSON array.\n\n\
         Task:\n{prompt}\n\n\
         Respond with ONLY a JSON array of step objects. \
         Each object must have a \"description\" string field. \
         No markdown, no backticks, no explanation — just the raw JSON array. \
         Example: [{{\"description\":\"Read the relevant files\"}},{{\"description\":\"Write tests\"}}]\n\
         Aim for 3-8 concrete, actionable steps."
    );

    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg(&plan_prompt)
        .arg("--output-format")
        .arg("json")
        .arg(config.general.agent_permission_mode.cli_flag());
    if let Some(val) = config.general.agent_permission_mode.cli_flag_value() {
        cmd.arg(val);
    }
    let output = cmd.current_dir(worktree_path).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let result_text = response.get("result")?.as_str()?;

    // Strip optional markdown code fences that Claude sometimes adds
    let trimmed = result_text.trim();
    let json_text = if let Some(inner) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        inner.trim_end_matches("```").trim()
    } else {
        trimmed
    };

    let raw: Vec<serde_json::Value> = serde_json::from_str(json_text).ok()?;
    let steps: Vec<PlanStep> = raw
        .into_iter()
        .filter_map(|v| {
            let description = v.get("description")?.as_str()?.to_string();
            Some(PlanStep {
                description,
                ..Default::default()
            })
        })
        .collect();

    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
}

/// Read `path` and, if it is an internal conductor temp file
/// (`conductor-prompt-*.txt` in the system temp directory), delete it afterwards.
///
/// User-supplied files passed via `--prompt-file` are left untouched.
pub(crate) fn read_and_maybe_cleanup_prompt_file(path: &str) -> anyhow::Result<String> {
    use anyhow::Context;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read prompt file: {path}"))?;
    let p = std::path::Path::new(path);
    if let Some(filename) = p.file_name().and_then(|f| f.to_str()) {
        if filename.starts_with("conductor-prompt-")
            && filename.ends_with(".txt")
            && p.parent() == Some(std::env::temp_dir().as_path())
        {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(content)
}

/// Sync issues for a single repo using the given fetch closure, printing results.
pub(crate) fn sync_repo(
    syncer: &conductor_core::tickets::TicketSyncer,
    repo_id: &str,
    repo_slug: &str,
    source_type: &str,
    label: &str,
    fetch: impl FnOnce() -> Result<Vec<TicketInput>, ConductorError>,
) {
    match fetch() {
        Ok(tickets) => {
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            match syncer.upsert_tickets(repo_id, &tickets) {
                Ok(count) => {
                    let closed = syncer
                        .close_missing_tickets(repo_id, source_type, &synced_ids)
                        .unwrap_or_else(|e| {
                            eprintln!("  {repo_slug} — warning: close_missing_tickets failed: {e}");
                            0
                        });
                    let merged = syncer
                        .mark_worktrees_for_closed_tickets(repo_id)
                        .unwrap_or_else(|e| {
                            eprintln!(
                                "  {repo_slug} — warning: mark_worktrees_for_closed_tickets failed: {e}"
                            );
                            0
                        });
                    print!("  {} — synced {count} {label}", repo_slug);
                    if count == 0 {
                        print!(" (no items matched — check issue source configuration)");
                    }
                    if closed > 0 {
                        print!(", {closed} marked closed");
                    }
                    if merged > 0 {
                        print!(", {merged} worktrees merged");
                    }
                    println!();
                }
                Err(e) => {
                    eprintln!("  {} — sync failed: {e}", repo_slug);
                }
            }
        }
        Err(e) => {
            eprintln!("  {} — sync failed: {e}", repo_slug);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_ticket_ids, read_and_maybe_cleanup_prompt_file, truncate_str};

    #[test]
    fn internal_temp_file_is_deleted_after_read() {
        let tmp = std::env::temp_dir();
        let path = tmp.join("conductor-prompt-run-abc123.txt");
        std::fs::write(&path, "hello").unwrap();
        let content = read_and_maybe_cleanup_prompt_file(path.to_str().unwrap()).unwrap();
        assert_eq!(content, "hello");
        assert!(
            !path.exists(),
            "internal temp file should have been deleted"
        );
    }

    #[test]
    fn user_prompt_file_is_not_deleted_after_read() {
        let tmp = std::env::temp_dir();
        let path = tmp.join("my-custom-prompt.txt");
        std::fs::write(&path, "custom prompt").unwrap();
        let content = read_and_maybe_cleanup_prompt_file(path.to_str().unwrap()).unwrap();
        assert_eq!(content, "custom prompt");
        assert!(
            path.exists(),
            "user-provided prompt file should not be deleted"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nonexistent_file_returns_error() {
        let result = read_and_maybe_cleanup_prompt_file(
            "/tmp/conductor-nonexistent-file-that-does-not-exist.txt",
        );
        assert!(result.is_err(), "expected Err for nonexistent file, got Ok");
    }

    #[test]
    fn parse_ticket_ids_basic() {
        assert_eq!(parse_ticket_ids("ABC-1,ABC-2"), vec!["ABC-1", "ABC-2"]);
    }

    #[test]
    fn parse_ticket_ids_trims_whitespace() {
        assert_eq!(
            parse_ticket_ids("  ABC-1 , ABC-2 , ABC-3 "),
            vec!["ABC-1", "ABC-2", "ABC-3"]
        );
    }

    #[test]
    fn parse_ticket_ids_drops_empties() {
        assert_eq!(parse_ticket_ids(",ABC-1,,ABC-2,"), vec!["ABC-1", "ABC-2"]);
    }

    #[test]
    fn parse_ticket_ids_single() {
        assert_eq!(parse_ticket_ids("ABC-1"), vec!["ABC-1"]);
    }

    #[test]
    fn parse_ticket_ids_empty_string() {
        let result: Vec<String> = parse_ticket_ids("");
        assert!(result.is_empty());
    }

    #[test]
    fn truncate_str_ascii() {
        assert_eq!(truncate_str("hello world", 11), "hello world");
        assert_eq!(truncate_str("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_str_multibyte_no_panic() {
        // 5 two-byte chars = 10 bytes; truncating at char boundary must not panic
        let s = "ääääää"; // 6 chars, each 2 bytes
        let result = truncate_str(s, 5);
        assert!(result.ends_with("..."));
        // Should have 2 'ä' chars + "..."
        assert_eq!(result, "ää...");
    }

    #[test]
    fn truncate_str_emoji() {
        let s = "👋🌍🚀✨🎉";
        let result = truncate_str(s, 4);
        assert_eq!(result, "👋...");
    }
}
