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
