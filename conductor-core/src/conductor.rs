use std::path::PathBuf;

use rusqlite::Connection;

use crate::config::{db_path as config_db_path, ensure_dirs, load_config, Config};
use crate::db::{open_database, open_database_compat};
use crate::error::Result;

pub struct Conductor {
    pub conn: Connection,
    pub config: Config,
}

impl Conductor {
    pub fn open() -> Result<Self> {
        let config = load_config()?;
        ensure_dirs(&config)?;
        let conn = open_database(&config_db_path())?;
        Ok(Self { conn, config })
    }

    pub fn open_compat() -> Result<Self> {
        let config = load_config()?;
        ensure_dirs(&config)?;
        let conn = open_database_compat(&config_db_path())?;
        Ok(Self { conn, config })
    }

    pub fn db_path() -> PathBuf {
        config_db_path()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tempfile::NamedTempFile;

    use super::*;

    /// Serializes tests that mutate CONDUCTOR_DB_PATH to prevent races.
    static DB_PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn assert_conductor_functional(conductor: Conductor) {
        let n: i64 = conductor
            .conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 0);
    }

    fn with_tmp_db<F: FnOnce() -> Result<Conductor>>(opener: F, label: &str) {
        let _guard = DB_PATH_ENV_LOCK.lock().unwrap();
        let tmp = NamedTempFile::new().unwrap();
        unsafe {
            std::env::set_var("CONDUCTOR_DB_PATH", tmp.path());
        }
        let result = opener();
        unsafe {
            std::env::remove_var("CONDUCTOR_DB_PATH");
        }
        let conductor = result.unwrap_or_else(|e| panic!("{label} should succeed: {e}"));
        assert_conductor_functional(conductor);
    }

    #[test]
    fn open_success() {
        with_tmp_db(Conductor::open, "Conductor::open");
    }

    #[test]
    fn open_compat_success() {
        with_tmp_db(Conductor::open_compat, "Conductor::open_compat");
    }

    #[test]
    fn db_path_ends_with_conductor_db() {
        let _guard = DB_PATH_ENV_LOCK.lock().unwrap();
        let path = Conductor::db_path();
        assert_eq!(path.file_name().unwrap(), "conductor.db");
    }
}
