use conductor_core::{config::Config, db::open_database, Conductor};

pub(super) fn make_test_conductor() -> (tempfile::NamedTempFile, Conductor) {
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
