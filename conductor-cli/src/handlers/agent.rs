use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use anyhow::Result;
use rusqlite::Connection;

use conductor_core::agent::{
    build_startup_context, parse_events_from_line, AgentManager, PlanStep,
};
use conductor_core::config::{load_config, Config};
use conductor_core::github;
use conductor_core::github_app;
use conductor_core::orchestrator::{self, OrchestratorConfig};
use conductor_core::repo::RepoManager;
use conductor_core::workflow::WorkflowManager;
use conductor_core::worktree::WorktreeManager;

use crate::commands::{AgentCommands, CONDUCTOR_RUN_ID_ENV};
use crate::helpers::{generate_plan, read_and_maybe_cleanup_prompt_file};

pub fn handle_agent(command: AgentCommands, conn: &Connection, config: &Config) -> Result<()> {
    // Reap orphaned runs before handling any agent command.
    {
        let agent_mgr = AgentManager::new(conn);
        if let Err(e) = agent_mgr.reap_orphaned_runs() {
            eprintln!("Warning: reap_orphaned_runs failed: {e}");
        }
        if let Err(e) = agent_mgr.dismiss_expired_feedback_requests() {
            eprintln!("Warning: dismiss_expired_feedback_requests failed: {e}");
        }
        let wf_mgr = WorkflowManager::new(conn);
        if let Err(e) = wf_mgr.reap_orphaned_workflow_runs() {
            eprintln!("Warning: reap_orphaned_workflow_runs failed: {e}");
        }
    }

    match command {
        AgentCommands::Run {
            run_id,
            worktree_path,
            prompt,
            prompt_file,
            resume,
            model,
            bot_name,
            permission_mode,
            plugin_dirs,
        } => {
            let resolved_prompt = match (prompt, prompt_file) {
                (Some(p), _) => p,
                (None, Some(path)) => read_and_maybe_cleanup_prompt_file(&path)?,
                (None, None) => {
                    anyhow::bail!("Either --prompt or --prompt-file is required")
                }
            };
            let perm_mode = match permission_mode.as_deref() {
                Some("plan") => Some(conductor_core::config::AgentPermissionMode::Plan),
                Some("repo-safe") => Some(conductor_core::config::AgentPermissionMode::RepoSafe),
                Some(other) => {
                    anyhow::bail!(
                        "Unknown permission-mode '{}'; valid values: plan, repo-safe",
                        other
                    )
                }
                None => None,
            };
            run_agent(
                conn,
                &run_id,
                &worktree_path,
                &resolved_prompt,
                resume.as_deref(),
                model.as_deref(),
                bot_name.as_deref(),
                perm_mode.as_ref(),
                &plugin_dirs,
            )?;
        }
        AgentCommands::Orchestrate {
            run_id,
            worktree_path,
            model,
            fail_fast,
            child_timeout_secs,
        } => {
            run_orchestrate(
                conn,
                config,
                &run_id,
                &worktree_path,
                model.as_deref(),
                fail_fast,
                child_timeout_secs,
            )?;
        }
        AgentCommands::CreateIssue {
            title,
            body,
            run_id,
        } => {
            let run_id = run_id
                .or_else(|| std::env::var(CONDUCTOR_RUN_ID_ENV).ok())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "No run ID provided. Use --run-id or set ${CONDUCTOR_RUN_ID_ENV}."
                    )
                })?;

            let agent_mgr = AgentManager::new(conn);

            // Look up run → worktree → repo
            let run = agent_mgr
                .get_run(&run_id)?
                .ok_or_else(|| anyhow::anyhow!("Agent run not found: {run_id}"))?;

            let worktree_id = run.worktree_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot create issues from ephemeral workflow runs \
                     (run {run_id} has no registered worktree)"
                )
            })?;

            let wt_mgr = WorktreeManager::new(conn, config);
            let wt = wt_mgr
                .get_by_id(worktree_id)
                .map_err(|e| anyhow::anyhow!("Could not find worktree {worktree_id}: {e}"))?;

            let repo_mgr = RepoManager::new(conn, config);
            let repo_obj = repo_mgr.get_by_id(&wt.repo_id).map_err(|e| {
                anyhow::anyhow!("Could not find repo for worktree {worktree_id}: {e}")
            })?;

            let repo_id = &repo_obj.id;
            let remote_url = &repo_obj.remote_url;
            let allow = repo_obj.allow_agent_issue_creation;

            if !allow {
                anyhow::bail!(
                    "Agent issue creation is disabled for this repo. \
                 Enable it via `conductor repo allow-agent-issues <repo-slug>`."
                );
            }

            // Determine GitHub owner/repo from remote URL
            let (owner, repo_name) = github::parse_github_remote(remote_url).ok_or_else(|| {
                anyhow::anyhow!("Cannot determine GitHub repo from remote URL: {remote_url}")
            })?;

            // Create the GitHub issue
            let (source_id, url) =
                github::create_github_issue(&owner, &repo_name, &title, &body, &[], None)?;

            // Record in DB
            agent_mgr.record_created_issue(&run_id, repo_id, "github", &source_id, &title, &url)?;

            println!("Created issue #{source_id}: {url}");
        }
    }
    Ok(())
}

