//! Tauri command bridge for the desktop app.
//!
//! The desktop app embeds the full conductor-web axum server on a random
//! localhost port. The only Tauri IPC command needed is `get_api_port` so the
//! frontend knows where to send `fetch()` and `EventSource` requests.

use tauri::State;

use crate::state::ApiPort;

/// Returns the port of the embedded HTTP API server so the frontend
/// can direct `fetch()` and `EventSource` to `http://localhost:{port}`.
#[tauri::command]
pub fn get_api_port(port: State<'_, ApiPort>) -> u16 {
    port.0
}

// ---------------------------------------------------------------------------
// macOS PATH fixup
// ---------------------------------------------------------------------------

/// On macOS, GUI apps don't inherit the user's shell PATH. This function
/// resolves common tool locations (homebrew, cargo) and prepends them.
pub fn fixup_macos_path() {
    if cfg!(target_os = "macos") {
        let cargo_bin = match std::env::var("HOME") {
            Ok(h) => format!("{h}/.cargo/bin"),
            Err(_) => String::new(),
        };
        let mut extra_paths: Vec<&str> = vec!["/opt/homebrew/bin", "/usr/local/bin"];
        if !cargo_bin.is_empty() {
            extra_paths.push(cargo_bin.as_str());
        }
        let current = std::env::var("PATH").unwrap_or_default();
        let mut parts: Vec<&str> = extra_paths.to_vec();
        if !current.is_empty() {
            parts.push(&current);
        }
        std::env::set_var("PATH", parts.join(":"));
    }
}
