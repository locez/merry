use merry_llm::{ModelName, ModelProvider};
use merry_runtime::RuntimeModelRole;
use std::sync::Arc;

pub(crate) struct RuntimeRoleProviderConfig {
    pub(crate) role: RuntimeModelRole,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
}
