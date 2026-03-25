use rust_embed::Embed;

use super::parser::parse_wft;
use super::types::WorkflowTemplate;

#[derive(Embed)]
#[folder = "templates/"]
#[include = "*.wft"]
struct TemplateAssets;

/// List all embedded workflow templates.
///
/// Templates that fail to parse are logged as warnings and skipped.
pub fn list_embedded_templates() -> Vec<WorkflowTemplate> {
    let mut templates = Vec::new();
    for file_name in <TemplateAssets as Embed>::iter() {
        let file_name_str = file_name.as_ref();
        if let Some(data) = <TemplateAssets as Embed>::get(file_name_str) {
            let content = match std::str::from_utf8(data.data.as_ref()) {
                Ok(s) => s.to_string(),
                Err(e) => {
                    tracing::warn!("Template {}: invalid UTF-8: {}", file_name_str, e);
                    continue;
                }
            };
            match parse_wft(&content, file_name_str) {
                Ok(tmpl) => templates.push(tmpl),
                Err(e) => {
                    tracing::warn!("Template {}: parse error: {}", file_name_str, e);
                }
            }
        }
    }
    templates.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
    templates
}

/// Get a single embedded template by name (O(1) asset lookup).
pub fn get_embedded_template(name: &str) -> Option<WorkflowTemplate> {
    // Try direct filename lookup first for O(1) fetch.
    let filename = format!("{name}.wft");
    if let Some(data) = <TemplateAssets as Embed>::get(&filename) {
        let content = std::str::from_utf8(data.data.as_ref()).ok()?;
        return parse_wft(content, &filename).ok();
    }
    // Fallback: scan all templates in case the filename doesn't match the name field.
    list_embedded_templates()
        .into_iter()
        .find(|t| t.metadata.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_embedded_templates_returns_vec() {
        // Should not panic; may be empty if no .wft files are present yet
        let templates = list_embedded_templates();
        // Each returned template should have non-empty metadata
        for t in &templates {
            assert!(!t.metadata.name.is_empty());
            assert!(!t.metadata.version.is_empty());
            assert!(!t.body.is_empty());
        }
    }

    #[test]
    fn test_get_nonexistent_template() {
        assert!(get_embedded_template("nonexistent-template-xyz").is_none());
    }
}
