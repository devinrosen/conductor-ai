use std::collections::HashMap;
use std::path::Path;

use rmcp::model::CallToolResult;
use serde_json::Value;

use crate::mcp::helpers::{get_arg, open_db_and_config, tool_err, tool_ok};
use crate::mcp::resources::format_workflow_def;

pub(super) fn tool_list_workflows(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowManager;

    let repo_slug = require_arg!(args, "repo");
    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };
    let (defs, warnings) = match WorkflowManager::list_defs(&repo.local_path, &repo.local_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let mut out = String::new();
    for w in &warnings {
        out.push_str(&format!(
            "warning: Failed to parse {}: {}\n",
            w.file, w.message
        ));
    }
    if defs.is_empty() {
        out.push_str(&format!("No workflow definitions found in {repo_slug}."));
    } else {
        for def in defs {
            out.push_str(&format_workflow_def(&def));
        }
    }
    tool_ok(out)
}

pub(super) fn tool_validate_workflow(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::WorkflowManager;

    let repo_slug = require_arg!(args, "repo");
    let workflow_name = require_arg!(args, "workflow");

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo = match RepoManager::new(&conn, &config).get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    let wt_path = &repo.local_path;
    let repo_path = &repo.local_path;

    let workflow = match WorkflowManager::load_def_by_name(wt_path, repo_path, workflow_name) {
        Ok(w) => w,
        Err(e) => return tool_err(e),
    };

    let known_bots: std::collections::HashSet<String> =
        config.github.apps.keys().cloned().collect();

    let entry = match WorkflowManager::validate_single(wt_path, repo_path, &workflow, &known_bots) {
        Some(e) => e,
        None => {
            return tool_err(format!(
                "validation produced no result for workflow '{workflow_name}'"
            ))
        }
    };

    let mut errors: Vec<String> = Vec::new();
    for err in &entry.errors {
        if let Some(hint) = &err.hint {
            errors.push(format!("{} (hint: {hint})", err.message));
        } else {
            errors.push(err.message.clone());
        }
    }

    let mut warnings: Vec<String> = Vec::new();
    for w in &entry.warnings {
        warnings.push(w.message.to_string());
    }

    if errors.is_empty() && warnings.is_empty() {
        tool_ok(format!(
            "status: PASS\n\nWorkflow '{workflow_name}' is valid."
        ))
    } else if errors.is_empty() {
        let warning_list = warnings
            .iter()
            .map(|w| format!("- {w}"))
            .collect::<Vec<_>>()
            .join("\n");
        tool_ok(format!(
            "status: PASS\n\nWorkflow '{workflow_name}' is valid.\n\nWarnings:\n{warning_list}"
        ))
    } else {
        let error_list = errors
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        tool_ok(format!("status: FAIL\n\nErrors:\n{error_list}"))
    }
}

