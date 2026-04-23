use std::path::PathBuf;
use std::sync::Arc;

use crate::dsl::parse_workflow_file;
use crate::dsl::WorkflowDef;
use crate::engine_error::EngineError;
use crate::traits::workflow_resolver::WorkflowResolver;

pub struct DirectoryWorkflowResolver {
    root: PathBuf,
}

impl DirectoryWorkflowResolver {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl WorkflowResolver for DirectoryWorkflowResolver {
    fn resolve(&self, name: &str) -> Result<Arc<WorkflowDef>, EngineError> {
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err(EngineError::WorkflowNotFound(name.to_string()));
        }
        let path = self.root.join(format!("{name}.wf"));
        if !path.exists() {
            return Err(EngineError::WorkflowNotFound(name.to_string()));
        }
        parse_workflow_file(&path)
            .map(Arc::new)
            .map_err(EngineError::Workflow)
    }
}
