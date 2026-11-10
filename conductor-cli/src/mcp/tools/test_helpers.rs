use conductor_core::{config::Config, db::open_database, Conductor};

pub(crate) fn make_test_conductor() -> (tempfile::NamedTempFile, Conductor) {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let path = file.path().to_path_buf();
    let conn = open_database(&path).expect("open_database");
    (
        file,
        Conductor {
            conn,
            config: Config::default(),
        },
    )
}

/// Create an agent run plus a workflow run named `name`, optionally linked to a
/// worktree and repo. Returns the workflow run id.
pub(crate) fn create_list_run(
    conn: &rusqlite::Connection,
    name: &str,
    worktree_id: Option<&str>,
    repo_id: Option<&str>,
) -> String {
    let parent = conductor_core::agent::AgentManager::new(conn)
        .create_run(None, "workflow", None)
        .expect("create agent run");
    conductor_core::workflow::create_workflow_run_with_targets(
        conn,
        name,
        worktree_id,
        None,
        repo_id,
        &parent.id,
        false,
        "manual",
        None,
        None,
        None,
    )
    .expect("create workflow run")
    .id
    .clone()
}
