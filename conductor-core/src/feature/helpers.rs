use crate::git::git_in;

use super::types::Feature;

// ---------------------------------------------------------------------------
// Git timestamp helpers
// ---------------------------------------------------------------------------

/// Run `git log -1 --format=%cI <branch>` and return the committer timestamp,
/// or `None` if the branch is not reachable locally.
pub(super) fn last_commit_timestamp(repo_path: &str, branch: &str) -> Option<String> {
    match git_in(repo_path)
        .args(["log", "-1", "--format=%cI", branch])
        .output()
    {
        Ok(output) if output.status.success() => {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        _ => None,
    }
}

/// Fetch committer dates for all local branches in a single subprocess call.
/// Returns a map from short branch name to ISO 8601 timestamp.
pub(super) fn batch_branch_timestamps(
    repo_path: &str,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let output = git_in(repo_path)
        .args([
            "for-each-ref",
            "--format=%(refname:short) %(committerdate:iso-strict)",
            "refs/heads/",
        ])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if let Some((branch, ts)) = line.split_once(' ') {
                    if !ts.is_empty() {
                        map.insert(branch.to_string(), ts.to_string());
                    }
                }
            }
        }
    }
    map
}

pub(super) fn map_feature_row(row: &rusqlite::Row) -> rusqlite::Result<Feature> {
    Ok(Feature {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        name: row.get(2)?,
        branch: row.get(3)?,
        base_branch: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
        merged_at: row.get(7)?,
        source_type: row.get(8)?,
        source_id: row.get(9)?,
        tickets_total: row.get::<_, i64>(10).and_then(|v| {
            u32::try_from(v).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, v))
        })?,
        tickets_merged: row.get::<_, i64>(11).and_then(|v| {
            u32::try_from(v).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, v))
        })?,
    })
}

/// Derive a git branch name from a feature name.
/// Names containing `/` are used as-is; otherwise `feat/` is prepended.
pub(super) fn derive_branch_name(name: &str) -> String {
    if name.contains('/') {
        name.to_string()
    } else {
        format!("feat/{name}")
    }
}

/// Derive a feature name from a branch name (inverse of `derive_branch_name`).
///
/// Strips `feat/` and `fix/` prefixes; leaves everything else as-is.
pub fn branch_to_feature_name(branch: &str) -> &str {
    branch
        .strip_prefix("feat/")
        .or_else(|| branch.strip_prefix("fix/"))
        .unwrap_or(branch)
}
