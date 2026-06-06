use crate::config::MerryConfig;
use crate::{CliError, configured_runtime_builder, usage_error, write_runtime_step_events};
use merry_core::SessionId;
use merry_runtime::{StepContext, StepInput};

pub(crate) async fn run(
    session_id: &str,
    input: &str,
    merry_config: Option<&MerryConfig>,
) -> Result<(), CliError> {
    let session_id = SessionId::new(session_id).map_err(usage_error)?;
    let runtime = configured_runtime_builder(session_id, merry_config)?
        .build()
        .map_err(crate::unexpected)?;
    let input = StepInput::user_text(input).map_err(usage_error)?;
    write_runtime_step_events(&runtime, input, StepContext::default(), tokio::io::stdout()).await
}
