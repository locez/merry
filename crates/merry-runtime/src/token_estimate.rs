//! Deterministic token estimates used by request budgeting and compaction planning.

use merry_llm::ModelInputItem;

pub(crate) fn estimate_model_input_tokens(input: &[ModelInputItem]) -> u64 {
    input.iter().map(estimate_model_input_item_tokens).sum()
}

pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4)).expect("usize should fit in u64 on supported targets")
}

fn estimate_model_input_item_tokens(item: &ModelInputItem) -> u64 {
    match item {
        ModelInputItem::Message(message) => estimate_text_tokens(message.content().as_text()),
        ModelInputItem::ToolCall(call) => {
            estimate_text_tokens(call.name().as_str())
                + estimate_text_tokens(
                    &serde_json::to_string(call.arguments().as_object())
                        .expect("tool arguments must serialize for budget estimation"),
                )
        }
        ModelInputItem::ToolResult(result) => estimate_text_tokens(result.content().as_str()),
    }
}
