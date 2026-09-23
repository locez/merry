//! Deterministic token estimates used by request budgeting and compaction planning.

use merry_llm::{ModelContent, ModelInputItem, ModelRequest, ModelResponseFormat};

mod calibration;

pub(crate) use calibration::{
    RequestTokenCalibration, RequestTokenObservation, TokenEstimateScale,
};

/// Estimates the complete input, including tools and response schemas, before calibration.
pub(crate) fn estimate_request_input_tokens(request: &ModelRequest) -> u64 {
    estimate_model_input_tokens(request.input())
        .saturating_add(estimate_request_contract_tokens(request))
}

/// Estimates provider-visible tools and response schemas in addition to messages.
pub(crate) fn estimate_request_contract_tokens(request: &ModelRequest) -> u64 {
    let tools = request
        .tools()
        .iter()
        .map(|tool| {
            estimate_text_tokens(tool.name().as_str())
                + estimate_text_tokens(tool.description())
                + estimate_text_tokens(&tool.input_schema().as_schema().as_value().to_string())
        })
        .sum::<u64>();
    let format = match request.response_format() {
        Some(ModelResponseFormat::StructuredOutput(format)) => {
            estimate_text_tokens(&format.schema().as_value().to_string())
        }
        None => 0,
    };
    tools.saturating_add(format)
}

/// Bytes per token used by the deterministic base text estimate.
///
/// Primary request budgets and destination-history planning apply session-owned
/// usage calibration on top. Independent compactor requests do not inherit that
/// model's multiplier. This remains distinct from compaction's accepted-output
/// byte ceiling ([`crate::compaction`]), which adds slack so a checkpoint that
/// fits the token budget is not rejected on byte count.
pub(crate) const BYTES_PER_TOKEN: u64 = 4;

pub(crate) fn estimate_model_input_tokens(input: &[ModelInputItem]) -> u64 {
    input.iter().map(estimate_model_input_item_tokens).sum()
}

pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    estimate_utf8_tokens(u64::try_from(text.len()).unwrap_or(u64::MAX))
}

pub(crate) const fn estimate_utf8_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

fn estimate_model_input_item_tokens(item: &ModelInputItem) -> u64 {
    match item {
        ModelInputItem::Message(message) => estimate_model_content_tokens(message.content()),
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

fn estimate_model_content_tokens(content: &ModelContent) -> u64 {
    content.images().fold(
        estimate_text_tokens(content.as_text()),
        |estimated_tokens, image| {
            let pixels = u64::from(image.width()) * u64::from(image.height());
            estimated_tokens.saturating_add(pixels.div_ceil(750).max(85))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{estimate_model_input_item_tokens, estimate_text_tokens};
    use merry_llm::{ModelContent, ModelImage, ModelInputItem, ModelMessage, ModelMessageRole};
    use std::sync::Arc;

    #[test]
    fn image_token_estimate_adds_each_image_to_the_text_projection() {
        let png = Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]);
        let content = ModelContent::user_with_images(
            "inspect [Image #1] and [Image #2]",
            vec![
                ModelImage::png("[Image #1]", Arc::clone(&png), 1, 1).expect("valid small image"),
                ModelImage::png("[Image #2]", png, 1_000, 1_000).expect("valid large image"),
            ],
        )
        .expect("valid image content");
        let text_tokens = estimate_text_tokens(content.as_text());
        let item = ModelInputItem::Message(
            ModelMessage::new(ModelMessageRole::User, content).expect("valid user message"),
        );

        assert_eq!(
            estimate_model_input_item_tokens(&item),
            text_tokens + 85 + 1_000_000_u64.div_ceil(750)
        );
    }
}
