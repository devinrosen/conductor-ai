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
