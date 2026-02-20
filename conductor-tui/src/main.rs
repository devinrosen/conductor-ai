mod action;
mod app;
mod background;
mod event;
mod input;
mod state;
mod ui;

use anyhow::Result;

use conductor_core::config::{db_path, ensure_dirs, load_config};
use conductor_core::db::open_database;

fn main() -> Result<()> {
    // Install panic hook that restores terminal before printing panic info
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let config = load_config()?;
    ensure_dirs(&config)?;

    let db = db_path();
    let conn = open_database(&db)?;

    let mut terminal = ratatui::init();
    let result = app::App::new(conn, config).run(&mut terminal);
    ratatui::restore();

    result
}
