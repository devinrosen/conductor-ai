use std::path::Path;

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Theme {
    pub border_focused: Color,   // Cyan      — active pane border
    pub border_inactive: Color,  // DarkGray  — inactive border
    pub status_running: Color,   // Yellow
    pub status_completed: Color, // Green
    pub status_failed: Color,    // Red
    pub status_waiting: Color,   // Magenta
    pub status_cancelled: Color, // DarkGray
    pub label_primary: Color,    // White     — workflow names, PR titles, bold identifiers
    pub label_secondary: Color,  // DarkGray  — timestamps, paths, muted info
    pub label_accent: Color,     // Cyan      — durations, step counts
    pub label_warning: Color,    // Yellow    — inputs badge, retries, running state
    pub label_error: Color,      // Red       — failures, error snippets
    pub label_info: Color,       // Cyan      — tool_use events, agent activity
    pub label_url: Color,        // Blue      — hyperlinks (ticket URLs, etc.)
    pub highlight_bg: Color,     // DarkGray  — selected row background
    pub group_header: Color,     // Yellow    — repo/section group headers
}

impl Default for Theme {
    fn default() -> Self {
        Self::conductor()
    }
}

impl Theme {
    /// Resolve a theme by name. Returns `Err` with a descriptive message listing
    /// valid names when the name is not recognized.
    pub fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "conductor" => Ok(Self::conductor()),
            "nord" => Ok(Self::nord()),
            "gruvbox" => Ok(Self::gruvbox()),
            "catppuccin_mocha" => Ok(Self::catppuccin_mocha()),
            _ => Err(format!(
                "unknown theme \"{name}\". Available themes: conductor, nord, gruvbox, catppuccin_mocha"
            )),
        }
    }

    /// Default conductor theme using terminal named colors.
    pub fn conductor() -> Self {
        Self {
            border_focused: Color::Cyan,
            border_inactive: Color::DarkGray,
            status_running: Color::Yellow,
            status_completed: Color::Green,
            status_failed: Color::Red,
            status_waiting: Color::Magenta,
            status_cancelled: Color::DarkGray,
            label_primary: Color::White,
            label_secondary: Color::DarkGray,
            label_accent: Color::Cyan,
            label_warning: Color::Yellow,
            label_error: Color::Red,
            label_info: Color::Cyan,
            label_url: Color::Blue,
            highlight_bg: Color::DarkGray,
            group_header: Color::Yellow,
        }
    }

    /// Nord — arctic blue palette (arcticzine.com/nord)
    pub fn nord() -> Self {
        Self {
            border_focused: Color::Rgb(0x88, 0xC0, 0xD0), // nord8  — cyan
            border_inactive: Color::Rgb(0x4C, 0x56, 0x6A), // nord3  — muted
            status_running: Color::Rgb(0xEB, 0xCB, 0x8B), // nord13 — yellow
            status_completed: Color::Rgb(0xA3, 0xBE, 0x8C), // nord14 — green
            status_failed: Color::Rgb(0xBF, 0x61, 0x6A),  // nord11 — red
            status_waiting: Color::Rgb(0xB4, 0x8E, 0xAD), // nord15 — purple
            status_cancelled: Color::Rgb(0x4C, 0x56, 0x6A), // nord3  — muted
            label_primary: Color::Rgb(0xEC, 0xEF, 0xF4),  // nord6  — bright fg
            label_secondary: Color::Rgb(0x4C, 0x56, 0x6A), // nord3  — muted
            label_accent: Color::Rgb(0x88, 0xC0, 0xD0),   // nord8  — cyan
            label_warning: Color::Rgb(0xEB, 0xCB, 0x8B),  // nord13 — yellow
            label_error: Color::Rgb(0xBF, 0x61, 0x6A),    // nord11 — red
            label_info: Color::Rgb(0x8F, 0xBC, 0xBB),     // nord7  — teal
            label_url: Color::Rgb(0x81, 0xA1, 0xC1),      // nord9  — blue
            highlight_bg: Color::Rgb(0x3B, 0x42, 0x52),   // nord1  — slightly lighter bg
            group_header: Color::Rgb(0x81, 0xA1, 0xC1),   // nord9  — blue
        }
    }

    /// Gruvbox Dark — warm amber/green palette (gruvbox.com)
    pub fn gruvbox() -> Self {
        Self {
            border_focused: Color::Rgb(0x83, 0xA5, 0x98), // bright blue
            border_inactive: Color::Rgb(0x66, 0x5C, 0x54), // bg3
            status_running: Color::Rgb(0xFA, 0xBD, 0x2F), // bright yellow
            status_completed: Color::Rgb(0xB8, 0xBB, 0x26), // bright green
            status_failed: Color::Rgb(0xFB, 0x49, 0x34),  // bright red
            status_waiting: Color::Rgb(0xD3, 0x86, 0x9B), // bright purple
            status_cancelled: Color::Rgb(0x66, 0x5C, 0x54), // bg3
            label_primary: Color::Rgb(0xEB, 0xDB, 0xB2),  // fg
            label_secondary: Color::Rgb(0x92, 0x83, 0x74), // gray
            label_accent: Color::Rgb(0x8E, 0xC0, 0x7C),   // bright aqua
            label_warning: Color::Rgb(0xFA, 0xBD, 0x2F),  // bright yellow
            label_error: Color::Rgb(0xFB, 0x49, 0x34),    // bright red
            label_info: Color::Rgb(0x83, 0xA5, 0x98),     // bright blue
            label_url: Color::Rgb(0x45, 0x85, 0x88),      // blue
            highlight_bg: Color::Rgb(0x50, 0x49, 0x45),   // bg2
            group_header: Color::Rgb(0xD7, 0x99, 0x21),   // yellow
        }
    }

    /// Load a theme from a base16 TOML file.
    ///
    /// The file must contain entries for the required base16 slots. Any missing
    /// slot or invalid hex value returns an `Err` naming the offending field.
    pub fn from_base16_file(path: &Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read theme file {}: {e}", path.display()))?;
        let value: toml::Value = toml::from_str(&contents)
            .map_err(|e| format!("failed to parse theme file {}: {e}", path.display()))?;

        let get = |slot: &str| -> Result<Color, String> {
            let hex = value
                .get(slot)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("missing required base16 slot \"{slot}\" in theme file"))?;
            parse_hex_color(slot, hex)
        };

        let base02 = get("base02")?;
        let base03 = get("base03")?;
        let base05 = get("base05")?;
        let base08 = get("base08")?;
        let base0a = get("base0A")?;
        let base0b = get("base0B")?;
        let base0c = get("base0C")?;
        let base0d = get("base0D")?;
        let base0e = get("base0E")?;

        Ok(Self {
            highlight_bg: base02,
            border_inactive: base03,
            status_cancelled: base03,
            label_secondary: base03,
            label_primary: base05,
            status_failed: base08,
            label_error: base08,
            status_running: base0a,
            label_warning: base0a,
            label_accent: base0a,
            status_completed: base0b,
            border_focused: base0c,
            group_header: base0c,
            label_info: base0d,
            label_url: base0d,
            status_waiting: base0e,
        })
    }

    /// Catppuccin Mocha — dark pastel palette (catppuccin.com)
    pub fn catppuccin_mocha() -> Self {
        Self {
            border_focused: Color::Rgb(0x89, 0xB4, 0xFA),   // Blue
            border_inactive: Color::Rgb(0x45, 0x47, 0x5A),  // Surface1
            status_running: Color::Rgb(0xF9, 0xE2, 0xAF),   // Yellow
            status_completed: Color::Rgb(0xA6, 0xE3, 0xA1), // Green
            status_failed: Color::Rgb(0xF3, 0x8B, 0xA8),    // Red
            status_waiting: Color::Rgb(0xCB, 0xA6, 0xF7),   // Mauve
            status_cancelled: Color::Rgb(0x45, 0x47, 0x5A), // Surface1
            label_primary: Color::Rgb(0xCD, 0xD6, 0xF4),    // Text
            label_secondary: Color::Rgb(0x6C, 0x70, 0x86),  // Overlay0
            label_accent: Color::Rgb(0x94, 0xE2, 0xD5),     // Teal
            label_warning: Color::Rgb(0xFA, 0xB3, 0x87),    // Peach
            label_error: Color::Rgb(0xF3, 0x8B, 0xA8),      // Red
            label_info: Color::Rgb(0x89, 0xDC, 0xEB),       // Sky
            label_url: Color::Rgb(0x74, 0xC7, 0xEC),        // Sapphire
            highlight_bg: Color::Rgb(0x31, 0x32, 0x44),     // Surface0
            group_header: Color::Rgb(0xB4, 0xBE, 0xFE),     // Lavender
        }
    }
}

