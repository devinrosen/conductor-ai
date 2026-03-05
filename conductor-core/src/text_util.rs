/// Truncate a string at a char boundary no greater than `max_bytes`.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backwards from max_bytes to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_str_multibyte() {
        assert_eq!(truncate_str("ééé", 3), "é"); // 3 < 4, backs up to 2
        assert_eq!(truncate_str("ééé", 4), "éé");

        assert_eq!(truncate_str("🦀x", 2), ""); // can't fit the crab
        assert_eq!(truncate_str("🦀x", 4), "🦀");
        assert_eq!(truncate_str("🦀x", 5), "🦀x");

        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 3), "hel");
    }
}
