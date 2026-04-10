use anyhow::{Context, Result};
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::feature::FeatureManager;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::workflow::{WorkflowExecConfig, WorkflowManager};
use conductor_core::workflow_config;
use conductor_core::worktree::WorktreeManager;

use crate::commands::WorkflowCommands;
use crate::helpers::{report_workflow_result, truncate_str};

pub fn handle_workflow(
    command: WorkflowCommands,
    conn: &Connection,
    config: &Config,
) -> Result<()> {
    // Finalize and resume stuck workflow runs before handling any workflow command.
    {
        let wf_mgr = WorkflowManager::new(conn);
        match wf_mgr.reap_finalization_stuck_workflow_runs(60) {
            Ok(n) if n > 0 => eprintln!("Info: reaper finalized {n} stuck workflow run(s)"),
            Ok(_) => {}
            Err(e) => eprintln!("Warning: reap_finalization_stuck_workflow_runs failed: {e}"),
        }
        match wf_mgr.detect_stuck_workflow_run_ids(60) {
            Ok(ids) if !ids.is_empty() => {
                let conductor_bin_dir = conductor_core::workflow::resolve_conductor_bin_dir();
                for run_id in ids {
                    let config_clone = config.clone();
                    let bin_dir = conductor_bin_dir.clone();
                    std::thread::spawn(move || {
                        let params = conductor_core::workflow::WorkflowResumeStandalone {
                            config: config_clone,
                            workflow_run_id: run_id.clone(),
                            model: None,
                            from_step: None,
                            restart: false,
                            db_path: None,
                            conductor_bin_dir: bin_dir,
                        };
                        if let Err(e) =
                            conductor_core::workflow::resume_workflow_standalone(&params)
                        {
                            eprintln!("Warning: auto-resume of stuck run {run_id} failed: {e}");
                        }
                    });
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("Warning: detect_stuck_workflow_run_ids failed: {e}"),
        }
    }

    match command {
        WorkflowCommands::Active => {
            let wf_mgr = WorkflowManager::new(conn);
            let runs = wf_mgr.list_active_workflow_runs(&[])?;

            if runs.is_empty() {
                println!("No active workflow runs.");
            } else {
                for run in &runs {
                    let label = run.target_label.as_deref().unwrap_or("-");
                    let since = &run.started_at[..16.min(run.started_at.len())];
                    println!(
                        "  {:<26}  {:<30}  {:<10}  {label} ({since})",
                        &run.id[..26.min(run.id.len())],
                        run.workflow_name,
                        run.status,
                    );
                }
            }
        }
        WorkflowCommands::Runs { repo, worktree } => {
            let repo_mgr = RepoManager::new(conn, config);
            let r = repo_mgr.get_by_slug(&repo)?;

            let agent_mgr = conductor_core::agent::AgentManager::new(conn);
            let runs = if let Some(wt_slug) = worktree {
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt = wt_mgr.get_by_slug(&r.id, &wt_slug)?;
                agent_mgr.list_for_worktree(&wt.id)?
            } else {
                agent_mgr.list_for_repo(&r.id)?
            };

            if runs.is_empty() {
                println!("No workflow runs found.");
            } else {
                println!(
                    "  {:<10}  {:<40}  {:<20}  STARTED AT",
                    "RUN ID", "WORKFLOW", "STATUS"
                );
                for run in &runs {
                    println!(
                        "  {:<10}  {:<40}  {:<20}  {}",
                        &run.id[..8.min(run.id.len())],
                        truncate_str(&run.prompt, 40),
                        run.status,
                        &run.started_at[..16.min(run.started_at.len())],
                    );
                }
            }
        }
        WorkflowCommands::List {
            repo,
            worktree,
            path,
        } => {
            let (wt_path, repo_path) = if let Some(ref dir) = path {
                (dir.clone(), dir.clone())
            } else {
                let repo_mgr = RepoManager::new(conn, config);
                let repo_slug = repo
                    .as_deref()
                    .context("--repo is required when --path is not used")?;
                let r = repo_mgr.get_by_slug(repo_slug)?;
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt_slug = worktree
                    .as_deref()
                    .context("--worktree is required when --path is not used")?;
                let wt = wt_mgr.get_by_slug(&r.id, wt_slug)?;
                (wt.path, r.local_path)
            };

            // Try new .wf files first, fall back to legacy .md
            let (wf_defs, wf_warnings) = WorkflowManager::list_defs(&wt_path, &repo_path)?;
            for w in &wf_warnings {
                eprintln!("warning: Failed to parse {}: {}", w.file, w.message);
            }
            if !wf_defs.is_empty() {
                for def in &wf_defs {
                    let node_count = def.total_nodes();
                    let display = if def.title.is_some() {
                        format!("{} [id: {}]", def.display_name(), def.name)
                    } else {
                        def.name.clone()
                    };
                    println!(
                        "  {:<40} {:<40} [{}, {} nodes]",
                        display, def.description, def.trigger, node_count
                    );
                }
            } else {
                let defs = workflow_config::load_workflow_defs(&wt_path, &repo_path)?;
                if defs.is_empty() {
                    println!(
                        "No workflows found. Create .conductor/workflows/<name>.wf in your repo."
                    );
                } else {
                    for def in &defs {
                        println!(
                            "  {:<20} {:<40} [{}, {} steps]",
                            def.name,
                            def.description,
                            def.trigger,
                            def.steps.len()
                        );
                    }
                }
            }
        }
        WorkflowCommands::Run {
            repo,
            worktree,
            name,
            pr,
            repo_flag,
            workflow_run,
            ticket,
            model,
            dry_run,
            no_fail_fast,
            step_timeout_secs,
            inputs,
            feature,
            background,
            plugin_dirs,
        } => {
            // Parse input key=value pairs (shared by both paths)
            let mut input_map = std::collections::HashMap::new();
            for input_str in &inputs {
                if let Some((key, value)) = input_str.split_once('=') {
                    input_map.insert(key.to_string(), value.to_string());
                } else {
                    anyhow::bail!("Invalid input format: '{}'. Use key=value.", input_str);
                }
            }

            let exec_config = WorkflowExecConfig {
                step_timeout: std::time::Duration::from_secs(step_timeout_secs),
                fail_fast: !no_fail_fast,
                dry_run,
                ..Default::default()
            };

            if dry_run {
                println!("DRY RUN: Actor steps will show intended changes without committing.");
            }

            // Resolve --feature to a feature_id. When --feature is absent and
            // --ticket is provided, auto-detect from the feature_tickets table.
            let feature_id = FeatureManager::new(conn, config).resolve_feature_id_for_run(
                feature.as_deref(),
                repo.as_deref().or(repo_flag.as_deref()),
                ticket.as_deref(),
                worktree.as_deref(),
            )?;

            if let Some(pr_url) = pr {
                // Ephemeral PR run
                let pr_ref = conductor_core::workflow_ephemeral::parse_pr_ref(&pr_url)?;

                println!(
                    "Running workflow '{}' against PR #{} ({})...",
                    name,
                    pr_ref.number,
                    pr_ref.repo_slug()
                );

                match conductor_core::workflow_ephemeral::run_workflow_on_pr(
                    conn,
                    config,
                    &pr_ref,
                    &name,
                    model.as_deref(),
                    exec_config,
                    input_map,
                    dry_run,
                    conductor_core::workflow::resolve_conductor_bin_dir(),
                ) {
                    Ok(result) => report_workflow_result(result),
                    Err(e) => {
                        eprintln!("Workflow execution failed: {e}");
                        std::process::exit(1);
                    }
                }
            } else if let Some(repo_slug) = repo_flag {
                // Repo-targeted workflow run (no worktree)
                let repo_mgr = RepoManager::new(conn, config);
                let r = repo_mgr.get_by_slug(&repo_slug)?;

                let workflow =
                    WorkflowManager::load_def_by_name(&r.local_path, &r.local_path, &name)?;

                if !workflow.targets.contains(&"repo".to_string()) {
                    eprintln!(
                        "Warning: workflow '{}' targets {:?}, not 'repo'. Proceeding anyway.",
                        name, workflow.targets
                    );
                }

                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                let node_count = workflow.total_nodes();
                println!(
                    "Running workflow '{}' ({} nodes) on repo '{}'...",
                    workflow.name, node_count, repo_slug
                );

                run_and_report(&conductor_core::workflow::WorkflowExecInput {
                    conn,
                    config,
                    workflow: &workflow,
                    worktree_id: None,
                    working_dir: &r.local_path,
                    repo_path: &r.local_path,
                    ticket_id: None,
                    repo_id: Some(&r.id),
                    model: model.as_deref(),
                    exec_config: &exec_config,
                    inputs: input_map,
                    depth: 0,
                    parent_workflow_run_id: None,
                    target_label: Some(r.slug.as_str()),
                    default_bot_name: None,
                    feature_id: feature_id.as_deref(),
                    iteration: 0,
                    run_id_notify: None,
                    triggered_by_hook: false,
                    conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                    force: false,
                    extra_plugin_dirs: plugin_dirs.clone(),
                })?;
            } else if let Some(run_id) = workflow_run {
                // Workflow-run targeted run (e.g. postmortem workflows)
                let wf_mgr = WorkflowManager::new(conn);
                let ctx = wf_mgr.resolve_run_context(&run_id, config)?;

                // Auto-inject the workflow_run_id input (user --input flags merge after)
                input_map
                    .entry("workflow_run_id".to_string())
                    .or_insert_with(|| run_id.clone());

                let workflow =
                    WorkflowManager::load_def_by_name(&ctx.working_dir, &ctx.repo_path, &name)?;

                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                let node_count = workflow.total_nodes();
                println!(
                    "Running workflow '{}' ({} nodes) on workflow run {}...",
                    workflow.name, node_count, run_id
                );

                run_and_report(&conductor_core::workflow::WorkflowExecInput {
                    conn,
                    config,
                    workflow: &workflow,
                    worktree_id: ctx.worktree_id.as_deref(),
                    working_dir: &ctx.working_dir,
                    repo_path: &ctx.repo_path,
                    ticket_id: None,
                    repo_id: ctx.repo_id.as_deref(),
                    model: model.as_deref(),
                    exec_config: &exec_config,
                    inputs: input_map,
                    depth: 0,
                    parent_workflow_run_id: None,
                    target_label: Some(run_id.as_str()),
                    default_bot_name: None,
                    feature_id: feature_id.as_deref(),
                    iteration: 0,
                    run_id_notify: None,
                    triggered_by_hook: false,
                    conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                    force: false,
                    extra_plugin_dirs: plugin_dirs.clone(),
                })?;
            } else if let Some(ticket_id) = ticket {
                let syncer = TicketSyncer::new(conn);
                let ticket = syncer.get_by_id(&ticket_id)?;
                let repo_mgr = RepoManager::new(conn, config);
                let repo = repo_mgr.get_by_id(&ticket.repo_id)?;

                let workflow =
                    WorkflowManager::load_def_by_name(&repo.local_path, &repo.local_path, &name)?;

                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                println!(
                    "Running workflow '{}' ({} nodes) on ticket {}...",
                    workflow.name,
                    workflow.total_nodes(),
                    ticket_id
                );

                run_and_report(&conductor_core::workflow::WorkflowExecInput {
                    conn,
                    config,
                    workflow: &workflow,
                    worktree_id: None,
                    working_dir: &repo.local_path,
                    repo_path: &repo.local_path,
                    ticket_id: Some(&ticket_id),
                    repo_id: Some(&ticket.repo_id),
                    model: model.as_deref(),
                    exec_config: &exec_config,
                    inputs: input_map,
                    depth: 0,
                    parent_workflow_run_id: None,
                    target_label: Some(repo.slug.as_str()),
                    default_bot_name: None,
                    feature_id: feature_id.as_deref(),
                    iteration: 0,
                    run_id_notify: None,
                    triggered_by_hook: false,
                    conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                    force: false,
                    extra_plugin_dirs: plugin_dirs.clone(),
                })?;
            } else {
                // Normal registered repo/worktree run
                let repo_slug = repo.expect("repo is required when --pr is not used");
                let worktree_slug = worktree.expect("worktree is required when --pr is not used");

                let repo_mgr = RepoManager::new(conn, config);
                let r = repo_mgr.get_by_slug(&repo_slug)?;
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt = wt_mgr.get_by_slug(&r.id, &worktree_slug)?;

                let workflow = WorkflowManager::load_def_by_name(&wt.path, &r.local_path, &name)?;

                // Validate required inputs and apply defaults
                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                let node_count = workflow.total_nodes();
                let wt_label = format!("{repo_slug}/{worktree_slug}");

                #[cfg(unix)]
                if background {
                    let params = conductor_core::workflow::WorkflowExecStandalone {
                        config: config.clone(),
                        workflow,
                        worktree_id: Some(wt.id.clone()),
                        working_dir: wt.path.clone(),
                        repo_path: r.local_path.clone(),
                        ticket_id: wt.ticket_id.clone(),
                        repo_id: None,
                        model,
                        exec_config,
                        inputs: input_map,
                        target_label: Some(wt_label),
                        feature_id: feature_id.map(|s| s.to_string()),
                        run_id_notify: None,
                        triggered_by_hook: false,
                        conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                        force: false,
                        extra_plugin_dirs: plugin_dirs,
                        db_path: None,
                    };
                    let run_id = crate::background::fork_and_run_workflow(params)?;
                    println!("{}", run_id);
                    return Ok(());
                }
                #[cfg(not(unix))]
                if background {
                    anyhow::bail!("--background is only supported on Unix systems");
                }

                println!(
                    "Running workflow '{}' ({} nodes) on {}/{}...",
                    workflow.name, node_count, repo_slug, worktree_slug
                );

                run_and_report(&conductor_core::workflow::WorkflowExecInput {
                    conn,
                    config,
                    workflow: &workflow,
                    worktree_id: Some(&wt.id),
                    working_dir: &wt.path,
                    repo_path: &r.local_path,
                    ticket_id: wt.ticket_id.as_deref(),
                    repo_id: None,
                    model: model.as_deref(),
                    exec_config: &exec_config,
                    inputs: input_map,
                    depth: 0,
                    parent_workflow_run_id: None,
                    target_label: Some(&wt_label),
                    default_bot_name: None,
                    feature_id: feature_id.as_deref(),
                    iteration: 0,
                    run_id_notify: None,
                    triggered_by_hook: false,
                    conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                    force: false,
                    extra_plugin_dirs: plugin_dirs,
                })?;
            }
        }
        WorkflowCommands::RunShow { id } => {
            let wf_mgr = WorkflowManager::new(conn);
            match wf_mgr.get_workflow_run(&id)? {
                Some(run) => {
                    println!("Workflow Run: {}", run.id);
                    println!("  Name:    {}", run.workflow_name);
                    println!("  Status:  {}", run.status);
                    println!("  Trigger: {}", run.trigger);
                    println!("  Dry run: {}", run.dry_run);
                    println!("  Started: {}", run.started_at);
                    if let Some(ref ended) = run.ended_at {
                        println!("  Ended:   {ended}");
                    }
                    if !run.inputs.is_empty() {
                        println!("  Inputs:");
                        let mut sorted_inputs: Vec<_> = run.inputs.iter().collect();
                        sorted_inputs.sort_by_key(|(k, _)| k.as_str());
                        for (k, v) in sorted_inputs {
                            println!("    {k}: {v}");
                        }
                    }
                    if let Some(ref snapshot) = run.definition_snapshot {
                        println!("  Definition snapshot:");
                        for line in snapshot.lines() {
                            println!("    {line}");
                        }
                    }
                    if let Some(ref summary) = run.result_summary {
                        println!("\n{summary}");
                    }

                    let steps = wf_mgr.get_workflow_steps(&run.id)?;
                    if !steps.is_empty() {
                        println!("\nSteps:");
                        for step in &steps {
                            let marker = step.status.short_label();
                            let commit_flag = if step.can_commit { " [commit]" } else { "" };
                            let iter_label = if step.iteration > 0 {
                                format!(" iter={}", step.iteration)
                            } else {
                                String::new()
                            };
                            println!(
                                "  [{marker}] {} ({}{}){iter_label}",
                                step.step_name, step.role, commit_flag
                            );
                            if let Some(ref started) = step.started_at {
                                print!("        started: {started}");
                                if let Some(ref ended) = step.ended_at {
                                    print!("  ended: {ended}");
                                }
                                println!();
                            }
                            if let Some(ref gate_type) = step.gate_type {
                                print!("        gate: {gate_type}");
                                if let Some(ref approved_at) = step.gate_approved_at {
                                    print!(" (approved {approved_at})");
                                }
                                println!();
                            }
                            if step.retry_count > 0 {
                                println!("        retries: {}", step.retry_count);
                            }
                            if let Some(ref expr) = step.condition_expr {
                                let met = step
                                    .condition_met
                                    .map(|b| if b { "true" } else { "false" })
                                    .unwrap_or("(unevaluated)");
                                println!("        condition: {expr} => {met}");
                            }
                            if let Some(ref markers) = step.markers_out {
                                println!("        markers: {markers}");
                            }
                            if let Some(ref ctx) = step.context_out {
                                if !ctx.is_empty() {
                                    println!("        context: {ctx}");
                                }
                            }
                            if let Some(ref result) = step.result_text {
                                if !result.is_empty() {
                                    println!("        result: {result}");
                                }
                            }
                            if let Some(ref child) = step.child_run_id {
                                println!("        child run: {child}");
                            }
                        }
                    }
                }
                None => {
                    println!("Workflow run not found: {id}");
                }
            }
        }
        WorkflowCommands::Validate {
            repo,
            worktree,
            name,
            all,
            path,
        } => {
            if !all && name.is_none() {
                anyhow::bail!(
                    "either <NAME> or --all must be provided. \
                     Use --all to validate every workflow."
                );
            }

            let (wt_path, repo_path) = if let Some(ref dir) = path {
                (dir.clone(), dir.clone())
            } else if repo.is_some() && worktree.is_some() {
                let repo_mgr = RepoManager::new(conn, config);
                // SAFETY: guarded by `repo.is_some()` above
                let r = repo_mgr.get_by_slug(repo.as_deref().unwrap())?;
                let wt_mgr = WorktreeManager::new(conn, config);
                // SAFETY: guarded by `worktree.is_some()` above
                let wt = wt_mgr.get_by_slug(&r.id, worktree.as_deref().unwrap())?;
                (wt.path, r.local_path)
            } else if repo.is_some() || worktree.is_some() {
                anyhow::bail!(
                    "--repo and --worktree must be supplied together; \
                     got only {}. Pass both or omit both to auto-detect from CWD.",
                    if repo.is_some() {
                        "--repo"
                    } else {
                        "--worktree"
                    }
                );
            } else {
                let cwd = std::env::current_dir()?;
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt = wt_mgr.find_by_cwd(&cwd)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Could not detect repo/worktree from current directory. \
                         Run from inside a conductor-managed worktree, or pass <repo> <worktree> explicitly."
                    )
                })?;
                let repo_mgr = RepoManager::new(conn, config);
                let r = repo_mgr.get_by_id(&wt.repo_id)?;
                (wt.path, r.local_path)
            };

            // Collect workflows to validate.
            let workflows: Vec<conductor_core::workflow::WorkflowDef>;
            let mut parse_errors: Vec<String> = Vec::new();

            if all {
                let (defs, warnings) = WorkflowManager::list_defs(&wt_path, &repo_path)?;
                for w in &warnings {
                    parse_errors.push(format!("{}: {}", w.file, w.message));
                }
                workflows = defs;
                if workflows.is_empty() && parse_errors.is_empty() {
                    println!("No workflow files found.");
                    return Ok(());
                }
            } else {
                // SAFETY: the `!all && name.is_none()` guard above ensures `name` is `Some` here.
                let wf_name = name
                    .as_deref()
                    .expect("name must be Some when --all is not set");
                workflows = vec![WorkflowManager::load_def_by_name(
                    &wt_path, &repo_path, wf_name,
                )?];
            };

            let known_bots: std::collections::HashSet<String> =
                config.github.apps.keys().cloned().collect();
            let wt_ref = wt_path.clone();
            let repo_ref = repo_path.clone();
            let loader = |name: &str| {
                conductor_core::workflow::load_workflow_by_name(&wt_ref, &repo_ref, name)
                    .map_err(|e| e.to_string())
            };
            let result = conductor_core::workflow::validate_workflows_batch(
                &workflows,
                &parse_errors,
                &wt_path,
                &repo_path,
                &known_bots,
                &loader,
            );

            // Report parse failures first.
            for err in &result.parse_errors {
                println!("FAIL  {err}");
            }

            // Report per-workflow results.
            for entry in &result.entries {
                if entry.errors.is_empty() {
                    println!("PASS  {}", entry.name);
                } else {
                    println!("FAIL  {}", entry.name);
                    for e in &entry.errors {
                        println!("      \u{2717} {e}");
                    }
                }
                for w in &entry.warnings {
                    println!("      ~ warning: {}", w.message);
                }
            }

            // Summary when validating multiple workflows.
            if all {
                let passed = result.total() - result.failed_count();
                println!("\n{passed}/{} workflow(s) passed.", result.total());
            }

            if result.failed_count() > 0 {
                std::process::exit(1);
            }
        }
        WorkflowCommands::Resume {
            id,
            from_step,
            model,
            restart,
        } => {
            let resume_input = conductor_core::workflow::WorkflowResumeInput {
                conn,
                config,
                workflow_run_id: &id,
                model: model.as_deref(),
                from_step: from_step.as_deref(),
                restart,
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            };

            if restart {
                println!("Restarting workflow run {id} from the beginning...");
            } else if let Some(ref step) = from_step {
                println!("Resuming workflow run {id} from step '{step}'...");
            } else {
                println!("Resuming workflow run {id}...");
            }

            match conductor_core::workflow::resume_workflow(&resume_input) {
                Ok(result) => {
                    println!(
                        "\nTotal: ${:.4}, {} turns, {:.1}s",
                        result.total_cost,
                        result.total_turns,
                        result.total_duration_ms as f64 / 1000.0
                    );
                    if result.all_succeeded {
                        println!("Workflow resumed and completed successfully.");
                    } else {
                        eprintln!("Workflow resumed but finished with failures.");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("Workflow resume failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        WorkflowCommands::Cancel { id } => {
            let wf_mgr = WorkflowManager::new(conn);
            match wf_mgr.get_workflow_run(&id)? {
                Some(run) => {
                    if matches!(
                        run.status,
                        conductor_core::workflow::WorkflowRunStatus::Completed
                            | conductor_core::workflow::WorkflowRunStatus::Failed
                            | conductor_core::workflow::WorkflowRunStatus::Cancelled
                    ) {
                        println!(
                            "Workflow run {} is already in terminal state: {}",
                            id, run.status
                        );
                    } else {
                        wf_mgr.update_workflow_status(
                            &id,
                            conductor_core::workflow::WorkflowRunStatus::Cancelled,
                            Some("Cancelled by user"),
                            None,
                        )?;
                        println!("Workflow run {} cancelled.", id);
                    }
                }
                None => {
                    println!("Workflow run not found: {id}");
                }
            }
        }
        WorkflowCommands::GateApprove { run_id } => {
            with_waiting_gate(conn, &run_id, |wf_mgr, step, user| {
                wf_mgr.approve_gate(&step.id, user, None, None)?;
                println!("Gate '{}' approved by {user}.", step.step_name);
                Ok(())
            })?;
        }
        WorkflowCommands::GateReject { run_id } => {
            with_waiting_gate(conn, &run_id, |wf_mgr, step, user| {
                wf_mgr.reject_gate(&step.id, user, None)?;
                let reject_msg = format!("Gate '{}' rejected by {user}", step.step_name);
                wf_mgr.update_workflow_status(
                    &run_id,
                    conductor_core::workflow::WorkflowRunStatus::Failed,
                    Some(&reject_msg),
                    Some(&reject_msg),
                )?;
                println!("Gate '{}' rejected by {user}.", step.step_name);
                Ok(())
            })?;
        }
        WorkflowCommands::GateFeedback { run_id, feedback } => {
            with_waiting_gate(conn, &run_id, |wf_mgr, step, user| {
                wf_mgr.approve_gate(&step.id, user, Some(&feedback), None)?;
                println!(
                    "Gate '{}' approved with feedback by {user}.",
                    step.step_name
                );
                Ok(())
            })?;
        }
        WorkflowCommands::Purge {
            repo,
            status,
            dry_run,
        } => {
            const ALLOWED: &[&str] = &["completed", "failed", "cancelled"];
            let status_val = status.as_deref().unwrap_or("all");
            let statuses: Vec<&str> = if status_val == "all" {
                ALLOWED.to_vec()
            } else if ALLOWED.contains(&status_val) {
                vec![status_val]
            } else {
                anyhow::bail!(
                    "Unknown status '{status_val}'. Allowed values: completed, failed, cancelled, all"
                );
            };

            let repo_id: Option<String> = if let Some(slug) = &repo {
                let repo_mgr = RepoManager::new(conn, config);
                let r = repo_mgr.get_by_slug(slug)?;
                Some(r.id)
            } else {
                None
            };

            let wf_mgr = WorkflowManager::new(conn);
            if dry_run {
                let count = wf_mgr.purge_count(repo_id.as_deref(), &statuses)?;
                println!("Would purge {count} workflow run(s) (dry run).");
            } else {
                let count = wf_mgr.purge(repo_id.as_deref(), &statuses)?;
                println!("Purged {count} workflow run(s).");
            }
        }
        WorkflowCommands::TemplateList => {
            use conductor_core::workflow_template::list_embedded_templates;
            let templates = list_embedded_templates();
            if templates.is_empty() {
                println!("No workflow templates available.");
            } else {
                println!(
                    "{:<20} {:<10} {:<15} DESCRIPTION",
                    "NAME", "VERSION", "TARGETS"
                );
                for t in &templates {
                    let targets = if t.metadata.target_types.is_empty() {
                        "any".to_string()
                    } else {
                        t.metadata.target_types.join(", ")
                    };
                    println!(
                        "{:<20} {:<10} {:<15} {}",
                        t.metadata.name, t.metadata.version, targets, t.metadata.description
                    );
                }
            }
        }
        WorkflowCommands::TemplateShow { name } => {
            use conductor_core::workflow_template::get_embedded_template;
            match get_embedded_template(&name) {
                Some(t) => {
                    println!("Name:        {}", t.metadata.name);
                    println!("Version:     {}", t.metadata.version);
                    println!("Description: {}", t.metadata.description);
                    if !t.metadata.target_types.is_empty() {
                        println!("Targets:     {}", t.metadata.target_types.join(", "));
                    }
                    if !t.metadata.hints.is_empty() {
                        println!("\nHints:");
                        for h in &t.metadata.hints {
                            println!("  - {h}");
                        }
                    }
                    println!("\n--- Template Body ---\n");
                    println!("{}", t.body);
                }
                None => {
                    eprintln!("Template '{name}' not found. Run `conductor workflow template-list` to see available templates.");
                    std::process::exit(1);
                }
            }
        }
        WorkflowCommands::FromTemplate {
            template,
            repo,
            worktree,
        } => {
            use conductor_core::workflow_template::{
                build_instantiation_prompt, collect_existing_workflow_names, get_embedded_template,
            };

            let tmpl = get_embedded_template(&template).ok_or_else(|| {
                anyhow::anyhow!("Template '{template}' not found. Run `conductor workflow template-list` to see available templates.")
            })?;

            let repo_mgr = RepoManager::new(conn, config);
            let r = repo_mgr.get_by_slug(&repo)?;

            let (working_dir, _worktree_id) = if let Some(ref wt_slug) = worktree {
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt = wt_mgr.get_by_slug_or_branch(&r.id, wt_slug)?;
                (wt.path, Some(wt.id))
            } else {
                (r.local_path.clone(), None)
            };

            let existing_names = collect_existing_workflow_names(&working_dir, &r.local_path);

            let prompt_result = build_instantiation_prompt(&tmpl, &working_dir, &existing_names);

            println!(
                "Template: {} v{}",
                tmpl.metadata.name, tmpl.metadata.version
            );
            println!("Repo:     {repo}");
            if let Some(ref wt) = worktree {
                println!("Worktree: {wt}");
            }
            println!(
                "Output:   .conductor/workflows/{}",
                prompt_result.suggested_filename
            );
            println!(
                "\nAgent prompt has been prepared ({} chars).",
                prompt_result.prompt.len()
            );
            println!(
                "To instantiate, run this workflow with an agent that can write files in the repo."
            );
            println!("\n--- Prompt Preview (first 500 chars) ---\n");
            let preview: String = prompt_result.prompt.chars().take(500).collect();
            println!("{preview}…");
        }
        WorkflowCommands::UpgradeFromTemplate {
            template,
            repo,
            worktree,
        } => {
            use conductor_core::workflow_template::{
                build_upgrade_prompt, extract_template_version, get_embedded_template,
                template_slug,
            };

            let tmpl = get_embedded_template(&template)
                .ok_or_else(|| anyhow::anyhow!("Template '{template}' not found."))?;

            let repo_mgr = RepoManager::new(conn, config);
            let r = repo_mgr.get_by_slug(&repo)?;

            let working_dir = if let Some(ref wt_slug) = worktree {
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt = wt_mgr.get_by_slug_or_branch(&r.id, wt_slug)?;
                wt.path
            } else {
                r.local_path.clone()
            };

            let suggested_name = template_slug(&tmpl.metadata.name);
            let wf_path = std::path::Path::new(&working_dir)
                .join(".conductor")
                .join("workflows")
                .join(format!("{suggested_name}.wf"));

            if !wf_path.exists() {
                anyhow::bail!(
                    "No existing workflow found at {}. Use `from-template` to create one first.",
                    wf_path.display()
                );
            }

            let current_content =
                std::fs::read_to_string(&wf_path).context("Failed to read existing workflow")?;

            let current_version =
                extract_template_version(&current_content).map(|(_, v)| v.to_string());

            if let Some(ref cv) = current_version {
                if cv == &tmpl.metadata.version {
                    println!(
                        "Workflow is already at template version {}. No upgrade needed.",
                        tmpl.metadata.version
                    );
                    return Ok(());
                }
                println!("Upgrading from v{cv} to v{}", tmpl.metadata.version);
            } else {
                println!(
                    "No version comment found. Upgrading to v{}",
                    tmpl.metadata.version
                );
            }

            let prompt_result = build_upgrade_prompt(
                &tmpl,
                &current_content,
                current_version.as_deref(),
                &working_dir,
            );

            println!(
                "Agent prompt prepared ({} chars).",
                prompt_result.prompt.len()
            );
            println!("\n--- Prompt Preview (first 500 chars) ---\n");
            let preview: String = prompt_result.prompt.chars().take(500).collect();
            println!("{preview}…");
        }
    }
    Ok(())
}

/// Execute a workflow and report the result.
fn run_and_report(input: &conductor_core::workflow::WorkflowExecInput) -> Result<()> {
    let result = conductor_core::workflow::execute_workflow(input)
        .map_err(|e| anyhow::anyhow!("Workflow execution failed: {e}"))?;
    report_workflow_result(result);
    Ok(())
}

/// Shared gate operation: find the waiting gate, resolve the current user,
/// then call the provided action closure.
fn with_waiting_gate(
    conn: &Connection,
    run_id: &str,
    action: impl FnOnce(
        &WorkflowManager,
        &conductor_core::workflow::WorkflowRunStep,
        &str,
    ) -> Result<()>,
) -> Result<()> {
    let wf_mgr = WorkflowManager::new(conn);
    match wf_mgr.find_waiting_gate(run_id)? {
        Some(step) => {
            let user = std::env::var("USER").unwrap_or_else(|_| "cli".to_string());
            action(&wf_mgr, &step, &user)
        }
        None => {
            println!("No waiting gate found for workflow run: {run_id}");
            Ok(())
        }
    }
}