pub(super) fn tool_run_workflow(
    db_path: &Path,
    args: &serde_json::Map<String, Value>,
) -> CallToolResult {
    use conductor_core::repo::RepoManager;
    use conductor_core::workflow::{
        execute_workflow_standalone, RunIdSlot, WorkflowExecConfig, WorkflowExecStandalone,
        WorkflowManager,
    };
    use conductor_core::worktree::WorktreeManager;
    use std::sync::{Arc, Mutex};

    let workflow_name = require_arg!(args, "workflow");
    let repo_slug = require_arg!(args, "repo");
    let worktree_slug = get_arg(args, "worktree");
    let pr_arg = get_arg(args, "pr");
    let feature_arg = get_arg(args, "feature");
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // pr and worktree are mutually exclusive
    if pr_arg.is_some() && worktree_slug.is_some() {
        return tool_err("pr and worktree are mutually exclusive — provide one or the other");
    }

    // Validate and parse pr number early, before any DB access.
    let pr_number: Option<i64> = if let Some(pr_str) = pr_arg {
        use conductor_core::github::parse_pr_number_from_url;
        if let Ok(n) = pr_str.parse::<i64>() {
            Some(n)
        } else if let Some(n) = parse_pr_number_from_url(pr_str) {
            Some(n)
        } else {
            return tool_err(format!(
                "Invalid pr value '{pr_str}': expected a PR number (e.g. \"123\") or URL (e.g. \"https://github.com/owner/repo/pull/123\")"
            ));
        }
    } else {
        None
    };

    // Extract optional inputs object
    let inputs: HashMap<String, String> = match args.get("inputs") {
        None => HashMap::new(),
        Some(Value::Object(map)) => {
            let mut result = HashMap::new();
            for (k, v) in map {
                match v.as_str() {
                    Some(s) => {
                        result.insert(k.clone(), s.to_string());
                    }
                    None => return tool_err(format!("inputs.{k} must be a string value")),
                }
            }
            result
        }
        Some(other) => return tool_err(format!("inputs must be an object, got: {other}")),
    };

    let (conn, config) = match open_db_and_config(db_path) {
        Ok(v) => v,
        Err(e) => return tool_err(e),
    };
    let repo_mgr = RepoManager::new(&conn, &config);
    let repo = match repo_mgr.get_by_slug(repo_slug) {
        Ok(r) => r,
        Err(e) => return tool_err(e),
    };

    // Resolve optional --feature to a feature_id via the unified resolver
    let feature_id: Option<String> = {
        use conductor_core::feature::FeatureManager;
        let fm = FeatureManager::new(&conn, &config);
        match fm.resolve_feature_id_for_run(feature_arg, Some(repo_slug), None, worktree_slug) {
            Ok(id) => id,
            Err(e) => return tool_err(e),
        }
    };

    // Load the workflow definition
    let workflow = match WorkflowManager::load_def_by_name(
        &repo.local_path,
        &repo.local_path,
        workflow_name,
    ) {
        Ok(w) => w,
        Err(e) => return tool_err(format!("Failed to load workflow '{workflow_name}': {e}")),
    };

    // Resolve worktree: either from `worktree` slug or from `pr` number/URL
    let (worktree_id, working_dir, target_label) = if let Some(pr_number) = pr_number {
        use conductor_core::github::get_pr_head_branch;

        // Fetch the head branch from GitHub
        let branch = match get_pr_head_branch(&repo.remote_url, pr_number) {
            Ok(b) => b,
            Err(e) => return tool_err(e),
        };

        // Find the local worktree for that branch
        let wt_mgr = WorktreeManager::new(&conn, &config);
        let wt = match wt_mgr.get_by_branch(&repo.id, &branch) {
            Ok(wt) => wt,
            Err(conductor_core::error::ConductorError::WorktreeNotFound { .. }) => {
                return tool_err(format!(
                    "PR #{pr_number} (branch: {branch}) has no local worktree. \
                     Create one with conductor_create_worktree first."
                ))
            }
            Err(e) => return tool_err(e),
        };
        (Some(wt.id), wt.path, format!("{}#{}", repo_slug, pr_number))
    } else if let Some(wt_slug) = worktree_slug {
        let wt_mgr = WorktreeManager::new(&conn, &config);
        match wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug) {
            Ok(wt) => (Some(wt.id), wt.path, repo_slug.to_string()),
            Err(e) => return tool_err(e),
        }
    } else {
        (None, repo.local_path.clone(), repo_slug.to_string())
    };

    // Condvar-based notification: the workflow engine writes the run ID here and
    // signals the condvar once the run record is created (before any steps execute).
    let notify_pair: RunIdSlot = Arc::new((Mutex::new(None), std::sync::Condvar::new()));

    // Fire-and-forget: execute in a background thread
    let standalone = WorkflowExecStandalone {
        config,
        workflow,
        worktree_id,
        working_dir,
        repo_path: repo.local_path,
        ticket_id: None,
        repo_id: Some(repo.id),
        model: None,
        exec_config: WorkflowExecConfig {
            dry_run,
            ..WorkflowExecConfig::default()
        },
        inputs,
        target_label: Some(target_label),
        feature_id,
        run_id_notify: Some(Arc::clone(&notify_pair)),
    };

    // Slot receives the error message if execute_workflow_standalone fails before
    // creating the run record (i.e., before writing to run_id_notify).
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_slot_bg = Arc::clone(&error_slot);
    let notify_pair_bg = Arc::clone(&notify_pair);

    std::thread::spawn(move || {
        if let Err(e) = execute_workflow_standalone(&standalone) {
            *error_slot_bg.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string());
            // Wake the waiter so it surfaces the error immediately.
            notify_pair_bg.1.notify_one();
        }
    });

    // Block (without spinning) until the run record is created or 2 s elapses.
    let (lock, cvar) = notify_pair.as_ref();
    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let (guard, _timed_out) = cvar
        .wait_timeout_while(guard, std::time::Duration::from_secs(2), |v| {
            v.is_none()
                && error_slot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
        })
        .unwrap_or_else(|e| e.into_inner());

    // Surface startup errors before checking for the run ID.
    if let Some(err) = error_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        return tool_err(format!("Workflow failed to start: {err}"));
    }

    let run_id = match guard.as_ref() {
        Some(id) => id.clone(),
        None => {
            return tool_err(
                "Workflow started but run ID was not available within 2 s. \
             The workflow may still be running in the background — \
             use conductor_list_runs to check status.",
            )
        }
    };

    tool_ok(format!(
        "Workflow '{workflow_name}' started.\nrun_id: {run_id}\nstatus: pending\ndry_run: {dry_run}\nPoll progress with conductor_get_run."
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn make_test_db() -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use conductor_core::db::open_database;
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        open_database(&path).expect("open_database");
        (file, path)
    }

    fn empty_args() -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }

    fn args_with(key: &str, val: &str) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), Value::String(val.to_string()));
        m
    }

    /// Write a minimal `.conductor/workflows/<name>.wf` file under a temp dir
    /// and return the temp dir (kept alive).
    fn make_wf_dir_with_workflow(name: &str, content: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let wf_dir = dir.path().join(".conductor").join("workflows");
        std::fs::create_dir_all(&wf_dir).expect("create workflow dir");
        std::fs::write(wf_dir.join(format!("{name}.wf")), content).expect("write wf file");
        dir
    }

    #[test]
    fn test_dispatch_list_workflows_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_list_workflows(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_dispatch_list_workflows_unknown_repo() {
        let (_f, db) = make_test_db();
        let result = tool_list_workflows(&db, &args_with("repo", "ghost-repo"));
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_list_workflows_includes_input_schema() {
        use conductor_core::config::load_config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let wf_content = r#"
workflow deploy {
    meta { description = "Deploy to production" trigger = "manual" targets = ["worktree"] }
    inputs {
        env required description = "Target environment"
        dry_run default = "false" description = "Skip actual deploy"
    }
    call deployer
}
"#;
        let wf_dir = make_wf_dir_with_workflow("deploy", wf_content);
        let repo_path = wf_dir.path().to_str().unwrap();

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            let config = load_config().expect("load config");
            RepoManager::new(&conn, &config)
                .register("my-repo", repo_path, "https://github.com/x/y", None)
                .expect("register repo");
        }

        let result = tool_list_workflows(&db, &args_with("repo", "my-repo"));
        assert_ne!(
            result.is_error,
            Some(true),
            "should succeed; got: {result:?}"
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");

        assert!(text.contains("name: deploy"), "missing name; got: {text}");
        assert!(
            text.contains("description: Deploy to production"),
            "missing description; got: {text}"
        );
        assert!(
            text.contains("inputs:"),
            "missing inputs section; got: {text}"
        );
        assert!(text.contains("name: env"), "missing env input; got: {text}");
        assert!(
            text.contains("required: true"),
            "env should be required; got: {text}"
        );
        assert!(
            text.contains("description: Target environment"),
            "missing input description; got: {text}"
        );
        assert!(
            text.contains("name: dry_run"),
            "missing dry_run input; got: {text}"
        );
        assert!(
            text.contains("required: false"),
            "dry_run should not be required; got: {text}"
        );
        assert!(
            text.contains("default: false"),
            "missing default; got: {text}"
        );
        // Drop wf_dir after assertions so tempdir lives long enough.
        drop(wf_dir);
    }

    #[test]
    fn test_dispatch_list_workflows_description_only_input_is_required() {
        // Regression test: an input declared with only a description must remain required.
        use conductor_core::config::load_config;
        use conductor_core::db::open_database;
        use conductor_core::repo::RepoManager;

        let wf_content = r#"
workflow w {
    meta { description = "test" trigger = "manual" targets = ["worktree"] }
    inputs {
        ticket_id description = "The ticket to work on"
    }
    call agent
}
"#;
        let wf_dir = make_wf_dir_with_workflow("w", wf_content);
        let repo_path = wf_dir.path().to_str().unwrap();

        let (_f, db) = make_test_db();
        {
            let conn = open_database(&db).expect("open db");
            let config = load_config().expect("load config");
            RepoManager::new(&conn, &config)
                .register("my-repo", repo_path, "https://github.com/x/y", None)
                .expect("register repo");
        }

        let result = tool_list_workflows(&db, &args_with("repo", "my-repo"));
        assert_ne!(
            result.is_error,
            Some(true),
            "should succeed; got: {result:?}"
        );
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");

        assert!(
            text.contains("required: true"),
            "input with only a description must be required; got: {text}"
        );
        drop(wf_dir);
    }

    #[test]
    fn test_dispatch_run_workflow_missing_args() {
        let (_f, db) = make_test_db();
        // Missing both "workflow" and "repo"
        let result = tool_run_workflow(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_run_workflow_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_dispatch_run_workflow_inputs_as_object() {
        let (_f, db) = make_test_db();
        let mut inputs_map = serde_json::Map::new();
        inputs_map.insert("key1".to_string(), Value::String("val1".to_string()));
        inputs_map.insert("key2".to_string(), Value::String("val2".to_string()));
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("inputs".to_string(), Value::Object(inputs_map));
        // Should fail at repo lookup, not at inputs parsing
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            !content.contains("inputs must be an object"),
            "Should not fail on inputs parsing"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_inputs_as_string_fails() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert(
            "inputs".to_string(),
            Value::String(r#"{"key":"val"}"#.to_string()),
        );
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            content.contains("inputs must be an object"),
            "Should fail with inputs type error"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_inputs_non_string_value_fails() {
        let (_f, db) = make_test_db();
        let mut inputs_map = serde_json::Map::new();
        inputs_map.insert("count".to_string(), Value::Number(42.into()));
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("inputs".to_string(), Value::Object(inputs_map));
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            content.contains("inputs.count must be a string value"),
            "Should fail with per-key type error"
        );
    }

    #[test]
    fn test_run_workflow_response_format_includes_status_pending() {
        // Verify the response format string includes `status: pending`.
        // This guards against accidental removal of the field without needing
        // a full end-to-end workflow execution.
        let workflow_name = "my-wf";
        let run_id = "01HXXXXXXXXXXXXXXXXXXXXXXX";
        let dry_run = false;
        let response = format!(
            "Workflow '{workflow_name}' started.\nrun_id: {run_id}\nstatus: pending\ndry_run: {dry_run}\nPoll progress with conductor_get_run."
        );
        assert!(
            response.contains("status: pending"),
            "response must include status field: {response}"
        );
        assert!(
            response.contains(&format!("run_id: {run_id}")),
            "response must include run_id: {response}"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_dry_run_flag_parsed() {
        let (_f, db) = make_test_db();
        // dry_run: true should be accepted and reach the repo lookup (not fail on arg parsing)
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("dry_run".to_string(), Value::Bool(true));
        let result = tool_run_workflow(&db, &args);
        // Should fail at repo lookup (ghost-repo not registered), not at arg parsing
        assert_eq!(result.is_error, Some(true));
        let content = format!("{result:?}");
        assert!(
            !content.contains("dry_run"),
            "Should not fail on dry_run parsing; got: {content}"
        );

        // dry_run as a non-boolean (string) should be tolerated via unwrap_or(false)
        let mut args2 = serde_json::Map::new();
        args2.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args2.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args2.insert("dry_run".to_string(), Value::String("true".to_string()));
        let result2 = tool_run_workflow(&db, &args2);
        assert_eq!(result2.is_error, Some(true));
        let content2 = format!("{result2:?}");
        assert!(
            !content2.contains("dry_run"),
            "Non-boolean dry_run should be ignored (unwrap_or(false)); got: {content2}"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_pr_and_worktree_mutually_exclusive() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("pr".to_string(), Value::String("123".to_string()));
        args.insert(
            "worktree".to_string(),
            Value::String("feat-something".to_string()),
        );
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("mutually exclusive"),
            "expected 'mutually exclusive' error, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_pr_invalid_format() {
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert(
            "pr".to_string(),
            Value::String("not-a-pr-number-or-url".to_string()),
        );
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(
            text.contains("Invalid pr value"),
            "expected parse error, got: {text}"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_pr_number_valid_parse() {
        // A valid numeric string should pass parse and reach repo lookup (not parse error).
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert("pr".to_string(), Value::String("42".to_string()));
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        // Must fail at repo lookup (ghost-repo not found), not at PR parse.
        assert!(
            !text.contains("Invalid pr value"),
            "should not fail on PR parse; got: {text}"
        );
    }

    #[test]
    fn test_dispatch_run_workflow_pr_url_valid_parse() {
        // A full GitHub PR URL should be parsed and reach repo lookup (not parse error).
        let (_f, db) = make_test_db();
        let mut args = serde_json::Map::new();
        args.insert("workflow".to_string(), Value::String("my-wf".to_string()));
        args.insert("repo".to_string(), Value::String("ghost-repo".to_string()));
        args.insert(
            "pr".to_string(),
            Value::String("https://github.com/owner/repo/pull/99".to_string()),
        );
        let result = tool_run_workflow(&db, &args);
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        // Must fail at repo lookup (ghost-repo not found), not at PR URL parse.
        assert!(
            !text.contains("Invalid pr value"),
            "should not fail on PR URL parse; got: {text}"
        );
    }

    #[test]
    fn test_validate_workflow_missing_repo_arg() {
        let (_f, db) = make_test_db();
        let result = tool_validate_workflow(&db, &empty_args());
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_validate_workflow_missing_workflow_arg() {
        let (_f, db) = make_test_db();
        let result = tool_validate_workflow(&db, &args_with("repo", "my-repo"));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.as_str())
            .unwrap_or("");
        assert!(text.contains("Missing required argument"), "got: {text}");
    }

    #[test]
    fn test_validate_workflow_unknown_repo() {
        let (_f, db) = make_test_db();
        let mut a = empty_args();
        a.insert("repo".into(), Value::String("ghost-repo".into()));
        a.insert("workflow".into(), Value::String("deploy".into()));
        let result = tool_validate_workflow(&db, &a);
        assert_eq!(result.is_error, Some(true));
    }
}
