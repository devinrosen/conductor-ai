mod action;
mod app;
mod background;
mod event;
mod input;
mod state;
mod theme;
mod ui;

use anyhow::Result;

use conductor_core::config::{db_path, ensure_dirs, load_config};
use conductor_core::db::open_database;
use theme::Theme;

fn main() -> Result<()> {
    // Install panic hook that restores terminal before printing panic info
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let config = load_config()?;
    ensure_dirs(&config)?;

    let theme = if let Some(raw_path) = config.general.theme_path.as_deref() {
        let expanded = if raw_path.starts_with('~') {
            let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
            home.join(raw_path.trim_start_matches("~/").trim_start_matches('~'))
        } else {
            std::path::PathBuf::from(raw_path)
        };
        Theme::from_base16_file(&expanded).unwrap_or_else(|e| {
            eprintln!("theme error: {e}");
            std::process::exit(1);
        })
    } else {
        match config.general.theme.as_deref() {
            Some(name) => Theme::from_name(name).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            }),
            None => Theme::default(),
        }
    };

    let db = db_path();
    let conn = open_database(&db)?;

    let mut terminal = ratatui::init();
    let result = app::App::new(conn, config, theme).run(&mut terminal);
    ratatui::restore();

    result
}
