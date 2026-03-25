mod embed;
mod instantiate;
mod parser;
pub mod types;

pub use embed::{get_embedded_template, list_embedded_templates};
pub use instantiate::{
    build_instantiation_prompt, build_upgrade_prompt, extract_template_version, InstantiationPrompt,
};
pub use parser::parse_wft;
pub use types::{TemplateFrontmatter, WorkflowTemplate};
