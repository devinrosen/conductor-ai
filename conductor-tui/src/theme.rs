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
}
