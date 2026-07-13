use super::transcript::TranscriptItemId;
use merry_core::ArtifactId;

const ASSISTANT_OUTPUT_ARTIFACT_PREFIX: &str = "assistant-output-";
pub(crate) const CONTEXT_SEED_ARTIFACT_PREFIX: &str = "context-seed-";
const FINAL_OUTPUT_ARTIFACT_PREFIX: &str = "final-output-";
const PROCESS_INPUT_ARTIFACT_PREFIX: &str = "process-input-";
const PLAN_SUBAGENT_ARTIFACT_PREFIX: &str = "plan-subagent-artifact-";
const LEGACY_PLAN_ARTIFACT_PREFIX: &str = "plan-worker-artifact-";
const TOOL_RESULT_ARTIFACT_PREFIX: &str = "tool-result-";
const USER_MESSAGE_ARTIFACT_PREFIX: &str = "user-message-";

pub(crate) fn is_runtime_reserved_artifact_id(artifact_id: &ArtifactId) -> bool {
    artifact_id
        .as_str()
        .starts_with(ASSISTANT_OUTPUT_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(CONTEXT_SEED_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(PROCESS_INPUT_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(PLAN_SUBAGENT_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(LEGACY_PLAN_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(TOOL_RESULT_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(USER_MESSAGE_ARTIFACT_PREFIX)
}

pub(super) fn assistant_output_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{ASSISTANT_OUTPUT_ARTIFACT_PREFIX}{sequence}"))
        .expect("assistant output artifact id uses a valid static prefix and sequence")
}

pub(super) fn final_output_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{FINAL_OUTPUT_ARTIFACT_PREFIX}{sequence}"))
        .expect("final output artifact id uses a valid static prefix and sequence")
}

pub(super) fn process_input_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{PROCESS_INPUT_ARTIFACT_PREFIX}{sequence}"))
        .expect("process input artifact id uses a valid static prefix and sequence")
}

pub(super) fn tool_result_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{TOOL_RESULT_ARTIFACT_PREFIX}{sequence}"))
        .expect("tool result artifact id uses a valid static prefix and sequence")
}

pub(super) fn user_message_id(item_id: TranscriptItemId) -> ArtifactId {
    ArtifactId::new(&format!(
        "{USER_MESSAGE_ARTIFACT_PREFIX}{}",
        item_id.as_u64()
    ))
    .expect("user message artifact id uses a valid static prefix and transcript item id")
}

pub(super) fn user_message_image_id(item_id: TranscriptItemId, image_index: usize) -> ArtifactId {
    ArtifactId::new(&format!(
        "{USER_MESSAGE_ARTIFACT_PREFIX}{}-image-{image_index}",
        item_id.as_u64()
    ))
    .expect("user image artifact id uses a valid static prefix, transcript id, and image index")
}
