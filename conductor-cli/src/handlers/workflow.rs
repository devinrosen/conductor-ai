use anyhow::{Context, Result};
use rusqlite::Connection;

use conductor_core::config::Config;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::TicketSyncer;
use conductor_core::workflow::{is_workflow_call_step, WorkflowExecConfig};
use conductor_core::workflow_config;
use conductor_core::worktree::WorktreeManager;

/// Registered so stale-process checks cover foreground runs (the reaper runs here too).
fn register_foreground_run() -> Option<conductor_core::process_registry::ProcessGuard> {
    conductor_core::process_registry::register_or_warn(
        conductor_core::process_registry::ProcessKind::Workflow,
    )
}

/// Where a standalone workflow run executes.
#[derive(Clone)]
struct ExecTarget {
    worktree_id: Option<String>,
    working_dir: String,
    repo_path: String,
    ticket_id: Option<String>,
    repo_id: Option<String>,
    target_label: Option<String>,
}

impl ExecTarget {
    fn for_worktree(
        wt: &conductor_core::worktree::Worktree,
        r: &conductor_core::repo::Repo,
    ) -> Self {
        Self {
            worktree_id: Some(wt.id.clone()),
            working_dir: wt.path.clone(),
            repo_path: r.local_path.clone(),
            ticket_id: wt.ticket_id.clone(),
            repo_id: Some(r.id.clone()),
            target_label: Some(format!("{}/{}", r.slug, wt.slug)),
        }
    }

    fn for_repo(r: &conductor_core::repo::Repo) -> Self {
        Self {
            worktree_id: None,
            working_dir: r.local_path.clone(),
            repo_path: r.local_path.clone(),
            ticket_id: None,
            repo_id: Some(r.id.clone()),
            target_label: Some(r.slug.clone()),
        }
    }

    /// A repo-level target: a registered repo with no worktree.
    fn is_repo_level(&self) -> bool {
        self.worktree_id.is_none() && self.repo_id.is_some()
    }
}

/// Resolve a directory to the registered worktree containing it, else the
/// registered repo whose checkout contains it. Returns the workflow lookup
/// paths `(wt_path, repo_path)` and the target. `None` when `dir` is in neither.
fn resolve_dir_target(
    conn: Option<&Connection>,
    config: &Config,
    dir: &std::path::Path,
) -> anyhow::Result<Option<(String, String, ExecTarget)>> {
    let Some(conn) = conn else {
        return Ok(None);
    };
    let repo_mgr = RepoManager::new(conn, config);
    if let Some(wt) = WorktreeManager::new(conn, config).find_by_cwd(dir)? {
        let r = repo_mgr.get_by_id(&wt.repo_id)?;
        let target = ExecTarget::for_worktree(&wt, &r);
        return Ok(Some((wt.path, r.local_path, target)));
    }
    Ok(repo_mgr.find_by_path(dir)?.map(|r| {
        let target = ExecTarget::for_repo(&r);
        (r.local_path.clone(), r.local_path, target)
    }))
}

/// The `(wt_path, repo_path)` script search dirs for `validate --templates`.
/// Inside a registered worktree the worktree is searched first and the main
/// checkout second; otherwise `dir` is used as-is. A failed lookup also falls
/// back to `dir`, with a warning, so the user knows why a variable may be
/// reported as unknown.
fn template_script_dirs(
    conn: Option<&Connection>,
    config: &Config,
    dir: String,
) -> (String, String) {
    let lookup = std::path::absolute(&dir)
        .map_err(anyhow::Error::from)
        .and_then(|abs| resolve_dir_target(conn, config, &abs))
        .map(|found| found.map(|(wt_path, repo_path, _)| (wt_path, repo_path)));
    pick_script_dirs(dir, lookup)
}

fn pick_script_dirs(
    dir: String,
    lookup: anyhow::Result<Option<(String, String)>>,
) -> (String, String) {
    match lookup {
        Ok(Some(dirs)) => dirs,
        Ok(None) => (dir.clone(), dir),
        Err(e) => {
            eprintln!(
                "warning: could not resolve {dir} as a registered worktree, using it as-is: {e}"
            );
            (dir.clone(), dir)
        }
    }
}

/// The per-run options that vary between standalone run call sites.
struct ExecOptions {
    model: Option<String>,
    exec_config: WorkflowExecConfig,
    inputs: std::collections::HashMap<String, String>,
    extra_plugin_dirs: Vec<String>,
    db_path: Option<std::path::PathBuf>,
}

/// Build a top-level (depth 0, no parent) `WorkflowExecStandalone`, filling
/// the defaults every call site shares.
fn exec_params(
    config: &Config,
    workflow: conductor_core::workflow::WorkflowDef,
    target: ExecTarget,
    opts: ExecOptions,
) -> conductor_core::workflow::WorkflowExecStandalone {
    conductor_core::workflow::WorkflowExecStandalone {
        worktree_id: target.worktree_id,
        ticket_id: target.ticket_id,
        repo_id: target.repo_id,
        model: opts.model,
        inputs: opts.inputs,
        target_label: target.target_label,
        conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
        extra_plugin_dirs: opts.extra_plugin_dirs,
        db_path: opts.db_path,
        ..conductor_core::workflow::WorkflowExecStandalone::new(
            config.clone(),
            workflow,
            target.working_dir,
            target.repo_path,
            opts.exec_config,
        )
    }
}

