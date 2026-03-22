/// Given a flat list sorted by group (get_group returns a comparable key),
/// return the visual row index (including interleaved header rows) for
/// the item at `logical_idx`.
pub fn visual_idx_with_headers<T, K: PartialEq + Default>(
    items: &[T],
    get_group: impl Fn(&T) -> K,
    logical_idx: usize,
) -> usize {
    let mut headers = 0usize;
    let mut prev = K::default();
    for item in items.iter().take(logical_idx + 1) {
        let group = get_group(item);
        if group != prev {
            headers += 1;
            prev = group;
        }
    }
    logical_idx + headers
}

/// Replace absolute worktree and home-directory paths with short placeholders.
pub fn shorten_paths(summary: &str, worktree_path: &str, home_dir: Option<&str>) -> String {
    // Replace worktree path first (more specific), then home dir (less specific)
    let s = if !worktree_path.is_empty() {
        summary.replacen(worktree_path, "{worktree}", 1)
    } else {
        summary.to_string()
    };
    match home_dir {
        Some(home) => s.replacen(home, "~", 1),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::workflow::Condition;

    // ── visual_idx_with_headers ─────────────────────────────────────────

    #[test]
    fn visual_idx_empty_list() {
        let items: Vec<i32> = vec![];
        // With an empty list, visual_idx_with_headers shouldn't be called,
        // but if called with idx=0, it should handle gracefully
        assert_eq!(visual_idx_with_headers(&items, |_: &i32| 0, 0), 0);
    }

    #[test]
    fn visual_idx_single_group() {
        // All items in the same group: one header is added
        let items = vec![1, 2, 3];
        // Group key: all items → group 1
        assert_eq!(visual_idx_with_headers(&items, |_| 1, 0), 1); // header + idx 0
        assert_eq!(visual_idx_with_headers(&items, |_| 1, 1), 2); // header + idx 1
        assert_eq!(visual_idx_with_headers(&items, |_| 1, 2), 3); // header + idx 2
    }

    #[test]
    fn visual_idx_two_groups() {
        // Items: [A, A, B, B] — two group headers
        let items = vec![1, 1, 2, 2];
        let get_group = |x: &i32| *x;
        assert_eq!(visual_idx_with_headers(&items, get_group, 0), 1); // header(A) + 0
        assert_eq!(visual_idx_with_headers(&items, get_group, 1), 2); // header(A) + 1
        assert_eq!(visual_idx_with_headers(&items, get_group, 2), 4); // header(A) + 2 items + header(B) + 0
        assert_eq!(visual_idx_with_headers(&items, get_group, 3), 5); // header(A) + 2 + header(B) + 1
    }

    // ── shorten_paths ───────────────────────────────────────────────────

    #[test]
    fn shorten_paths_replaces_worktree() {
        let result = shorten_paths(
            "Edited /home/user/my-app/.worktrees/feat-x/src/main.rs",
            "/home/user/my-app/.worktrees/feat-x",
            None,
        );
        assert_eq!(result, "Edited {worktree}/src/main.rs");
    }

    #[test]
    fn shorten_paths_replaces_home() {
        let result = shorten_paths(
            "File at /home/user/.config/settings.json",
            "",
            Some("/home/user"),
        );
        assert_eq!(result, "File at ~/.config/settings.json");
    }

    #[test]
    fn shorten_paths_both() {
        let result = shorten_paths(
            "Edited /home/user/my-app/.worktrees/feat-x/lib.rs and /home/user/.config/foo",
            "/home/user/my-app/.worktrees/feat-x",
            Some("/home/user"),
        );
        assert!(result.contains("{worktree}/lib.rs"));
        assert!(result.contains("~/.config/foo"));
    }

    #[test]
    fn shorten_paths_empty_worktree_path() {
        let result = shorten_paths("no change", "", None);
        assert_eq!(result, "no change");
    }

    // ── format_condition ────────────────────────────────────────────────

    #[test]
    fn format_condition_step_marker() {
        let c = Condition::StepMarker {
            step: "build".into(),
            marker: "success".into(),
        };
        assert_eq!(format_condition(&c), "build.success");
    }

    #[test]
    fn format_condition_bool_input() {
        let c = Condition::BoolInput {
            input: "dry_run".into(),
        };
        assert_eq!(format_condition(&c), "dry_run");
    }
}

/// Format a workflow condition for display. Uses `step.marker` notation for
/// step-marker conditions and the bare input name for boolean inputs.
pub fn format_condition(c: &conductor_core::workflow::Condition) -> String {
    match c {
        conductor_core::workflow::Condition::StepMarker { step, marker } => {
            format!("{step}.{marker}")
        }
        conductor_core::workflow::Condition::BoolInput { input } => input.clone(),
    }
}
