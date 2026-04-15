pub(crate) mod helpers;
pub(crate) mod manager;
pub(crate) mod types;

#[cfg(test)]
mod tests;

pub use helpers::branch_to_feature_name;
pub use manager::{build_milestone_source_id, FeatureManager};
pub use types::*;
