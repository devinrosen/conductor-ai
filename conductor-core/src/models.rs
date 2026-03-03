use serde::Serialize;

/// Tier indicating model capability/cost tradeoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModelTier {
    /// Fast & cheap (★)
    Fast = 1,
    /// Balanced (★★)
    Balanced = 2,
    /// Most capable (★★★)
    Powerful = 3,
}

/// A known Claude model with metadata for display in pickers.
#[derive(Debug, Clone, Serialize)]
pub struct KnownModel {
    /// Full model ID (e.g. "claude-opus-4-6")
    pub id: &'static str,
    /// Short alias (e.g. "opus")
    pub alias: &'static str,
    /// Capability/cost tier
    pub tier: ModelTier,
    /// Human-readable description of best use cases
    pub description: &'static str,
}

impl KnownModel {
    /// Returns the tier as a star string for display (e.g. "★★★").
    pub fn tier_stars(&self) -> &'static str {
        match self.tier {
            ModelTier::Fast => "★",
            ModelTier::Balanced => "★★",
            ModelTier::Powerful => "★★★",
        }
    }

    /// Returns the tier label (e.g. "Fast", "Balanced", "Powerful").
    pub fn tier_label(&self) -> &'static str {
        match self.tier {
            ModelTier::Fast => "Fast",
            ModelTier::Balanced => "Balanced",
            ModelTier::Powerful => "Powerful",
        }
    }
}

/// Curated list of known Claude models, ordered by tier (powerful → fast).
pub const KNOWN_MODELS: &[KnownModel] = &[
    KnownModel {
        id: "claude-opus-4-6",
        alias: "opus",
        tier: ModelTier::Powerful,
        description: "Planning, architecture, complex analysis",
    },
    KnownModel {
        id: "claude-sonnet-4-6",
        alias: "sonnet",
        tier: ModelTier::Balanced,
        description: "General implementation (default)",
    },
    KnownModel {
        id: "claude-haiku-4-5-20251001",
        alias: "haiku",
        tier: ModelTier::Fast,
        description: "Commit messages, formatting, quick edits",
    },
];

/// Look up a known model by its ID or alias. Returns `None` for custom model strings.
pub fn find_known_model(id_or_alias: &str) -> Option<&'static KnownModel> {
    KNOWN_MODELS
        .iter()
        .find(|m| m.id == id_or_alias || m.alias == id_or_alias)
}

/// Keywords that suggest a fast/cheap model (haiku).
const HAIKU_KEYWORDS: &[&str] = &[
    "commit",
    "format",
    "lint",
    "rename",
    "typo",
    "bump version",
    "changelog",
    "formatting",
    "fix typo",
    "update version",
];

/// Keywords that suggest a powerful model (opus).
const OPUS_KEYWORDS: &[&str] = &[
    "plan",
    "architect",
    "design",
    "refactor",
    "analyze",
    "review",
    "implement",
    "rewrite",
    "migrate",
    "complex",
];

/// Suggest a model alias based on prompt text using keyword heuristics.
///
/// Returns the alias of the suggested model ("haiku", "opus", or "sonnet").
/// This is a pure function with no side effects.
pub fn suggest_model(prompt: &str) -> &'static str {
    let lower = prompt.to_lowercase();

    // Check haiku keywords first (cheap tasks)
    for kw in HAIKU_KEYWORDS {
        if lower.contains(kw) {
            return "haiku";
        }
    }

    // Check opus keywords (complex tasks)
    for kw in OPUS_KEYWORDS {
        if lower.contains(kw) {
            return "opus";
        }
    }

    // Default to sonnet
    "sonnet"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_models_count() {
        assert_eq!(KNOWN_MODELS.len(), 3);
    }

    #[test]
    fn test_known_models_order() {
        // Ordered powerful → fast
        assert_eq!(KNOWN_MODELS[0].alias, "opus");
        assert_eq!(KNOWN_MODELS[1].alias, "sonnet");
        assert_eq!(KNOWN_MODELS[2].alias, "haiku");
    }

    #[test]
    fn test_find_known_model_by_id() {
        let m = find_known_model("claude-sonnet-4-6").unwrap();
        assert_eq!(m.alias, "sonnet");
    }

    #[test]
    fn test_find_known_model_by_alias() {
        let m = find_known_model("opus").unwrap();
        assert_eq!(m.id, "claude-opus-4-6");
    }

    #[test]
    fn test_find_known_model_unknown() {
        assert!(find_known_model("gpt-4").is_none());
    }

    #[test]
    fn test_suggest_model_commit() {
        assert_eq!(
            suggest_model("write a commit message for these changes"),
            "haiku"
        );
    }

    #[test]
    fn test_suggest_model_format() {
        assert_eq!(suggest_model("format the code"), "haiku");
    }

    #[test]
    fn test_suggest_model_lint() {
        assert_eq!(suggest_model("fix lint errors"), "haiku");
    }

    #[test]
    fn test_suggest_model_plan() {
        assert_eq!(
            suggest_model("plan the architecture for authentication"),
            "opus"
        );
    }

    #[test]
    fn test_suggest_model_refactor() {
        assert_eq!(suggest_model("refactor the database module"), "opus");
    }

    #[test]
    fn test_suggest_model_implement() {
        assert_eq!(suggest_model("implement the new API endpoint"), "opus");
    }

    #[test]
    fn test_suggest_model_default() {
        assert_eq!(suggest_model("fix the login bug"), "sonnet");
    }

    #[test]
    fn test_suggest_model_empty() {
        assert_eq!(suggest_model(""), "sonnet");
    }

    #[test]
    fn test_suggest_model_case_insensitive() {
        assert_eq!(suggest_model("COMMIT message please"), "haiku");
        assert_eq!(suggest_model("PLAN the architecture"), "opus");
    }

    #[test]
    fn test_tier_stars() {
        assert_eq!(KNOWN_MODELS[0].tier_stars(), "★★★");
        assert_eq!(KNOWN_MODELS[1].tier_stars(), "★★");
        assert_eq!(KNOWN_MODELS[2].tier_stars(), "★");
    }

    #[test]
    fn test_tier_labels() {
        assert_eq!(KNOWN_MODELS[0].tier_label(), "Powerful");
        assert_eq!(KNOWN_MODELS[1].tier_label(), "Balanced");
        assert_eq!(KNOWN_MODELS[2].tier_label(), "Fast");
    }
}
