use serde::{Deserialize, Serialize};

/// Metadata extracted from the YAML frontmatter of a `.wft` template file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateFrontmatter {
    /// Human-readable template name (e.g. "create-issue").
    pub name: String,
    /// One-line description of what the template does.
    pub description: String,
    /// Semver-style version string (e.g. "1.0.0").
    pub version: String,
    /// Target types this template applies to (e.g. ["repo"], ["worktree"]).
    #[serde(default)]
    pub target_types: Vec<String>,
    /// Free-form hints the agent should consider when customizing the template.
    #[serde(default)]
    pub hints: Vec<String>,
}

/// A parsed workflow template: frontmatter metadata + raw `.wf` body.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTemplate {
    /// Parsed frontmatter metadata.
    pub metadata: TemplateFrontmatter,
    /// The raw `.wf` DSL body (everything after the frontmatter).
    pub body: String,
}