/// Handle `workflow launch-detached`: read the [`LaunchSpec`] from stdin and
/// start it as the detached child. Only returns on a startup failure, which
/// `main` reports on the notify pipe.
#[cfg(unix)]
fn launch_detached(config: &Config, notify_fd: i32) -> Result<()> {
    use crate::background::LaunchSpec;
    use std::io::Read;

    let _process_guard = register_foreground_run();
    let mut payload = Vec::new();
    std::io::stdin()
        .read_to_end(&mut payload)
        .context("failed to read the launch spec from stdin")?;
    let spec: LaunchSpec =
        serde_json::from_slice(&payload).context("invalid launch spec on stdin")?;

    match spec {
        LaunchSpec::Run {
            workflow_name,
            worktree_id,
            working_dir,
            repo_path,
            ticket_id,
            repo_id,
            inputs,
            dry_run,
            target_label,
            db_path,
        } => {
            let workflow = conductor_core::workflow::load_def_by_name(
                &working_dir,
                &repo_path,
                &workflow_name,
            )?;
            let params = exec_params(
                config,
                workflow,
                ExecTarget {
                    worktree_id,
                    working_dir,
                    repo_path,
                    ticket_id,
                    repo_id,
                    target_label,
                },
                ExecOptions {
                    model: None,
                    exec_config: WorkflowExecConfig {
                        dry_run,
                        ..WorkflowExecConfig::default()
                    },
                    inputs,
                    extra_plugin_dirs: vec![],
                    db_path: db_path.clone(),
                },
            );
            crate::background::run_as_background_child(params, db_path, notify_fd)
        }
        LaunchSpec::Resume {
            run_id,
            from_step,
            model,
            db_path,
        } => {
            let params = conductor_core::workflow::WorkflowResumeStandalone {
                config: config.clone(),
                workflow_run_id: run_id,
                model,
                runtime: None,
                from_step,
                restart: false,
                db_path: db_path.clone(),
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                shutdown: None,
                event_sinks: vec![],
            };
            crate::background::resume_as_background_child(params, db_path, notify_fd)
        }
    }
}

use crate::commands::WorkflowCommands;
use crate::helpers::{report_workflow_result, truncate_str};

/// `workflow validate`. `conn` is `None` for a static validate with no usable
/// database (#3470); `--dry-run` always has one.
fn validate(command: WorkflowCommands, conn: Option<&Connection>, config: &Config) -> Result<()> {
    let WorkflowCommands::Validate {
        repo,
        worktree,
        name,
        all,
        path,
        templates,
        dry_run,
        inputs,
        json,
    } = command
    else {
        unreachable!("validate is only called with WorkflowCommands::Validate");
    };
    if !all && name.is_none() && !templates {
        anyhow::bail!(
            "either <NAME>, --all or --templates must be provided. \
             Use --all to validate every workflow."
        );
    }
    // `--templates` alone checks only the embedded templates: no repo
    // or worktree to resolve.
    let templates_only = templates && !all && name.is_none();
    if dry_run && templates_only {
        anyhow::bail!(
            "--dry-run starts a workflow, and templates can't be started. \
             Pass <NAME> or --all."
        );
    }

    // Where a smoke run executes (only set when --dry-run resolves one).
    let mut smoke_target: Option<ExecTarget> = None;

    let (wt_path, repo_path) = if templates_only {
        let dir = path.clone().unwrap_or_else(|| ".".to_string());
        template_script_dirs(conn, config, dir)
    } else if let Some(ref dir) = path {
        let abs = std::path::absolute(dir)?;
        // A static validate must not fail on a database it only reads
        // optionally: warn and use the directory as-is.
        let found = match resolve_dir_target(conn, config, &abs) {
            Ok(found) => found,
            Err(e) if !dry_run => {
                eprintln!(
                    "warning: could not resolve {dir} as a registered worktree, \
                     using it as-is: {e}"
                );
                None
            }
            Err(e) => return Err(e),
        };
        if let Some((wt_path, repo_path, target)) = found {
            smoke_target = Some(target);
            (wt_path, repo_path)
        } else {
            // Unregistered directory: used as-is, no database ids.
            smoke_target = Some(ExecTarget {
                worktree_id: None,
                working_dir: dir.clone(),
                repo_path: dir.clone(),
                ticket_id: None,
                repo_id: None,
                target_label: Some(dir.clone()),
            });
            (dir.clone(), dir.clone())
        }
    } else if repo.is_some() && worktree.is_some() {
        let Some(conn) = conn else {
            anyhow::bail!(
                "looking up <repo> <worktree> needs the conductor database, which was not \
                 found or not readable at {}; pass --path <dir> instead.",
                conductor_core::config::db_path().display()
            );
        };
        let repo_mgr = RepoManager::new(conn, config);
        // SAFETY: guarded by `repo.is_some()` above
        let r = repo_mgr.get_by_slug(repo.as_deref().unwrap())?;
        let wt_mgr = WorktreeManager::new(conn, config);
        // SAFETY: guarded by `worktree.is_some()` above
        let wt = wt_mgr.get_by_slug_or_branch(&r.id, worktree.as_deref().unwrap())?;
        smoke_target = Some(ExecTarget::for_worktree(&wt, &r));
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
        let (wt_path, repo_path, target) = resolve_dir_target(conn, config, &cwd)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not detect a registered repo or worktree from the current \
                     directory. Run from inside a registered repo's checkout or a \
                     conductor worktree, pass <repo> <worktree>, or pass --path <dir>.{}",
                    if conn.is_none() {
                        " (The conductor database was not available.)"
                    } else {
                        ""
                    }
                )
            })?;
        smoke_target = Some(target);
        (wt_path, repo_path)
    };

    // Collect workflows to validate.
    let workflows: Vec<conductor_core::workflow::WorkflowDef>;
    let mut parse_errors: Vec<String> = Vec::new();
    let mut parse_error_pairs: Vec<(String, String)> = Vec::new();

    if templates_only {
        workflows = Vec::new();
    } else if all {
        let (defs, warnings) = conductor_core::workflow::list_defs(&wt_path, &repo_path)?;
        for w in &warnings {
            parse_errors.push(format!("{}: {}", w.file, w.message));
            parse_error_pairs.push((w.file.clone(), w.message.clone()));
        }
        workflows = defs;
        if workflows.is_empty() && parse_errors.is_empty() && !templates {
            if !json {
                println!("No workflow files found.");
            }
            return Ok(());
        }
    } else {
        // SAFETY: the `!all && name.is_none()` guard above ensures `name` is `Some` here.
        let wf_name = name
            .as_deref()
            .expect("name must be Some when --all is not set");
        match conductor_core::workflow::load_def_by_name(&wt_path, &repo_path, wf_name) {
            Ok(def) => workflows = vec![def],
            Err(e) if json => {
                // A named target that can't load is still one report.
                print_smoke_json(&SmokeReport::failed(wf_name, e.to_string()));
                std::process::exit(1);
            }
            Err(e) => return Err(e.into()),
        }
    };

    let known_bots: std::collections::HashSet<String> =
        config.github.apps.keys().cloned().collect();
    let wt_ref = wt_path.clone();
    let repo_ref = repo_path.clone();
    // TODO(#2349-followup): replace with DirectoryWorkflowResolver from runkon-flow once
    // conductor-core re-exports runkon_flow::dsl::WorkflowDef (currently the two types are
    // structurally identical but distinct Rust types — a From impl or re-export is required
    // before validate_workflows_batch can accept the runkon-flow resolver directly).
    let loader = |name: &str| {
        conductor_core::workflow::load_def_by_name(&wt_ref, &repo_ref, name)
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

    let mut results = vec![result];
    if templates {
        results.push(conductor_core::workflow::validate_template_sources(
            &conductor_core::workflow_template::embedded_template_sources(),
            &wt_path,
            &repo_path,
        ));
    }

    let mut total = 0;
    let mut failed = 0;
    for result in &results {
        if json {
            total += result.total();
            failed += result.failed_count();
            continue;
        }
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
        total += result.total();
        failed += result.failed_count();
    }

    // Summary when validating multiple workflows.
    if (all || templates) && !json {
        println!("\n{}/{total} workflow(s) passed.", total - failed);
    }

    // Smoke-start each workflow that passed the static checks.
    let mut smoke_failed = 0;
    if dry_run {
        let mut base_inputs = std::collections::HashMap::new();
        for input_str in &inputs {
            match input_str.split_once('=') {
                Some((k, v)) => {
                    base_inputs.insert(k.to_string(), v.to_string());
                }
                None => {
                    anyhow::bail!("Invalid input format: '{input_str}'. Use key=value.")
                }
            }
        }
        let target = smoke_target
            .clone()
            .expect("--dry-run resolves a target unless templates-only");
        if json {
            for (file, message) in &parse_error_pairs {
                print_smoke_json(&SmokeReport::failed(file, message.clone()));
            }
        } else {
            println!();
        }
        for wf in &workflows {
            let static_failure = static_failure_entry(&results[0], wf);
            let report = if let Some(entry) = static_failure {
                // Failed the static checks: no smoke.
                if !json {
                    continue;
                }
                let mut report = SmokeReport::failed(
                    &wf.name,
                    entry
                        .errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; "),
                );
                report.warnings =
                    entry.warnings.iter().map(|w| w.message.clone()).collect();
                report
            } else if target.is_repo_level() && !wf.targets.iter().any(|t| t == "repo") {
                let reason = format!(
                    "targets {:?}; a smoke run needs a worktree: pass <repo> <worktree>, \
                     or --path <worktree dir>",
                    wf.targets
                );
                let status = if all {
                    SmokeStatus::Skip
                } else {
                    SmokeStatus::Fail
                };
                let mut report = SmokeReport::new(&wf.name, status);
                report.human_lines = vec![format!(
                    "SMOKE {}  {}  {reason}",
                    if all { "SKIP" } else { "FAIL" },
                    wf.name
                )];
                report.error = Some(reason);
                report
            } else {
                smoke_one(
                    conn,
                    config,
                    wf,
                    &target,
                    &base_inputs,
                    /* skip_unfillable */ all,
                )
            };
            if report.status == SmokeStatus::Fail && static_failure.is_none() {
                smoke_failed += 1;
            }
            if json {
                print_smoke_json(&report);
            } else {
                for line in &report.human_lines {
                    println!("{line}");
                }
            }
        }
    }

    if failed > 0 || smoke_failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Static `workflow validate` (no `--dry-run`), dispatched from `main()` before
/// `Conductor::open()` so a branch build never migrates the real database or
/// runs workflow maintenance (#3470). The database is opened read-only, only
/// when it exists and its schema isn't newer than this binary's.
pub fn handle_static_validate(command: WorkflowCommands) -> Result<()> {
    let config = conductor_core::config::load_config()?;
    let db_path = conductor_core::config::db_path();
    let conn = match conductor_core::db::open_database_read_only(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!(
                "warning: not using the conductor database at {}: {e}",
                db_path.display()
            );
            None
        }
    };
    crate::helpers::check_prerequisites();
    validate(command, conn.as_ref(), &config)
}


