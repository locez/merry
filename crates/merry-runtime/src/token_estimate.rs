//! Deterministic token estimates used by request budgeting and compaction planning.

use merry_llm::{ModelContent, ModelInputItem};

pub(crate) fn estimate_model_input_tokens(input: &[ModelInputItem]) -> u64 {
    input.iter().map(estimate_model_input_item_tokens).sum()
}

pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    u64::try_from(text.len().div_ceil(4)).expect("usize should fit in u64 on supported targets")
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
