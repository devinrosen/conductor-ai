//! Detection of "runaway placeholders" in agent briefs and prompt snippets.
//!
//! The engine's agent-prompt substitution takes each `{{`, finds the first
//! `}}` after it and treats everything between as one placeholder key. A key
//! that is not a known variable is removed from the prompt. So a literal `{{`
//! in prose (say, "if it still shows a literal `{{`") followed later by a real
//! `{{name}}` makes the engine read the whole span between them as one unknown
//! placeholder and drop it. This module finds those spans.

/// Whether `key` has the shape of a variable name (`name`, `step.field`, `a-b`).
pub(crate) fn is_variable_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// A `{{ … }}` span whose inside is not a variable name, which the engine
/// removes from the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunawayPlaceholder {
    /// 1-based line of the opening `{{`.
    pub start_line: usize,
    /// 1-based line of the closing `}}`.
    pub end_line: usize,
    /// Characters removed, delimiters included.
    pub dropped_chars: usize,
}

impl RunawayPlaceholder {
    /// The shared warning sentence, without a subject prefix.
    pub(crate) fn describe(&self) -> String {
        if self.start_line == self.end_line {
            format!(
                "`{{{{` on line {} opens a placeholder that is not a variable name; the engine \
                 removes it, dropping {} characters from the prompt. Write the literal another way",
                self.start_line, self.dropped_chars
            )
        } else {
            format!(
                "`{{{{` on line {} has no variable name after it; the engine reads lines {}-{} \
                 as one placeholder and removes it, dropping {} characters from the prompt. \
                 Write the literal another way",
                self.start_line, self.start_line, self.end_line, self.dropped_chars
            )
        }
    }
}

/// Start of the system-event summary recorded for a prompt warning. The adapter
/// writes events with it and `step-log` reads them back by it, so both build on
/// this one constant.
pub(crate) const PROMPT_WARNING_PREFIX: &str = "Prompt warning:";

/// One `{{ … }}` span as the engine's substitution sees it.
pub(crate) struct PlaceholderSpan<'a> {
    /// Byte offset of the opening `{{`.
    pub open: usize,
    /// Byte offset just past the closing `}}`.
    pub end: usize,
    /// Everything between the delimiters.
    pub key: &'a str,
}

/// Iterate the `{{ … }}` spans of `text` the way the engine does: the first
/// `}}` after a `{{`, no nesting; an unclosed `{{` ends the scan.
pub(crate) fn placeholder_spans(text: &str) -> impl Iterator<Item = PlaceholderSpan<'_>> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        let open = pos + text[pos..].find("{{")?;
        let key_start = open + 2;
        let close = key_start + text[key_start..].find("}}")?;
        pos = close + 2;
        Some(PlaceholderSpan {
            open,
            end: pos,
            key: &text[key_start..close],
        })
    })
}

/// Find every `{{ … }}` span the engine would remove although it is not a
/// variable name. Mirrors the engine's scan (see [`placeholder_spans`]); an
/// unclosed `{{` is kept verbatim and not reported.
pub(crate) fn runaway_placeholders(text: &str) -> Vec<RunawayPlaceholder> {
    placeholder_spans(text)
        .filter(|span| {
            // `{{#if x}}`, `{{else}}` and `{{/if}}` are conditional tags the engine
            // resolves before substitution, not placeholders.
            let is_tag =
                span.key.starts_with('#') || span.key.starts_with('/') || span.key == "else";
            !is_variable_name(span.key) && !is_tag
        })
        .map(|span| {
            let start_line = text[..span.open].matches('\n').count() + 1;
            RunawayPlaceholder {
                start_line,
                end_line: start_line + span.key.matches('\n').count(),
                dropped_chars: text[span.open..span.end].chars().count(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn draft_whats_new_shape() {
        let text = "intro\nstill shows a literal `{{`, dry run.\nmore\nfetch {{feature_base_branch}} now\n";
        let hits = runaway_placeholders(text);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].start_line, 2);
        assert_eq!(hits[0].end_line, 4);
        assert!(hits[0].describe().contains("lines 2-4"));
    }

    #[test]
    fn spaced_name_is_reported() {
        let hits = runaway_placeholders("a {{ name }} b");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].dropped_chars, "{{ name }}".len());
        assert_eq!(hits[0].start_line, hits[0].end_line);
    }

    #[test]
    fn valid_names_are_not_reported() {
        assert!(runaway_placeholders("{{a.b-c}} and {{ticket_id}}").is_empty());
    }

    #[test]
    fn unclosed_open_is_not_reported() {
        assert!(runaway_placeholders("text {{ never closed").is_empty());
    }

    #[test]
    fn several_hits() {
        let hits = runaway_placeholders("{{ a }} y {{b c}}");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn conditional_tags_are_not_reported() {
        assert!(runaway_placeholders("{{#if x}}y{{else}}z{{/if}}").is_empty());
    }

    #[test]
    fn counts_chars_not_bytes() {
        let hits = runaway_placeholders("{{é ü}}");
        assert_eq!(hits[0].dropped_chars, 7);
    }

    #[test]
    fn matches_engine_substitution() {
        let text = "a `{{` b\nc\nd {{x}} e {{ y }} f {{open";
        let rendered = runkon_flow::prompt_builder::substitute_variables(text, &HashMap::new());
        let dropped: usize = runaway_placeholders(text)
            .iter()
            .map(|h| h.dropped_chars)
            .sum();
        // The first span runs from the literal `{{` through `{{x}}`, so every
        // removed character belongs to a reported span; the unclosed tail stays.
        assert_eq!(text.chars().count() - rendered.chars().count(), dropped);
    }
}
