use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // In a worktree `.git` is a file, so `.git/HEAD` does not exist. Resolve the
    // real git dir (per-worktree HEAD) and the common dir (shared refs).
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]).map(PathBuf::from) {
        let common_dir = git(&["rev-parse", "--git-common-dir"])
            .map(PathBuf::from)
            .unwrap_or_else(|| git_dir.clone());
        let head = git_dir.join("HEAD");
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(contents) = std::fs::read_to_string(&head) {
            if let Some(branch_ref) = contents.trim().strip_prefix("ref: ") {
                let ref_path = common_dir.join(branch_ref);
                if ref_path.exists() {
                    println!("cargo:rerun-if-changed={}", ref_path.display());
                }
            }
        }
        println!(
            "cargo:rerun-if-changed={}",
            common_dir.join("packed-refs").display()
        );
    }

    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let commit_time = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CONDUCTOR_GIT_SHA={sha}");
    println!("cargo:rustc-env=CONDUCTOR_GIT_COMMIT_TIME={commit_time}");
}