/// Run a Claude agent for a worktree. Called inside a tmux window by the TUI.
///
/// Uses `--output-format json` (single JSON result) since the tmux terminal IS the display.
/// Claude's interactive output goes directly to the terminal; we only parse the final JSON result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_agent(
    conn: &rusqlite::Connection,
    run_id: &str,
    worktree_path: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    permission_mode_override: Option<&conductor_core::config::AgentPermissionMode>,
    extra_plugin_dirs: &[String],
) -> Result<()> {
    let mgr = AgentManager::new(conn);

    // Verify the run exists
    let run = mgr.get_run(run_id)?;
    let run = match run {
        Some(r) => r,
        None => anyhow::bail!("agent run not found: {run_id}"),
    };

    // Build effective prompt with optional startup context
    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[conductor] Warning: failed to load config, GitHub App auth will be skipped: {e}"
            );
            conductor_core::config::Config::default()
        }
    };
    let effective_prompt = if config.general.inject_startup_context {
        let context = build_startup_context(
            conn,
            &config,
            run.worktree_id.as_deref(),
            run_id,
            worktree_path,
        );
        eprintln!("[conductor] Injecting session context into prompt");
        format!("{context}\n\n---\n\n{prompt}")
    } else {
        prompt.to_string()
    };

    // Phase 1: Plan generation (only for new runs, not resumes)
    if resume_session_id.is_none() {
        eprintln!("[conductor] Phase 1: Generating plan...");
        match generate_plan(worktree_path, &effective_prompt, &config) {
            Some(steps) => {
                eprintln!("[conductor] Plan ({} steps):", steps.len());
                for (i, step) in steps.iter().enumerate() {
                    eprintln!("  {}. {}", i + 1, step.description);
                }
                if let Err(e) = mgr.update_run_plan(run_id, &steps) {
                    eprintln!("[conductor] Warning: could not save plan to DB: {e}");
                }
            }
            None => {
                eprintln!("[conductor] Plan generation skipped (no steps returned)");
            }
        }
        eprintln!("[conductor] Phase 2: Executing...");
    } else {
        // Resuming: carry forward the plan from the previous run
        // Look up the previous run that owns this session_id
        if let Some(wt_id) = run.worktree_id.as_deref() {
            if let Ok(prev_runs) = mgr.list_for_worktree(wt_id) {
                let prev_run = prev_runs.iter().find(|r| {
                    r.claude_session_id.as_deref() == resume_session_id && r.id != run_id
                });
                if let Some(prev) = prev_run {
                    if prev.has_incomplete_plan_steps() {
                        let incomplete: Vec<&PlanStep> = prev.incomplete_plan_steps();
                        eprintln!(
                            "[conductor] Resuming with {} incomplete plan steps:",
                            incomplete.len()
                        );
                        for (i, step) in incomplete.iter().enumerate() {
                            eprintln!("  {}. {}", i + 1, step.description);
                        }
                        // Carry forward the full plan to the new run
                        if let Some(ref plan) = prev.plan {
                            if let Err(e) = mgr.update_run_plan(run_id, plan) {
                                eprintln!("[conductor] Warning: could not carry forward plan: {e}");
                            }
                        }
                    }
                }
            }
        }
        eprintln!("[conductor] Resuming session...");
    }

    eprintln!(
        "[conductor] Running agent for run_id={} in {}",
        run_id, worktree_path
    );

    // Emit the user's prompt as the first event so it appears in the activity log
    {
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(e) = mgr.create_event(run_id, "prompt", prompt, &now, None) {
            eprintln!("[conductor] Warning: could not persist prompt event: {e}");
        }
    }

    // Set up log file path once; created on turn 0, appended on feedback resume turns.
    let log_dir = conductor_core::config::agent_log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = conductor_core::config::agent_log_path(run_id);

    // session_id persists across turns so feedback resumes can use --resume <sid>
    let mut session_id_parsed: Option<String> = None;

    // Accumulated cost/token stats across all turns
    let mut acc_cost_usd: f64 = 0.0;
    let mut acc_input_tokens: i64 = 0;
    let mut acc_output_tokens: i64 = 0;
    let mut acc_cache_read_tokens: i64 = 0;
    let mut acc_cache_creation_tokens: i64 = 0;
    let mut acc_num_turns: i64 = 0;
    let mut acc_duration_ms: i64 = 0;

    // When Some, the next loop iteration is a feedback resume turn
    let mut feedback_response_for_resume: Option<String> = None;
    // True once at least one feedback resume turn has completed; used to decide
    // whether to override the eager DB update with accumulated totals at the end.
    let mut had_feedback_resume = false;

    loop {
        // ── per-turn mutable state ────────────────────────────────────────────
        let mut pending_feedback_id: Option<String> = None;
        let mut result_text: Option<String> = None;
        let mut cost_usd: Option<f64> = None;
        let mut num_turns: Option<i64> = None;
        let mut duration_ms: Option<i64> = None;
        let mut is_error = false;
        let mut input_tokens: Option<i64> = None;
        let mut output_tokens: Option<i64> = None;
        let mut cache_read_input_tokens: Option<i64> = None;
        let mut cache_creation_input_tokens: Option<i64> = None;
        let mut db_updated_eagerly = false;
        let mut last_event_id: Option<String> = None;

        // ── build command for this turn ───────────────────────────────────────
        // stdout: stream-json events (piped, parsed for result metadata)
        // stderr: verbose turn-by-turn output (inherited, visible in tmux)
        let mut cmd = Command::new("claude");
        if let Some(ref feedback) = feedback_response_for_resume {
            // Feedback resume turn: deliver the human response as the next message
            let sid = session_id_parsed
                .as_deref()
                .expect("session_id always captured before a feedback resume");
            cmd.arg("-p").arg(feedback).arg("--resume").arg(sid);
        } else {
            cmd.arg("-p").arg(&effective_prompt);
            if let Some(ref sid) = resume_session_id {
                cmd.arg("--resume").arg(sid);
            }
        }
        let effective_perm_mode =
            permission_mode_override.unwrap_or(&config.general.agent_permission_mode);
        cmd.arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg(effective_perm_mode.claude_permission_flag());
        if let Some(val) = effective_perm_mode.claude_permission_flag_value() {
            cmd.arg(val);
        }
        if let Some(pattern) = effective_perm_mode.allowed_tools() {
            cmd.arg("--allowedTools").arg(pattern);
        }
        cmd.env(CONDUCTOR_RUN_ID_ENV, run_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(worktree_path);

        // Pass CLAUDE_CONFIG_DIR when explicitly configured so agent runs use
        // the custom Claude config directory instead of the default ~/.claude.
        if config.general.claude_config_dir.is_some() {
            if let Ok(dir) = config.general.resolved_claude_config_dir() {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
        }

        // Inject GH_TOKEN from the GitHub App installation token so all `gh` calls
        // made by the agent (including `gh pr create`) use the bot identity rather
        // than the human `gh` CLI user. Fall back gracefully when not configured.
        match github_app::resolve_named_app_token(&config, bot_name, "agent-run") {
            github_app::TokenResolution::AppToken(token) => {
                if let Some(name) = bot_name {
                    eprintln!("[conductor] Using GitHub App token for bot identity: {name}");
                }
                cmd.env("GH_TOKEN", token);
            }
            github_app::TokenResolution::Fallback { reason } => {
                eprintln!(
                    "[conductor] Warning: GitHub App token failed, agents will use gh user identity: {reason}"
                );
            }
            github_app::TokenResolution::NotConfigured => {
                if let Some(name) = bot_name {
                    eprintln!(
                        "[conductor] Warning: bot name '{name}' specified but no matching GitHub App found in config — agents will use gh user identity"
                    );
                }
            }
        }

        if let Some(m) = model {
            cmd.arg("--model").arg(m);
        }

        for dir in extra_plugin_dirs {
            cmd.arg("--plugin-dir").arg(dir);
        }

        // ── open log file (create on turn 0, append on feedback resume turns) ─
        let mut log_file = if feedback_response_for_resume.is_some() {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .ok()
        } else {
            let f = std::fs::File::create(&log_path).ok();
            // Store log file path in DB so the TUI can read streaming events.
            if f.is_some() {
                let path_str = log_path.to_string_lossy().to_string();
                if let Err(e) = mgr.update_run_log_file(run_id, &path_str) {
                    eprintln!("[conductor] Warning: could not save log path to DB: {e}");
                }
            }
            f
        };

        // ── spawn ─────────────────────────────────────────────────────────────
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                let error_msg = format!("Failed to spawn claude: {e}");
                mgr.update_run_failed(run_id, &error_msg)?;
                eprintln!("[conductor] {}", error_msg);
                return Ok(());
            }
        };

        // ── drain stdout ──────────────────────────────────────────────────────
        if let Some(stdout) = child.stdout.take() {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { continue };

                // Write every line to the log file
                if let Some(ref mut f) = log_file {
                    let _ = writeln!(f, "{line}");
                }

                let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };

                // Display human-readable activity in the tmux terminal
                print_event_summary(&event);

                // Capture session_id from init message and save immediately for resume
                if let Some(sid) = event.get("session_id").and_then(|v| v.as_str()) {
                    session_id_parsed = Some(sid.to_string());
                    if let Err(e) = mgr.update_run_session_id(run_id, sid) {
                        eprintln!("[conductor] Warning: could not save session_id: {e}");
                    }
                }

                // Capture result from final message and eagerly update DB to
                // narrow the race window if the process is killed before child.wait().
                // Skip the eager update when feedback is pending — we must stay in
                // waiting_for_feedback until the human responds.
                if event.get("result").is_some() {
                    let parsed = conductor_core::agent::parse_result_event(&event);
                    result_text = parsed.result_text;
                    cost_usd = parsed.cost_usd;
                    num_turns = parsed.num_turns;
                    duration_ms = parsed.duration_ms;
                    is_error = parsed.is_error;
                    input_tokens = parsed.input_tokens;
                    output_tokens = parsed.output_tokens;
                    cache_read_input_tokens = parsed.cache_read_input_tokens;
                    cache_creation_input_tokens = parsed.cache_creation_input_tokens;

                    // Only eagerly mark complete/failed when no feedback is pending.
                    // If feedback was requested, the run must stay in waiting_for_feedback
                    // until the human responds; resume_run_after_feedback() transitions it
                    // back to running when the TUI submits the response.
                    if pending_feedback_id.is_none() {
                        if is_error {
                            let error_msg = result_text
                                .as_deref()
                                .unwrap_or(conductor_core::agent::DEFAULT_AGENT_ERROR_MSG);
                            if let Err(e) = mgr.update_run_failed_with_session(
                                run_id,
                                error_msg,
                                session_id_parsed.as_deref(),
                            ) {
                                eprintln!("[conductor] Warning: eager DB update failed: {e}");
                            }
                        } else if let Err(e) = mgr.update_run_completed(
                            run_id,
                            session_id_parsed.as_deref(),
                            result_text.as_deref(),
                            cost_usd,
                            num_turns,
                            duration_ms,
                            input_tokens,
                            output_tokens,
                            cache_read_input_tokens,
                            cache_creation_input_tokens,
                        ) {
                            eprintln!("[conductor] Warning: eager DB update failed: {e}");
                        }
                        db_updated_eagerly = true;
                    }
                }

                // Persist parsed events to DB as spans
                let parsed = parse_events_from_line(&line);
                if !parsed.is_empty() {
                    let now = chrono::Utc::now().to_rfc3339();
                    // Close the previous span
                    if let Some(ref prev_id) = last_event_id {
                        let _ = mgr.update_event_ended_at(prev_id, &now);
                    }
                    // Create a new span for each parsed event; only the last one stays open
                    for ev in &parsed {
                        match mgr.create_event(
                            run_id,
                            &ev.kind,
                            &ev.summary,
                            &now,
                            ev.metadata.as_deref(),
                        ) {
                            Ok(db_ev) => last_event_id = Some(db_ev.id),
                            Err(e) => {
                                eprintln!("[conductor] Warning: could not persist event: {e}")
                            }
                        }

                        // Detect feedback request markers in agent text output.
                        // Record the pending feedback id but do NOT block here — blocking
                        // would fill the OS pipe buffer (64 KB) if Claude keeps writing,
                        // causing a deadlock. We wait after child.wait() instead.
                        if ev.kind == "text" {
                            if let Some(parsed) =
                                conductor_core::agent::parse_feedback_marker_structured(&ev.summary)
                            {
                                eprintln!(
                                    "[conductor] Agent requesting feedback: {}",
                                    parsed.prompt
                                );
                                let params = conductor_core::agent::FeedbackRequestParams {
                                    feedback_type: parsed.feedback_type,
                                    options: parsed.options,
                                    timeout_secs: parsed.timeout_secs,
                                };
                                match mgr.request_feedback(run_id, &parsed.prompt, Some(&params)) {
                                    Ok(fb) => {
                                        eprintln!(
                                            "[conductor] Feedback requested (id: {}), will wait after turn completes",
                                            fb.id
                                        );
                                        pending_feedback_id = Some(fb.id);
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "[conductor] Warning: could not create feedback request: {e}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── wait for child to exit ────────────────────────────────────────────
        let status = child.wait();

        let end_time = chrono::Utc::now().to_rfc3339();

        // Close the last open event span
        if let Some(ref prev_id) = last_event_id {
            let _ = mgr.update_event_ended_at(prev_id, &end_time);
        }

        // ── accumulate stats ──────────────────────────────────────────────────
        acc_cost_usd += cost_usd.unwrap_or(0.0);
        acc_input_tokens += input_tokens.unwrap_or(0);
        acc_output_tokens += output_tokens.unwrap_or(0);
        acc_cache_read_tokens += cache_read_input_tokens.unwrap_or(0);
        acc_cache_creation_tokens += cache_creation_input_tokens.unwrap_or(0);
        acc_num_turns += num_turns.unwrap_or(0);
        acc_duration_ms += duration_ms.unwrap_or(0);

        // ── deliver feedback and loop, or fall through to completion ──────────
        // Now that stdout is at EOF, it is safe to block waiting for the human.
        if let Some(ref feedback_id) = pending_feedback_id {
            if !is_error {
                eprintln!("[conductor] Waiting for human feedback (id: {feedback_id})...");
                if let Some(response) = wait_for_feedback_response(&mgr, feedback_id) {
                    eprintln!("[conductor] Feedback received, spawning resume turn...");
                    let fb_now = chrono::Utc::now().to_rfc3339();
                    let _ = mgr.create_event(
                        run_id,
                        "feedback",
                        &format!("Human feedback: {response}"),
                        &fb_now,
                        None,
                    );
                    if session_id_parsed.is_some() {
                        feedback_response_for_resume = Some(response);
                        had_feedback_resume = true;
                        continue; // spawn the resume turn
                    } else {
                        let msg =
                            "Cannot deliver feedback: no session_id captured from Claude output";
                        eprintln!("[conductor] Error: {msg}");
                        mgr.update_run_failed(run_id, msg)?;
                        return Ok(());
                    }
                }
                // Feedback dismissed or timed out — fall through to normal completion
                eprintln!("[conductor] Feedback dismissed or timed out, completing run");
            }
        }

        // ── final completion handling ─────────────────────────────────────────
        // When we had feedback resume turns, the eager update used per-turn values
        // for the last turn only. Override with accumulated totals for accuracy.
        let final_cost = if acc_cost_usd > 0.0 {
            Some(acc_cost_usd)
        } else {
            cost_usd
        };
        let final_num_turns = if acc_num_turns > 0 {
            Some(acc_num_turns)
        } else {
            num_turns
        };
        let final_duration_ms = if acc_duration_ms > 0 {
            Some(acc_duration_ms)
        } else {
            duration_ms
        };
        let final_input_tokens = if acc_input_tokens > 0 {
            Some(acc_input_tokens)
        } else {
            input_tokens
        };
        let final_output_tokens = if acc_output_tokens > 0 {
            Some(acc_output_tokens)
        } else {
            output_tokens
        };
        let final_cache_read = if acc_cache_read_tokens > 0 {
            Some(acc_cache_read_tokens)
        } else {
            cache_read_input_tokens
        };
        let final_cache_creation = if acc_cache_creation_tokens > 0 {
            Some(acc_cache_creation_tokens)
        } else {
            cache_creation_input_tokens
        };

        match status {
            Ok(s) if s.success() && !is_error => {
                if !db_updated_eagerly || had_feedback_resume {
                    mgr.update_run_completed(
                        run_id,
                        session_id_parsed.as_deref(),
                        result_text.as_deref(),
                        final_cost,
                        final_num_turns,
                        final_duration_ms,
                        final_input_tokens,
                        final_output_tokens,
                        final_cache_read,
                        final_cache_creation,
                    )?;
                }
                // Mark all plan steps done now that the run succeeded
                if let Err(e) = mgr.mark_plan_done(run_id) {
                    eprintln!("[conductor] Warning: could not mark plan done: {e}");
                }
                eprintln!("[conductor] Agent completed successfully");
                {
                    let fmt_k = |n: i64| -> String {
                        if n >= 1000 {
                            format!("{:.1}k", n as f64 / 1000.0)
                        } else {
                            n.to_string()
                        }
                    };
                    let in_str = fmt_k(final_input_tokens.unwrap_or(0));
                    let out_str = final_output_tokens.unwrap_or(0).to_string();
                    let cache_r_str = fmt_k(final_cache_read.unwrap_or(0));
                    let cache_w_str = fmt_k(final_cache_creation.unwrap_or(0));
                    let turns = final_num_turns.unwrap_or(0);
                    let dur = final_duration_ms
                        .map(|ms| ms as f64 / 1000.0)
                        .unwrap_or(0.0);
                    eprintln!(
                        "[conductor] in: {in_str}  out: {out_str}  cache_r: {cache_r_str}  cache_w: {cache_w_str}  turns: {turns}  duration: {dur:.1}s"
                    );
                }
            }
            Ok(_) if is_error => {
                let error_msg = result_text
                    .as_deref()
                    .unwrap_or(conductor_core::agent::DEFAULT_AGENT_ERROR_MSG);
                if !db_updated_eagerly {
                    mgr.update_run_failed_with_session(
                        run_id,
                        error_msg,
                        session_id_parsed.as_deref(),
                    )?;
                }
                eprintln!("[conductor] Agent failed: {}", error_msg);
            }
            Ok(s) => {
                // Non-zero exit without is_error — override any eager update
                let error_msg = format!("Claude exited with status: {}", s);
                mgr.update_run_failed_with_session(
                    run_id,
                    &error_msg,
                    session_id_parsed.as_deref(),
                )?;
                eprintln!("[conductor] Agent failed: {}", error_msg);
            }
            Err(e) => {
                let error_msg = format!("Error waiting for claude: {e}");
                mgr.update_run_failed_with_session(
                    run_id,
                    &error_msg,
                    session_id_parsed.as_deref(),
                )?;
                eprintln!("[conductor] {}", error_msg);
            }
        }

        break;
    } // end multi-turn loop

    eprintln!(
        "[conductor] Agent log saved to {}",
        log_path.to_string_lossy()
    );

    Ok(())
}

/// Poll the database for a feedback response. Returns the response text if responded,
/// or None if dismissed. Polls every 2 seconds for up to 1 hour.
fn wait_for_feedback_response(mgr: &AgentManager, feedback_id: &str) -> Option<String> {
    let max_polls = 1800; // 2s * 1800 = 1 hour
    for _ in 0..max_polls {
        std::thread::sleep(std::time::Duration::from_secs(2));

        match mgr.get_feedback(feedback_id) {
            Ok(Some(fb)) => match fb.status {
                conductor_core::agent::FeedbackStatus::Responded => return fb.response,
                conductor_core::agent::FeedbackStatus::Dismissed => return None,
                _ => continue, // still pending
            },
            Ok(None) => return None, // feedback request deleted
            Err(e) => {
                eprintln!("[conductor] Warning: error polling feedback: {e}");
                continue;
            }
        }
    }

    eprintln!("[conductor] Feedback request timed out after 1 hour");
    None
}

/// Run the orchestration: generate a plan, then spawn child agents for each step.
/// Note: feedback detection (`[NEEDS_FEEDBACK]`) is intentionally omitted here.
/// The orchestrator manages a plan and spawns child `run_agent` invocations;
/// each child has its own event loop that handles feedback markers.
fn run_orchestrate(
    conn: &rusqlite::Connection,
    config: &conductor_core::config::Config,
    run_id: &str,
    worktree_path: &str,
    model: Option<&str>,
    fail_fast: bool,
    child_timeout_secs: u64,
) -> Result<()> {
    let mgr = AgentManager::new(conn);

    // Verify the run exists
    let run = mgr.get_run(run_id)?;
    let run = match run {
        Some(r) => r,
        None => anyhow::bail!("agent run not found: {run_id}"),
    };

    // Build effective prompt with startup context
    let effective_prompt = if config.general.inject_startup_context {
        let context = build_startup_context(
            conn,
            config,
            run.worktree_id.as_deref(),
            run_id,
            worktree_path,
        );
        eprintln!("[orchestrator] Injecting session context into prompt");
        format!("{context}\n\n---\n\n{}", run.prompt)
    } else {
        run.prompt.clone()
    };

    // Phase 1: Generate plan
    eprintln!("[orchestrator] Generating plan...");
    let steps = generate_plan(worktree_path, &effective_prompt, config);
    match steps {
        Some(ref plan_steps) => {
            eprintln!("[orchestrator] Plan ({} steps):", plan_steps.len());
            for (i, step) in plan_steps.iter().enumerate() {
                eprintln!("  {}. {}", i + 1, step.description);
            }
            if let Err(e) = mgr.update_run_plan(run_id, plan_steps) {
                eprintln!("[orchestrator] Warning: could not save plan to DB: {e}");
            }
        }
        None => {
            let msg = "Plan generation returned no steps — cannot orchestrate";
            eprintln!("[orchestrator] {msg}");
            mgr.update_run_failed(run_id, msg)?;
            return Ok(());
        }
    }

    // Emit orchestration start event
    {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = mgr.create_event(
            run_id,
            "system",
            &format!("Orchestrating {} plan steps", steps.as_ref().unwrap().len()),
            &now,
            None,
        );
    }

    // Phase 2: Orchestrate child runs
    eprintln!("[orchestrator] Starting child orchestration...");

    let orch_config = OrchestratorConfig {
        fail_fast,
        child_timeout: std::time::Duration::from_secs(child_timeout_secs),
        ..Default::default()
    };

    match orchestrator::orchestrate_run(conn, config, run_id, worktree_path, model, &orch_config) {
        Ok(result) => {
            if result.all_succeeded {
                eprintln!("[orchestrator] All steps completed successfully");
            } else {
                eprintln!("[orchestrator] Orchestration completed with failures");
            }
        }
        Err(e) => {
            eprintln!("[orchestrator] Orchestration failed: {e}");
            let _ = mgr.update_run_failed(run_id, &format!("Orchestration error: {e}"));
        }
    }

    Ok(())
}

/// Print a human-readable summary of a stream-json event to stderr (visible in tmux).
fn print_event_summary(event: &serde_json::Value) {
    let msg_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        // Assistant text message
        "assistant" => {
            if let Some(content) = event.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                eprintln!("{text}");
                            }
                        }
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                            let tool = block.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            eprintln!("[tool: {tool}]");
                        }
                    }
                }
            }
            // Also check top-level content (varies by event shape)
            if let Some(content) = event.get("content") {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                eprintln!("{text}");
                            }
                        }
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                            let tool = block.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            eprintln!("[tool: {tool}]");
                        }
                    }
                }
            }
        }
        // Result message
        "result" => {
            if let Some(text) = event.get("result").and_then(|v| v.as_str()) {
                let truncated = if text.len() > 200 { &text[..200] } else { text };
                eprintln!("[result] {truncated}");
            }
        }
        _ => {}
    }
}
