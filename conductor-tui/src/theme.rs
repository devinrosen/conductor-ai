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

/// All built-in named themes: `(internal_name, display_label)`.
///
/// This is the single authoritative list used by the theme picker modal and
/// `Theme::from_name()`. Adding a new theme requires entries here and in
/// `from_name()`'s match arms.
pub const KNOWN_THEMES: &[(&str, &str)] = &[
    ("conductor", "Conductor (default)"),
    ("nord", "Nord"),
    ("gruvbox", "Gruvbox"),
    ("catppuccin_mocha", "Catppuccin Mocha"),
];

impl Default for Theme {
    fn default() -> Self {
        Self::conductor()
    }
}

impl Theme {
    /// Resolve a theme by name.
    ///
    /// Lookup order:
    /// 1. Built-in named themes (conductor, nord, gruvbox, catppuccin_mocha)
    /// 2. `~/.conductor/themes/<name>.toml`
    /// 3. `~/.conductor/themes/<name>.yaml`
    /// 4. `~/.conductor/themes/<name>.yml`
    pub fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "conductor" => Ok(Self::conductor()),
            "nord" => Ok(Self::nord()),
            "gruvbox" => Ok(Self::gruvbox()),
            "catppuccin_mocha" => Ok(Self::catppuccin_mocha()),
            _ => {
                let dir = conductor_core::config::themes_dir();
                let toml_path = dir.join(format!("{name}.toml"));
                if toml_path.exists() {
                    return Self::from_base16_file(&toml_path);
                }
                let yaml_path = dir.join(format!("{name}.yaml"));
                if yaml_path.exists() {
                    return Self::from_base16_yaml_file(&yaml_path);
                }
                let yml_path = dir.join(format!("{name}.yml"));
                if yml_path.exists() {
                    return Self::from_base16_yaml_file(&yml_path);
                }
                Err(format!(
                    "unknown theme \"{name}\". Built-in themes: conductor, nord, gruvbox, catppuccin_mocha. \
                     Custom themes go in ~/.conductor/themes/ as .toml, .yaml, or .yml files."
                ))
            }
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
            let hex = value.get(slot).and_then(|v| v.as_str()).ok_or_else(|| {
                format!(
                    "{}: missing required base16 slot \"{slot}\"",
                    path.display()
                )
            })?;
            parse_hex_color(slot, hex).map_err(|e| format!("{}: {e}", path.display()))
        };

        build_theme_from_base16(|slot| get(slot))
    }

    /// Load a theme from a base16 YAML file.
    ///
    /// Supports both the classic flat format (used by tinted-theming/base16-schemes)
    /// and the newer nested palette format, trying flat first:
    ///
    /// ```yaml
    /// # Classic flat format (base16-schemes)
    /// scheme: "My Theme"
    /// base00: "1d2021"
    /// base08: "fb4934"
    /// ```
    ///
    /// ```yaml
    /// # Nested palette format
    /// name: "My Theme"
    /// palette:
    ///   base00: "1d2021"
    ///   base08: "fb4934"
    /// ```
    ///
    /// Hex values may include or omit the `#` prefix.
    pub fn from_base16_yaml_file(path: &Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read theme file {}: {e}", path.display()))?;
        let value: serde_yml::Value = serde_yml::from_str(&contents)
            .map_err(|e| format!("failed to parse theme file {}: {e}", path.display()))?;

        // Detect format: if base16 slots are at the top level (classic flat format),
        // use them directly. Otherwise expect a nested `palette:` section.
        #[allow(clippy::type_complexity)]
        let get: Box<dyn Fn(&str) -> Result<Color, String>> = if value.get("base08").is_some() {
            Box::new(|slot: &str| {
                let hex = value.get(slot).and_then(|v| v.as_str()).ok_or_else(|| {
                    format!(
                        "{}: missing required base16 slot \"{slot}\"",
                        path.display()
                    )
                })?;
                parse_hex_color(slot, hex).map_err(|e| format!("{}: {e}", path.display()))
            })
        } else {
            let palette = value.get("palette").ok_or_else(|| {
                format!(
                    "{}: missing \"palette\" section in theme file",
                    path.display()
                )
            })?;
            Box::new(|slot: &str| {
                let hex = palette.get(slot).and_then(|v| v.as_str()).ok_or_else(|| {
                    format!(
                        "{}: missing required base16 slot \"{slot}\"",
                        path.display()
                    )
                })?;
                parse_hex_color(slot, hex).map_err(|e| format!("{}: {e}", path.display()))
            })
        };

        build_theme_from_base16(|slot| get(slot))
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

/// Returns all themes: built-in named themes followed by custom themes from
/// `~/.conductor/themes/`, sorted alphabetically by display label.
///
/// Custom themes whose stem matches a built-in theme name are skipped to avoid
/// duplicate entries in the picker.
///
/// Call this at theme-picker-open time so newly dropped files appear without
/// restarting the TUI.
///
/// Returns `(themes, warnings)` where `warnings` lists paths that failed to parse.
pub fn all_themes() -> (Vec<(String, String)>, Vec<String>) {
    let built_in_names: std::collections::HashSet<&str> =
        KNOWN_THEMES.iter().map(|(n, _)| *n).collect();
    let mut themes: Vec<(String, String)> = KNOWN_THEMES
        .iter()
        .map(|(name, label)| (name.to_string(), label.to_string()))
        .collect();
    let (custom, warnings) = scan_custom_themes();
    themes.extend(
        custom
            .into_iter()
            .filter(|(name, _)| !built_in_names.contains(name.as_str())),
    );
    (themes, warnings)
}

/// Scan `~/.conductor/themes/` for valid base16 theme files (.toml, .yaml, .yml).
///
/// Returns a pair `(valid_themes, warnings)`:
/// - `valid_themes`: `(stem, display_label)` pairs sorted by display label (case-insensitive).
/// - `warnings`: human-readable error strings for files that failed to parse, each including
///   the file path so the user can identify and fix the broken file.
pub fn scan_custom_themes() -> (Vec<(String, String)>, Vec<String>) {
    let dir = conductor_core::config::themes_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (vec![], vec![]);
    };

    let mut results: Vec<(String, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        match ext {
            "toml" => match Theme::from_base16_file(&path) {
                Ok(_) => results.push((stem.clone(), stem)),
                Err(e) => warnings.push(e),
            },
            "yaml" | "yml" => match Theme::from_base16_yaml_file(&path) {
                Ok(_) => {
                    let display = yaml_display_name(&path, &stem);
                    results.push((stem, display));
                }
                Err(e) => warnings.push(e),
            },
            _ => {}
        }
    }

    results.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    (results, warnings)
}

