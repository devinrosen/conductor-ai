use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use conductor_core::workflow::WorkflowRunStatus;
use conductor_core::Conductor;

use crate::commands::DevCommands;

pub fn handle_dev(command: DevCommands) -> Result<()> {
    match command {
        DevCommands::Seed { reset } => {
            let db_path = Conductor::db_path();
            if reset && db_path.exists() {
                std::fs::remove_file(&db_path).context("failed to remove existing database")?;
                println!("Removed {}", db_path.display());
            }
            let conductor = Conductor::open()?;
            conductor_core::db::seed::seed_database(&conductor.conn)?;
            println!("Seeded database at {}", db_path.display());
        }
        DevCommands::Reinstall {
            check,
            force,
            web,
            path,
        } => reinstall(check, force, web, path)?,
        DevCommands::Processes => print_processes(),
    }
    Ok(())
}

/// Resolve and validate the conductor-ai checkout to install from.
fn resolve_checkout(path: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match path {
        Some(p) => p,
        None => {
            let out = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .context("failed to run git")?;
            if !out.status.success() {
                bail!("not inside a git checkout; pass --path <conductor-ai checkout>");
            }
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
        }
    };
    validate_checkout(&dir)?;
    Ok(dir)
}

fn validate_checkout(dir: &Path) -> Result<()> {
    for krate in ["conductor-cli", "conductor-tui"] {
        if !dir.join(krate).join("Cargo.toml").is_file() {
            bail!(
                "{} is not a conductor-ai checkout (missing {krate}/Cargo.toml)",
                dir.display()
            );
        }
    }
    Ok(())
}

type InstallStep = (PathBuf, Vec<String>);

/// The commands `reinstall` runs, as `(cwd, argv)` pairs.
fn install_commands(checkout: &Path, web: bool) -> Vec<InstallStep> {
    let cargo_install = |krate: &str| -> InstallStep {
        (
            checkout.to_path_buf(),
            ["cargo", "install", "--path", krate, "--locked", "--force"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
    };
    let mut steps = vec![
        cargo_install("conductor-cli"),
        cargo_install("conductor-tui"),
    ];
    if web {
        let frontend = checkout.join("conductor-web").join("frontend");
        steps.push((frontend.clone(), vec!["bun".into(), "install".into()]));
        steps.push((frontend, vec!["bun".into(), "run".into(), "build".into()]));
        steps.push(cargo_install("conductor-web"));
    }
    steps
}

fn git_output(checkout: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(checkout)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn print_build_gap(checkout: &Path) {
    let installed = conductor_core::build_info::get().git_sha;
    let head =
        git_output(checkout, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("Installed conductor: {}", conductor_core::long_version!());
    println!("Checkout HEAD:       {head}");
    if installed != "unknown" && installed != head {
        match git_output(
            checkout,
            &["rev-list", "--count", &format!("{installed}..HEAD")],
        ) {
            Some(n) => println!("  HEAD is {n} commit(s) ahead of the installed build"),
            None => println!("  installed commit {installed} not found in this checkout"),
        }
    }
    if let Some(n) = git_output(checkout, &["rev-list", "--count", "HEAD..origin/main"]) {
        println!("  origin/main is {n} commit(s) ahead of HEAD (no fetch was done)");
    }
    if let Ok(out) = Command::new("conductor-tui").arg("--version").output() {
        if out.status.success() {
            println!(
                "Installed TUI:       {}",
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
    }
}

fn reinstall(check: bool, force: bool, web: bool, path: Option<PathBuf>) -> Result<()> {
    let checkout = resolve_checkout(path)?;
    print_build_gap(&checkout);

    let conductor = Conductor::open()?;
    let active = conductor_core::workflow::list_active_workflow_runs(
        &conductor.conn,
        &[
            WorkflowRunStatus::Running,
            WorkflowRunStatus::Waiting,
            WorkflowRunStatus::Cancelling,
        ],
    )?;
    if !active.is_empty() {
        println!("\nActive workflow runs:");
        for r in &active {
            println!("  {}  {}  ({})", r.id, r.workflow_name, r.status);
        }
        if !check && !force {
            bail!(
                "{} workflow run(s) are active; reinstalling can break them. \
                 Wait for them or pass --force.",
                active.len()
            );
        }
    }

    if check {
        println!();
        print_processes();
        return Ok(());
    }

    for (cwd, argv) in install_commands(&checkout, web) {
        println!("\n$ {}   (in {})", argv.join(" "), cwd.display());
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&cwd)
            .status()
            .with_context(|| format!("failed to run {}", argv[0]))?;
        if !status.success() {
            bail!("`{}` failed ({status})", argv.join(" "));
        }
    }

    println!("\nInstall complete.");
    let stale = conductor_core::process_registry::stale_processes();
    match conductor_core::process_registry::format_stale_warning(&stale) {
        Some(w) => println!("Processes still running the old build:\n{w}"),
        None => println!("No running conductor processes need a restart."),
    }
    Ok(())
}

fn print_processes() {
    use conductor_core::process_registry::{disk_build_sha_for, list_live, stale_processes};
    let live = list_live();
    if live.is_empty() {
        println!("No registered conductor processes.");
        return;
    }
    let stale = stale_processes();
    println!(
        "{:<9} {:<8} {:<9} {:<9} {:<27} EXE",
        "KIND", "PID", "BUILD", "ON DISK", "STARTED"
    );
    for r in &live {
        let flag = if stale.iter().any(|s| s.record.pid == r.pid) {
            "  STALE"
        } else {
            ""
        };
        println!(
            "{:<9} {:<8} {:<9} {:<9} {:<27} {}{flag}",
            r.kind,
            r.pid,
            r.git_sha,
            disk_build_sha_for(r).as_deref().unwrap_or("-"),
            r.started_at,
            r.exe.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_commands_without_web() {
        let steps = install_commands(Path::new("/repo"), false);
        assert_eq!(steps.len(), 2);
        assert!(steps[0].1.contains(&"conductor-cli".to_string()));
        assert!(steps[1].1.contains(&"conductor-tui".to_string()));
        assert!(steps.iter().all(|(cwd, a)| cwd == Path::new("/repo")
            && a.contains(&"--locked".to_string())
            && a.contains(&"--force".to_string())));
    }

    #[test]
    fn install_commands_with_web() {
        let steps = install_commands(Path::new("/repo"), true);
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[1].1[3], "conductor-tui");
        assert_eq!(steps[2].1, ["bun", "install"]);
        assert_eq!(steps[3].1, ["bun", "run", "build"]);
        assert!(steps[2].0.ends_with("conductor-web/frontend"));
        assert!(steps[4].1.contains(&"conductor-web".to_string()));
    }

    #[test]
    fn validate_checkout_rejects_other_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_checkout(dir.path()).is_err());
    }
}