pub fn handle_workflow(
    command: WorkflowCommands,
    conn: &Connection,
    config: &Config,
) -> Result<()> {
    // Finalize and resume stuck workflow runs before handling any workflow command.
    {
        let conductor_bin_dir = conductor_core::workflow::resolve_conductor_bin_dir();
        conductor_core::workflow::run_workflow_maintenance(conn, config, conductor_bin_dir);
    }

    match command {
        WorkflowCommands::Active => {
            let runs = conductor_core::workflow::list_active_workflow_runs(conn, &[])?;

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
                let wt = wt_mgr.get_by_slug_or_branch(&r.id, &wt_slug)?;
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
                let wt = wt_mgr.get_by_slug_or_branch(&r.id, wt_slug)?;
                (wt.path, r.local_path)
            };

            // Try new .wf files first, fall back to legacy .md
            let (wf_defs, wf_warnings) = conductor_core::workflow::list_defs(&wt_path, &repo_path)?;
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
            background,
            background_notify_fd,
            plugin_dirs,
        } => {
            let _process_guard = register_foreground_run();
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
                    config,
                    &pr_ref,
                    &name,
                    model.as_deref(),
                    None,
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

                let workflow = conductor_core::workflow::load_def_by_name(
                    &r.local_path,
                    &r.local_path,
                    &name,
                )?;

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

                run_and_report(exec_params(
                    config,
                    workflow,
                    ExecTarget::for_repo(&r),
                    ExecOptions {
                        model: model.clone(),
                        exec_config: exec_config.clone(),
                        inputs: input_map,
                        extra_plugin_dirs: plugin_dirs.clone(),
                        db_path: None,
                    },
                ))?;
            } else if let Some(run_id) = workflow_run {
                // Workflow-run targeted run (e.g. postmortem workflows)
                let ctx = conductor_core::workflow::resolve_run_context(conn, &run_id, config)?;

                // Auto-inject the workflow_run_id input (user --input flags merge after)
                input_map
                    .entry("workflow_run_id".to_string())
                    .or_insert_with(|| run_id.clone());

                let workflow = conductor_core::workflow::load_def_by_name(
                    &ctx.working_dir,
                    &ctx.repo_path,
                    &name,
                )?;

                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                let node_count = workflow.total_nodes();
                println!(
                    "Running workflow '{}' ({} nodes) on workflow run {}...",
                    workflow.name, node_count, run_id
                );

                run_and_report(exec_params(
                    config,
                    workflow,
                    ExecTarget {
                        worktree_id: ctx.worktree_id.clone(),
                        working_dir: ctx.working_dir.clone(),
                        repo_path: ctx.repo_path.clone(),
                        ticket_id: None,
                        repo_id: ctx.repo_id.clone(),
                        target_label: Some(run_id.clone()),
                    },
                    ExecOptions {
                        model: model.clone(),
                        exec_config: exec_config.clone(),
                        inputs: input_map,
                        extra_plugin_dirs: plugin_dirs.clone(),
                        db_path: None,
                    },
                ))?;
            } else if let Some(ticket_id) = ticket {
                let syncer = TicketSyncer::new(conn);
                let ticket = syncer.get_by_id(&ticket_id)?;
                let repo_mgr = RepoManager::new(conn, config);
                let repo = repo_mgr.get_by_id(&ticket.repo_id)?;

                let workflow = conductor_core::workflow::load_def_by_name(
                    &repo.local_path,
                    &repo.local_path,
                    &name,
                )?;

                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                println!(
                    "Running workflow '{}' ({} nodes) on ticket {}...",
                    workflow.name,
                    workflow.total_nodes(),
                    ticket_id
                );

                run_and_report(exec_params(
                    config,
                    workflow,
                    ExecTarget {
                        worktree_id: None,
                        working_dir: repo.local_path.clone(),
                        repo_path: repo.local_path.clone(),
                        ticket_id: Some(ticket_id.clone()),
                        repo_id: Some(ticket.repo_id.clone()),
                        target_label: Some(repo.slug.clone()),
                    },
                    ExecOptions {
                        model: model.clone(),
                        exec_config: exec_config.clone(),
                        inputs: input_map,
                        extra_plugin_dirs: plugin_dirs.clone(),
                        db_path: None,
                    },
                ))?;
            } else {
                // Normal registered repo/worktree run
                let repo_slug = repo
                    .ok_or_else(|| anyhow::anyhow!("--repo is required when --pr is not used"))?;
                let worktree_slug = worktree.ok_or_else(|| {
                    anyhow::anyhow!("--worktree is required when --pr is not used")
                })?;

                let repo_mgr = RepoManager::new(conn, config);
                let r = repo_mgr.get_by_slug(&repo_slug)?;
                let wt_mgr = WorktreeManager::new(conn, config);
                let wt = wt_mgr.get_by_slug_or_branch(&r.id, &worktree_slug)?;

                let workflow =
                    conductor_core::workflow::load_def_by_name(&wt.path, &r.local_path, &name)?;

                // Validate required inputs and apply defaults
                conductor_core::workflow::apply_workflow_input_defaults(&workflow, &mut input_map)?;

                let node_count = workflow.total_nodes();
                let wt_label = format!("{repo_slug}/{}", wt.slug);

                #[cfg(unix)]
                if let Some(notify_fd) = background_notify_fd {
                    let params = exec_params(
                        config,
                        workflow,
                        ExecTarget {
                            worktree_id: Some(wt.id.clone()),
                            working_dir: wt.path.clone(),
                            repo_path: r.local_path.clone(),
                            ticket_id: wt.ticket_id.clone(),
                            repo_id: Some(r.id.clone()),
                            target_label: Some(wt_label),
                        },
                        ExecOptions {
                            model,
                            exec_config,
                            inputs: input_map,
                            extra_plugin_dirs: plugin_dirs,
                            db_path: None,
                        },
                    );
                    crate::background::run_as_background_child(params, None, notify_fd);
                }
                #[cfg(unix)]
                if background {
                    let run_id = crate::background::spawn_background_workflow()?;
                    println!("{}", run_id);
                    return Ok(());
                }
                #[cfg(not(unix))]
                if background || background_notify_fd.is_some() {
                    anyhow::bail!("--background is only supported on Unix systems");
                }

                println!(
                    "Running workflow '{}' ({} nodes) on {}/{}...",
                    workflow.name, node_count, repo_slug, wt.slug
                );

                run_and_report(exec_params(
                    config,
                    workflow,
                    ExecTarget {
                        worktree_id: Some(wt.id.clone()),
                        working_dir: wt.path.clone(),
                        repo_path: r.local_path.clone(),
                        ticket_id: wt.ticket_id.clone(),
                        repo_id: Some(r.id.clone()),
                        target_label: Some(wt_label.clone()),
                    },
                    ExecOptions {
                        model: model.clone(),
                        exec_config: exec_config.clone(),
                        inputs: input_map,
                        extra_plugin_dirs: plugin_dirs,
                        db_path: None,
                    },
                ))?;
            }
        }
        WorkflowCommands::Wait {
            id,
            timeout,
            stall_after,
            through_gates,
            json,
        } => {
            let opts = conductor_core::workflow::WaitOptions::with_overrides(
                timeout.map(std::time::Duration::from_secs),
                stall_after.map(std::time::Duration::from_secs),
                through_gates,
            );
            let summary = conductor_core::workflow::wait_for_workflow_run(conn, &id, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print!(
                    "{}",
                    conductor_core::workflow::format_wait_summary(&summary)
                );
            }
            let code = summary.outcome.exit_code();
            if code != 0 {
                std::process::exit(code);
            }
        }
        WorkflowCommands::RunStats {
            run_ids,
            compare,
            json,
        } => {
            let text = conductor_core::workflow::run_stats::render_run_stats(
                conn, &run_ids, &compare, json,
            )?;
            if json {
                println!("{text}");
            } else {
                print!("{text}");
            }
        }
        WorkflowCommands::ReviewShow {
            run_id,
            repo,
            pr,
            json,
        } => {
            use conductor_core::workflow::review_report::{
                find_review_run_by_slug, format_review_report, load_review_report,
            };
            let id = find_review_run_by_slug(conn, config, run_id.as_deref(), repo.as_deref(), pr)?;
            let report = load_review_report(conn, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_review_report(&report));
            }
        }
        WorkflowCommands::StepLog {
            run_id,
            step_name,
            mode,
            lines,
        } => {
            use conductor_core::workflow::{render_step_log, resolve_step_log_target, StepLogMode};
            let mode = StepLogMode::parse(Some(&mode), lines)?;
            let target = resolve_step_log_target(conn, &run_id, &step_name, mode)?;
            print!("{}", render_step_log(&target, mode)?);
        }
        WorkflowCommands::RunShow { id } => {
            match conductor_core::workflow::get_workflow_run(conn, &id)? {
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
                    let run_chain = conductor_core::workflow::run_chain_if_failed(conn, &run);
                    if let Some(cause) = conductor_core::workflow::format_failure_cause(&run_chain)
                    {
                        println!("\nCause: {cause}");
                    }

                    let steps = conductor_core::workflow::get_workflow_steps(conn, &run.id)?;
                    if !steps.is_empty() {
                        println!("\nSteps:");
                        for step in &steps {
                            println!("{}", step_header_line(step));
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
                            if let Some(ref err) = step.step_error {
                                if !err.is_empty() {
                                    println!("        validation error: {err}");
                                }
                            }
                            if let Some(excerpt) =
                                conductor_core::workflow::step_output_error_excerpt(conn, step)
                            {
                                println!("        {excerpt}");
                            }
                            if let Some(block) = conductor_core::workflow::script_stderr_block(
                                step,
                                "conductor workflow step-log",
                                "        ",
                            ) {
                                print!("{block}");
                            }
                            if let Some(line) = step_child_run_line(step) {
                                println!("{line}");
                            }
                            print!(
                                "{}",
                                conductor_core::workflow::render_step_failure_tree(
                                    conn, step, &run_chain, 8
                                )
                            );
                            // foreach fan-out progress
                            if step.role == "foreach" {
                                if let Some(total) = step.fan_out_total {
                                    println!(
                                        "        fan-out: {}/{} completed  ({} failed, {} skipped)",
                                        step.fan_out_completed,
                                        total,
                                        step.fan_out_failed,
                                        step.fan_out_skipped,
                                    );
                                }
                            }
                        }
                    }
                }
                None => {
                    println!("Workflow run not found: {id}");
                }
            }
        }
        cmd @ WorkflowCommands::Validate { .. } => validate(cmd, Some(conn), config)?,
        WorkflowCommands::Resume {
            id,
            from_step,
            model,
            restart,
        } => {
            let _process_guard = register_foreground_run();
            let resume_input = conductor_core::workflow::WorkflowResumeInput {
                config,
                workflow_run_id: &id,
                model: model.as_deref(),
                runtime: None,
                from_step: from_step.as_deref(),
                restart,
                conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
                event_sinks: vec![],
                db_path: None,
                shutdown: None,
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
                    let metrics = result
                        .extensions
                        .get::<conductor_core::workflow::LlmRunMetrics>();
                    let total_cost = metrics
                        .as_ref()
                        .and_then(|m| m.total_cost_usd)
                        .unwrap_or(0.0);
                    let total_turns = metrics.as_ref().and_then(|m| m.total_turns).unwrap_or(0);
                    println!(
                        "\nTotal: ${:.4}, {} turns, {:.1}s",
                        total_cost,
                        total_turns,
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
        WorkflowCommands::LaunchDetached { notify_fd } => {
            #[cfg(unix)]
            launch_detached(config, notify_fd)?;
            #[cfg(not(unix))]
            {
                let _ = notify_fd;
                anyhow::bail!("launch-detached is only supported on Unix systems");
            }
        }
        WorkflowCommands::Cancel { id } => {
            match conductor_core::workflow::cancel_run(conn, &id, "Cancelled by user") {
                Ok(()) => println!("Workflow run {id} cancelled."),
                Err(e) => {
                    eprintln!("Failed to cancel workflow run {id}: {e}");
                    std::process::exit(1);
                }
            }
        }
        WorkflowCommands::GateApprove { run_id } => {
            with_waiting_gate(conn, &run_id, |step, user| {
                conductor_core::workflow::approve_gate(conn, &step.id, user, None, None, None)?;
                println!("Gate '{}' approved by {user}.", step.step_name);
                Ok(())
            })?;
        }
        WorkflowCommands::GateReject { run_id } => {
            with_waiting_gate(conn, &run_id, |step, user| {
                conductor_core::workflow::reject_gate(conn, &step.id, user, None)?;
                let reject_msg = format!("Gate '{}' rejected by {user}", step.step_name);
                conductor_core::workflow::update_workflow_status(
                    conn,
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
            with_waiting_gate(conn, &run_id, |step, user| {
                conductor_core::workflow::approve_gate(
                    conn,
                    &step.id,
                    user,
                    Some(&feedback),
                    None,
                    None,
                )?;
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
            if dry_run {
                let count =
                    conductor_core::workflow::purge_count(conn, repo_id.as_deref(), &statuses)?;
                println!("Would purge {count} workflow run(s) (dry run).");
            } else {
                let count = conductor_core::workflow::purge(conn, repo_id.as_deref(), &statuses)?;
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
fn run_and_report(input: conductor_core::workflow::WorkflowExecStandalone) -> Result<()> {
    let result = conductor_core::workflow::execute_workflow_standalone(&input)
        .map_err(|e| anyhow::anyhow!("Workflow execution failed: {e}"))?;
    report_workflow_result(result);
    Ok(())
}

/// Shared gate operation: find the waiting gate, resolve the current user,
/// then call the provided action closure.
fn with_waiting_gate(
    conn: &Connection,
    run_id: &str,
    action: impl FnOnce(&conductor_core::workflow::WorkflowRunStep, &str) -> Result<()>,
) -> Result<()> {
    match conductor_core::workflow::find_waiting_gate(conn, run_id)? {
        Some(step) => {
            let user = std::env::var("USER").unwrap_or_else(|_| "cli".to_string());
            action(&step, &user)
        }
        None => {
            println!("No waiting gate found for workflow run: {run_id}");
            Ok(())
        }
    }
}

/// `[marker] name (role[ [commit]][, child <id>]) iter=N` header for a step in run-show.
///
/// The child run ID is shown only for `call workflow` steps; other steps' `child_run_id`
/// is an agent run ID (see [`step_child_run_line`]).
fn step_header_line(step: &conductor_core::workflow::WorkflowRunStep) -> String {
    let commit_flag = if step.can_commit { " [commit]" } else { "" };
    let iter_label = if step.iteration > 0 {
        format!(" iter={}", step.iteration)
    } else {
        String::new()
    };
    let child_label = match (&step.child_run_id, is_workflow_call_step(step)) {
        (Some(child), true) => format!(", child {child}"),
        _ => String::new(),
    };
    format!(
        "  [{}] {} ({}{commit_flag}{child_label}){iter_label}",
        step.status.short_label(),
        step.step_name,
        step.role
    )
}

/// `child run: <id>` detail line for steps whose `child_run_id` is not a workflow run.
fn step_child_run_line(step: &conductor_core::workflow::WorkflowRunStep) -> Option<String> {
    match &step.child_run_id {
        Some(child) if !is_workflow_call_step(step) => Some(format!("        child run: {child}")),
        _ => None,
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use conductor_core::workflow::{WorkflowRunStep, WorkflowStepStatus};

    fn step(role: &str, child: Option<&str>) -> WorkflowRunStep {
        let conn = conductor_core::test_helpers::setup_db();
        conductor_core::test_helpers::insert_test_wf_run(&conn, "r", "wf", "failed", None);
        conductor_core::test_helpers::insert_test_wf_step(
            &conn,
            "s",
            "r",
            "build",
            role,
            "failed",
            0,
            "2026-01-01T00:00:01Z",
            child,
            Some("boom"),
        );
        let mut steps = conductor_core::workflow::get_workflow_steps(&conn, "r").unwrap();
        let mut step = steps.remove(0);
        step.status = WorkflowStepStatus::Failed;
        step
    }

    #[test]
    fn call_step_header_shows_child_and_suppresses_child_run_line() {
        let s = step("workflow", Some("C1"));
        assert_eq!(step_header_line(&s), "  [FAIL] build (workflow, child C1)");
        assert_eq!(step_child_run_line(&s), None);
    }

    #[test]
    fn non_call_step_keeps_child_run_line_and_hides_child_in_header() {
        let s = step("actor", Some("A1"));
        assert_eq!(step_header_line(&s), "  [FAIL] build (actor)");
        assert_eq!(
            step_child_run_line(&s).as_deref(),
            Some("        child run: A1")
        );
    }

    #[test]
    fn step_without_child_has_no_child_output() {
        let s = step("workflow", None);
        assert_eq!(step_header_line(&s), "  [FAIL] build (workflow)");
        assert_eq!(step_child_run_line(&s), None);
    }
}

#[cfg(test)]
mod step_log_cli_tests {
    use crate::commands::{Cli, Commands, WorkflowCommands};
    use clap::Parser;
    use conductor_core::workflow::StepLogMode;

    /// Parse `conductor workflow step-log ...` and return its (mode, lines) arguments.
    fn parse(extra: &[&str]) -> Result<(String, Option<usize>), clap::Error> {
        let mut argv = vec!["conductor", "workflow", "step-log", "run1", "plan"];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv)?.command {
            Commands::Workflow {
                command:
                    WorkflowCommands::StepLog {
                        run_id,
                        step_name,
                        mode,
                        lines,
                    },
            } => {
                assert_eq!((run_id.as_str(), step_name.as_str()), ("run1", "plan"));
                Ok((mode, lines))
            }
            _ => panic!("expected workflow step-log"),
        }
    }

    #[test]
    fn mode_defaults_to_result() {
        let (mode, lines) = parse(&[]).unwrap();
        assert_eq!(mode, "result");
        assert_eq!(lines, None);
        assert_eq!(
            StepLogMode::parse(Some(&mode), lines).unwrap(),
            StepLogMode::Result
        );
    }

    #[test]
    fn mode_and_lines_flags_are_parsed() {
        let (mode, lines) = parse(&["--mode", "tail", "--lines", "50"]).unwrap();
        assert_eq!((mode.as_str(), lines), ("tail", Some(50)));
        assert_eq!(
            StepLogMode::parse(Some(&mode), lines).unwrap(),
            StepLogMode::Tail { lines: 50 }
        );
    }

    #[test]
    fn prompt_mode_is_accepted() {
        let (mode, lines) = parse(&["--mode", "prompt"]).unwrap();
        assert_eq!(
            StepLogMode::parse(Some(&mode), lines).unwrap(),
            StepLogMode::Prompt
        );
    }

    #[test]
    fn invalid_mode_is_rejected_by_the_handler_parse() {
        let (mode, lines) = parse(&["--mode", "bogus"]).unwrap();
        let err = StepLogMode::parse(Some(&mode), lines).unwrap_err();
        assert!(err.to_string().contains("Invalid mode 'bogus'"), "{err}");
    }

    #[test]
    fn non_numeric_lines_is_a_parse_error() {
        assert!(parse(&["--lines", "many"]).is_err());
    }
}

/// One smoke target's result: printed as the human lines or, with `--json`,
/// as one JSON object.
#[derive(serde::Serialize)]
struct SmokeReport {
    workflow: String,
    status: SmokeStatus,
    run_id: Option<String>,
    stopped_at: Option<SmokeStopJson>,
    warnings: Vec<String>,
    error: Option<String>,
    missing_inputs: Vec<String>,
    /// The human result line and the `SMOKE WARN` lines, exactly as printed
    /// without `--json`.
    #[serde(skip)]
    human_lines: Vec<String>,
}

#[derive(serde::Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum SmokeStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(serde::Serialize)]
struct SmokeStopJson {
    workflow: String,
    step: String,
    kind: String,
}

impl SmokeReport {
    fn new(workflow: &str, status: SmokeStatus) -> Self {
        Self {
            workflow: workflow.to_string(),
            status,
            run_id: None,
            stopped_at: None,
            warnings: Vec::new(),
            error: None,
            missing_inputs: Vec::new(),
            human_lines: Vec::new(),
        }
    }
}

impl SmokeReport {
    /// A failed report carrying `error`.
    fn failed(workflow: &str, error: String) -> Self {
        let mut report = Self::new(workflow, SmokeStatus::Fail);
        report.error = Some(error);
        report
    }
}

fn print_smoke_json(report: &SmokeReport) {
    match serde_json::to_string(report) {
        Ok(line) => println!("{line}"),
        Err(e) => eprintln!("could not serialize smoke report: {e}"),
    }
}

/// The workflow's static-check entry when it failed them (so its failure is
/// already counted and it has no smoke).
fn static_failure_entry<'a>(
    result: &'a conductor_core::workflow::BatchValidationResult,
    wf: &conductor_core::workflow::WorkflowDef,
) -> Option<&'a conductor_core::workflow::WorkflowValidationEntry> {
    result
        .entries
        .iter()
        .find(|e| e.name == wf.name && !e.errors.is_empty())
}

/// Smoke-start one workflow and build its report.
fn smoke_one(
    conn: &Connection,
    config: &Config,
    workflow: &conductor_core::workflow::WorkflowDef,
    target: &ExecTarget,
    base_inputs: &std::collections::HashMap<String, String>,
    skip_unfillable: bool,
) -> SmokeReport {
    use conductor_core::workflow::SmokeResult;

    let mut target = target.clone();
    let mut inputs = base_inputs.clone();
    // The ticket the run is about: the worktree's linked ticket, else an
    // explicit `--input ticket_id=...` that names a known ticket.
    if let Some(tid) = target.ticket_id.clone() {
        inputs.entry("ticket_id".to_string()).or_insert(tid);
    } else if let Some(tid) = inputs.get("ticket_id") {
        if TicketSyncer::new(conn).get_by_id(tid).is_ok() {
            target.ticket_id = Some(tid.clone());
        }
    }
    // Check required inputs only: defaults are filled (and checked) by the shared
    // run-start path, so the smoke exercises the same path as a real run.
    if let Some(report) = smoke_required_input_gate(workflow, &inputs, skip_unfillable) {
        return report;
    }

    let outcome = conductor_core::workflow::smoke_run_workflow(exec_params(
        config,
        workflow.clone(),
        target,
        ExecOptions {
            model: None,
            exec_config: WorkflowExecConfig::default(),
            inputs,
            extra_plugin_dirs: vec![],
            db_path: None,
        },
    ));
    let run = outcome
        .run_id
        .as_deref()
        .map(|id| format!("  (run {id})"))
        .unwrap_or_default();
    let mut report = SmokeReport::new(&workflow.name, SmokeStatus::Pass);
    report.run_id = outcome.run_id.clone();
    match outcome.result {
        SmokeResult::Passed {
            stopped_at: Some(point),
        } => {
            let kind = point.kind;
            let child = if point.workflow == workflow.name {
                String::new()
            } else {
                format!(" in '{}'", point.workflow)
            };
            report.human_lines.push(format!(
                "SMOKE PASS  {}  stopped at {kind} step '{}'{child}{run}",
                workflow.name, point.step
            ));
            report.stopped_at = Some(SmokeStopJson {
                workflow: point.workflow.clone(),
                step: point.step.clone(),
                kind: kind.to_string(),
            });
        }
        SmokeResult::Passed { stopped_at: None } => {
            report.human_lines.push(format!(
                "SMOKE PASS  {}  ran to completion, no agent step{run}",
                workflow.name
            ));
        }
        SmokeResult::Failed { step, error } => {
            let at = step
                .as_ref()
                .map(|s| format!(" at step '{s}'"))
                .unwrap_or_default();
            report
                .human_lines
                .push(format!("SMOKE FAIL  {}{at}: {error}{run}", workflow.name));
            report.status = SmokeStatus::Fail;
            report.stopped_at = step.map(|s| SmokeStopJson {
                workflow: workflow.name.clone(),
                step: s,
                kind: "failed".to_string(),
            });
            report.error = Some(error.to_string());
        }
    }
    report.warnings = smoke_warnings(
        &workflow.name,
        &outcome.env_warnings,
        &outcome.env_scan_errors,
    );
    report.human_lines.extend(
        report
            .warnings
            .iter()
            .map(|t| smoke_warn_line(&workflow.name, t)),
    );
    report
}

/// The report (result line, status, every missing required input) when a smoke can't
/// start because a required input has no value: a skip when `skip_unfillable`,
/// else a failure. `None` when every required input is present.
fn smoke_required_input_gate(
    workflow: &conductor_core::workflow::WorkflowDef,
    inputs: &std::collections::HashMap<String, String>,
    skip_unfillable: bool,
) -> Option<SmokeReport> {
    let e = conductor_core::workflow::check_required_workflow_inputs(workflow, inputs).err()?;
    let (status, line) = if skip_unfillable {
        (
            SmokeStatus::Skip,
            format!("SMOKE SKIP  {}  ({e})", workflow.name),
        )
    } else {
        (
            SmokeStatus::Fail,
            format!("SMOKE FAIL  {}  start: {e}", workflow.name),
        )
    };
    let mut report = SmokeReport::failed(&workflow.name, e.to_string());
    report.status = status;
    report.missing_inputs =
        conductor_core::workflow::missing_required_workflow_inputs(workflow, inputs);
    report.human_lines = vec![line];
    Some(report)
}

/// The human `SMOKE WARN` line for one warning text.
fn smoke_warn_line(workflow_name: &str, text: &str) -> String {
    format!("SMOKE WARN  {workflow_name}  {text}")
}

/// The warning texts for a smoke outcome: one per unresolved script env
/// input (with an ` in '<child>'` suffix for a child workflow) and one per
/// scan error, so an incomplete scan never reads as a clean one.
fn smoke_warnings(
    workflow_name: &str,
    warnings: &[conductor_core::workflow::UnresolvedEnvInput],
    scan_errors: &[String],
) -> Vec<String> {
    let mut texts: Vec<String> = warnings
        .iter()
        .map(|w| {
            let child = if w.workflow == workflow_name {
                String::new()
            } else {
                format!(" in '{}'", w.workflow)
            };
            format!("{}{child}", w.describe())
        })
        .collect();
    texts.extend(
        scan_errors
            .iter()
            .map(|e| format!("script env scan incomplete: {e}")),
    );
    texts
}

#[cfg(test)]
mod smoke_warn_tests {
    use super::*;
    use conductor_core::workflow::UnresolvedEnvInput;

    fn warn(workflow: &str) -> UnresolvedEnvInput {
        UnresolvedEnvInput {
            workflow: workflow.into(),
            step: "s".into(),
            env_var: "E".into(),
            input: "opt".into(),
        }
    }

    fn warn_lines(
        name: &str,
        warnings: &[UnresolvedEnvInput],
        scan_errors: &[String],
    ) -> Vec<String> {
        smoke_warnings(name, warnings, scan_errors)
            .into_iter()
            .map(|t| smoke_warn_line(name, &t))
            .collect()
    }

    #[test]
    fn warn_line_has_child_suffix_only_for_a_child_workflow() {
        let lines = warn_lines("parent", &[warn("parent"), warn("child")], &[]);
        assert_eq!(
            lines[0],
            "SMOKE WARN  parent  script step 's' env E uses input 'opt', which has no value"
        );
        assert_eq!(
            lines[1],
            "SMOKE WARN  parent  script step 's' env E uses input 'opt', which has no value in 'child'"
        );
    }

    #[test]
    fn scan_errors_become_warn_lines() {
        let lines = warn_lines("wf", &[], &["run r1: could not read the run".into()]);
        assert_eq!(
            lines,
            vec!["SMOKE WARN  wf  script env scan incomplete: run r1: could not read the run"]
        );
    }

    fn wf_with_required(required: bool) -> conductor_core::workflow::WorkflowDef {
        let mut w = conductor_core::workflow::parse_workflow_str(
            "workflow wf {\n meta { trigger = \"manual\" targets = [\"worktree\"] }\n inputs { need }\n call some-agent\n}",
            "<test>",
        )
        .expect("test workflow should parse");
        // The parser makes a bare input required; only a hand-built def is optional.
        w.inputs[0].required = required;
        w
    }

    #[test]
    fn missing_required_input_skips_or_fails() {
        let w = wf_with_required(true);
        let none = std::collections::HashMap::new();
        let r = smoke_required_input_gate(&w, &none, true).unwrap();
        assert!(
            r.human_lines[0].starts_with("SMOKE SKIP  wf  (")
                && r.human_lines[0].contains("'need'"),
            "{:?}",
            r.human_lines
        );
        assert_eq!(r.status, SmokeStatus::Skip);
        assert_eq!(r.missing_inputs, vec!["need".to_string()]);
        let r = smoke_required_input_gate(&w, &none, false).unwrap();
        assert!(
            r.human_lines[0].starts_with("SMOKE FAIL  wf  start: ")
                && r.human_lines[0].contains("'need'"),
            "{:?}",
            r.human_lines
        );
        assert_eq!(r.status, SmokeStatus::Fail);
        assert_eq!(r.missing_inputs, vec!["need".to_string()]);
    }

    #[test]
    fn present_or_optional_input_passes_the_gate() {
        let have = std::collections::HashMap::from([("need".to_string(), "v".to_string())]);
        assert!(smoke_required_input_gate(&wf_with_required(true), &have, false).is_none());
        let none = std::collections::HashMap::new();
        assert!(smoke_required_input_gate(&wf_with_required(false), &none, false).is_none());
    }
}

#[cfg(test)]
mod script_dirs_tests {
    use super::*;

    #[test]
    fn registered_worktree_uses_worktree_then_repo() {
        let got = pick_script_dirs("/d".into(), Ok(Some(("/wt".into(), "/repo".into()))));
        assert_eq!(got, ("/wt".to_string(), "/repo".to_string()));
    }

    #[test]
    fn unregistered_dir_is_used_for_both() {
        let got = pick_script_dirs("/d".into(), Ok(None));
        assert_eq!(got, ("/d".to_string(), "/d".to_string()));
    }

    #[test]
    fn lookup_error_falls_back_to_dir() {
        let got = pick_script_dirs("/d".into(), Err(anyhow::anyhow!("db locked")));
        assert_eq!(got, ("/d".to_string(), "/d".to_string()));
    }

    #[test]
    fn db_lookup_selects_registered_worktree_and_unregistered_fallback() {
        let conn = conductor_core::test_helpers::create_test_conn();
        let config = Config::default();
        conductor_core::test_helpers::insert_test_repo(&conn, "r1", "repo", "/tmp/sd-main");
        conductor_core::test_helpers::insert_test_worktree(
            &conn,
            "w1",
            "r1",
            "feat-x",
            "/tmp/sd-wt/feat-x",
        );
        assert_eq!(
            template_script_dirs(Some(&conn), &config, "/tmp/sd-wt/feat-x".into()),
            ("/tmp/sd-wt/feat-x".to_string(), "/tmp/sd-main".to_string())
        );
        assert_eq!(
            template_script_dirs(Some(&conn), &config, "/tmp/sd-other".into()),
            ("/tmp/sd-other".to_string(), "/tmp/sd-other".to_string())
        );
        assert_eq!(
            template_script_dirs(None, &config, "/tmp/sd-wt/feat-x".into()),
            ("/tmp/sd-wt/feat-x".to_string(), "/tmp/sd-wt/feat-x".to_string())
        );
    }
}
