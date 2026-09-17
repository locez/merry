//! The stable prefix a compaction request shares with the agent loop.

use super::RuntimeInner;
use crate::{
    RuntimeError,
    step::{StablePrefixParts, compile_stable_prefix_items},
};
use merry_llm::ModelInputItem;
pub(in crate::runtime) async fn compaction_stable_prefix(
    inner: &RuntimeInner,
) -> Result<Vec<ModelInputItem>, RuntimeError> {
    let (skill_catalog, project_rules) = {
        let session = inner.session.lock().await;
        (session.skill_catalog(), session.project_rules())
    };
    compile_stable_prefix_items(StablePrefixParts {
        prompt_profile: &inner.prompt_profile,
        progress_commentary: inner.progress_commentary,
        skill_catalog: skill_catalog.as_ref(),
        project_rules: project_rules.as_ref(),
    })
    .map_err(|error| RuntimeError::CompactionModelRequest {
        message: error.to_string(),
    })
}