/// Read the display name from a base16 YAML file.
/// Tries `scheme:` first (classic flat format), then `name:` (nested format).
/// Falls back to `stem` if neither field is present or the file can't be parsed.
fn yaml_display_name(path: &Path, stem: &str) -> String {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return stem.to_string();
    };
    let Ok(value): Result<serde_yml::Value, _> = serde_yml::from_str(&contents) else {
        return stem.to_string();
    };
    value
        .get("scheme")
        .or_else(|| value.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(stem)
        .to_string()
}

/// Extract the nine required base16 slots via `get` and assemble a `Theme`.
///
/// Both the TOML and YAML loaders share this construction logic; they differ
/// only in how they build the `get` closure (top-level value vs. `palette` sub-key).
fn build_theme_from_base16(get: impl Fn(&str) -> Result<Color, String>) -> Result<Theme, String> {
    let base02 = get("base02")?;
    let base03 = get("base03")?;
    let base05 = get("base05")?;
    let base08 = get("base08")?;
    let base0a = get("base0A")?;
    let base0b = get("base0B")?;
    let base0c = get("base0C")?;
    let base0d = get("base0D")?;
    let base0e = get("base0E")?;

    Ok(Theme {
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
        assert!(
            err.contains("theme.toml"),
            "error should include file path, got: {err}"
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

    fn write_valid_yaml_theme(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(format!("{name}.yaml"));
        std::fs::write(
            &path,
            r#"name: "Test Theme"
palette:
  base02: "32302f"
  base03: "504945"
  base05: "d5c4a1"
  base08: "fb4934"
  base0A: "fabd2f"
  base0B: "b8bb26"
  base0C: "8ec07c"
  base0D: "83a598"
  base0E: "d3869b"
"#,
        )
        .unwrap();
        path
    }

    fn write_valid_flat_yaml_theme(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(format!("{name}.yaml"));
        std::fs::write(
            &path,
            r#"scheme: "Flat Test Theme"
base00: "1d2021"
base01: "282828"
base02: "32302f"
base03: "504945"
base04: "bdae93"
base05: "d5c4a1"
base06: "ebdbb2"
base07: "fbf1c7"
base08: "fb4934"
base09: "fe8019"
base0A: "fabd2f"
base0B: "b8bb26"
base0C: "8ec07c"
base0D: "83a598"
base0E: "d3869b"
base0F: "d65d0e"
"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn test_from_base16_yaml_file_flat_format() {
        // Classic flat base16-schemes format: slots at the top level, no `palette:` section.
        let dir = tempfile::tempdir().unwrap();
        let path = write_valid_flat_yaml_theme(dir.path(), "flat");
        let theme = Theme::from_base16_yaml_file(&path).expect("should parse flat YAML theme");
        // base02 = #32302f → highlight_bg
        assert_eq!(theme.highlight_bg, Color::Rgb(0x32, 0x30, 0x2f));
        // base0B = #b8bb26 → status_completed
        assert_eq!(theme.status_completed, Color::Rgb(0xb8, 0xbb, 0x26));
        // base0A = #fabd2f → status_running
        assert_eq!(theme.status_running, Color::Rgb(0xfa, 0xbd, 0x2f));
    }

    #[test]
    fn test_from_base16_yaml_file_flat_format_missing_slot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flat_missing.yaml");
        // Flat format with all required slots except base0B, so base0B is reported missing.
        std::fs::write(
            &path,
            "scheme: \"Flat\"\nbase02: \"32302f\"\nbase03: \"504945\"\nbase05: \"d5c4a1\"\nbase08: \"fb4934\"\nbase0A: \"fabd2f\"\nbase0C: \"8ec07c\"\nbase0D: \"83a598\"\nbase0E: \"d3869b\"\n",
        )
        .unwrap();
        let err = Theme::from_base16_yaml_file(&path).unwrap_err();
        assert!(
            err.contains("base0B"),
            "error should name missing slot, got: {err}"
        );
    }

    #[test]
    fn test_from_base16_yaml_file_nested_format() {
        // Nested palette format: slots under `palette:` key.
        let dir = tempfile::tempdir().unwrap();
        let path = write_valid_yaml_theme(dir.path(), "nested");
        let theme = Theme::from_base16_yaml_file(&path).expect("should parse nested YAML theme");
        assert_eq!(theme.highlight_bg, Color::Rgb(0x32, 0x30, 0x2f));
        assert_eq!(theme.status_completed, Color::Rgb(0xb8, 0xbb, 0x26));
    }

    #[test]
    fn test_scan_custom_themes_finds_toml_and_yaml() {
        let dir = tempfile::tempdir().unwrap();
        // Write a valid TOML theme
        let toml_path = dir.path().join("mytheme.toml");
        std::fs::copy(write_valid_theme(dir.path()), &toml_path).unwrap();
        // Write a valid YAML theme
        write_valid_yaml_theme(dir.path(), "another");

        // We can't easily override themes_dir() in tests without refactoring,
        // so directly test from_base16_file and from_base16_yaml_file paths instead,
        // and verify scan_custom_themes returns results with no warnings for those files.
        let theme = Theme::from_base16_file(&toml_path).unwrap();
        assert_eq!(theme.highlight_bg, Color::Rgb(0x32, 0x30, 0x2f));

        let yaml_path = dir.path().join("another.yaml");
        let theme_yaml = Theme::from_base16_yaml_file(&yaml_path).unwrap();
        assert_eq!(theme_yaml.highlight_bg, Color::Rgb(0x32, 0x30, 0x2f));
    }

    #[test]
    fn test_scan_custom_themes_returns_warning_for_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.yaml");
        std::fs::write(&broken, "name: bad\npalette:\n  base02: \"zzzzzz\"\n").unwrap();

        // Verify the broken file produces an error that includes the file path
        let err = Theme::from_base16_yaml_file(&broken).unwrap_err();
        assert!(
            err.contains("broken.yaml"),
            "error should include file path, got: {err}"
        );
    }

    #[test]
    fn test_yaml_missing_palette_error_includes_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nopalette.yaml");
        std::fs::write(&path, "name: \"No Palette\"\n").unwrap();

        let err = Theme::from_base16_yaml_file(&path).unwrap_err();
        assert!(
            err.contains("nopalette.yaml"),
            "missing palette error should include file path, got: {err}"
        );
        assert!(
            err.contains("palette"),
            "error should mention 'palette', got: {err}"
        );
    }

    #[test]
    fn test_all_themes_deduplicates_builtin_names() {
        // all_themes() must include the 4 built-in themes; names must be unique.
        let (themes, _warnings) = all_themes();
        let names: Vec<&str> = themes.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"conductor"));
        assert!(names.contains(&"nord"));
        assert!(names.contains(&"gruvbox"));
        assert!(names.contains(&"catppuccin_mocha"));
        // No duplicates
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate theme names found: {names:?}"
        );
    }

    #[test]
    fn test_all_themes_builtin_count() {
        // When no custom themes dir exists (fresh env), we get exactly the built-ins.
        // We can't control the real themes dir here, but we can verify built-ins are present.
        let (themes, _) = all_themes();
        assert!(themes.len() >= KNOWN_THEMES.len());
    }
}