/// Parse a 6-character hex color string (with optional `#` prefix) into `Color::Rgb`.
///
/// Returns a descriptive `Err` naming the offending slot on failure.
fn parse_hex_color(slot: &str, hex: &str) -> Result<Color, String> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return Err(format!(
            "{slot}: invalid hex '{hex}' (expected 6 hex digits)"
        ));
    }
    let r =
        u8::from_str_radix(&hex[0..2], 16).map_err(|_| format!("{slot}: invalid hex '{hex}'"))?;
    let g =
        u8::from_str_radix(&hex[2..4], 16).map_err(|_| format!("{slot}: invalid hex '{hex}'"))?;
    let b =
        u8::from_str_radix(&hex[4..6], 16).map_err(|_| format!("{slot}: invalid hex '{hex}'"))?;
    Ok(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_valid_theme(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("theme.toml");
        std::fs::write(
            &path,
            r##"
base00 = "#1d2021"
base01 = "#282828"
base02 = "#32302f"
base03 = "#504945"
base04 = "#bdae93"
base05 = "#d5c4a1"
base06 = "#ebdbb2"
base07 = "#fbf1c7"
base08 = "#fb4934"
base09 = "#fe8019"
base0A = "#fabd2f"
base0B = "#b8bb26"
base0C = "#8ec07c"
base0D = "#83a598"
base0E = "#d3869b"
base0F = "#d65d0e"
"##,
        )
        .unwrap();
        path
    }

    #[test]
    fn test_from_base16_file_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_valid_theme(dir.path());
        let theme = Theme::from_base16_file(&path).expect("should parse valid theme");
        // base02 = #32302f → highlight_bg
        assert_eq!(theme.highlight_bg, Color::Rgb(0x32, 0x30, 0x2f));
        // base0B = #b8bb26 → status_completed
        assert_eq!(theme.status_completed, Color::Rgb(0xb8, 0xbb, 0x26));
        // base0A = #fabd2f → status_running
        assert_eq!(theme.status_running, Color::Rgb(0xfa, 0xbd, 0x2f));
    }

    #[test]
    fn test_from_base16_file_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("theme.toml");
        // Omit base0B (required)
        std::fs::write(
            &path,
            r##"
base02 = "#32302f"
base03 = "#504945"
base05 = "#d5c4a1"
base08 = "#fb4934"
base0A = "#fabd2f"
base0C = "#8ec07c"
base0D = "#83a598"
base0E = "#d3869b"
"##,
        )
        .unwrap();
        let err = Theme::from_base16_file(&path).unwrap_err();
        assert!(
            err.contains("base0B"),
            "error should name missing slot, got: {err}"
        );
    }

    #[test]
    fn test_from_base16_file_invalid_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("theme.toml");
        std::fs::write(
            &path,
            r##"
base02 = "#32302f"
base03 = "#504945"
base05 = "#d5c4a1"
base08 = "gg0000"
base0A = "#fabd2f"
base0B = "#b8bb26"
base0C = "#8ec07c"
base0D = "#83a598"
base0E = "#d3869b"
"##,
        )
        .unwrap();
        let err = Theme::from_base16_file(&path).unwrap_err();
        assert!(
            err.contains("base08"),
            "error should name offending slot, got: {err}"
        );
        assert!(
            err.contains("gg0000"),
            "error should include bad hex value, got: {err}"
        );
    }
}
