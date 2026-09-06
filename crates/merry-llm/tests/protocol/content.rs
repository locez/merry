use crate::assert_json_round_trip;
use merry_llm::{ModelContent, ModelContentPart, ModelImage, ModelMessage, ModelMessageRole};
use serde_json::json;
use std::sync::Arc;

#[test]
fn model_text_content_uses_validated_constructor_and_accessor() {
    let content = ModelContent::text("hello").expect("valid text content");

    assert_eq!(content.as_text(), "hello");
    assert_eq!(
        serde_json::to_value(&content).expect("content should serialize"),
        json!({ "type": "text", "text": "hello" })
    );

    let decoded =
        serde_json::from_value::<ModelContent>(json!({ "type": "text", "text": "hello" }))
            .expect("valid text content should deserialize");
    assert_eq!(decoded.as_text(), "hello");
}

#[test]
fn model_user_image_content_preserves_frame_and_text_order() {
    let image_1 = ModelImage::png(
        "[Image #1]",
        Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
        2,
        3,
    )
    .expect("valid first image");
    let image_2 = ModelImage::png(
        "[Image #2]",
        Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 1]),
        4,
        5,
    )
    .expect("valid second image");

    let content = ModelContent::user_with_images(
        "look at [Image #1] and [Image #2]",
        vec![image_1.clone(), image_2.clone()],
    )
    .expect("valid multimodal content");

    assert_eq!(
        content.parts(),
        [
            ModelContentPart::Text {
                text: "<image name=[Image #1]>".to_owned(),
            },
            ModelContentPart::Image { image: image_1 },
            ModelContentPart::Text {
                text: "</image>".to_owned(),
            },
            ModelContentPart::Text {
                text: "<image name=[Image #2]>".to_owned(),
            },
            ModelContentPart::Image { image: image_2 },
            ModelContentPart::Text {
                text: "</image>".to_owned(),
            },
            ModelContentPart::Text {
                text: "look at [Image #1] and [Image #2]".to_owned(),
            },
        ]
    );
    assert_eq!(content.images().count(), 2);
    assert!(content.has_images());
    assert_eq!(
        content.as_text(),
        "<image name=[Image #1]></image><image name=[Image #2]></image>look at [Image #1] and [Image #2]"
    );
}

#[test]
fn model_user_image_content_round_trips_provider_neutral_png_parts() {
    let content = ModelContent::user_with_images(
        "inspect [Image #1]",
        vec![
            ModelImage::png(
                "[Image #1]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 42]),
                7,
                9,
            )
            .expect("valid image"),
        ],
    )
    .expect("valid multimodal content");

    let encoded = serde_json::to_value(&content).expect("content should serialize");
    assert_eq!(encoded["type"], json!("parts"));
    assert_eq!(encoded["parts"][1]["type"], json!("image"));
    assert_eq!(encoded["parts"][1]["image"]["label"], json!("[Image #1]"));
    assert_eq!(encoded["parts"][1]["image"]["width"], json!(7));
    assert_eq!(encoded["parts"][1]["image"]["height"], json!(9));
    assert_eq!(
        encoded["parts"][1]["image"]["png_bytes"],
        json!([137, 80, 78, 71, 13, 10, 26, 10, 42])
    );
    assert_json_round_trip(&content);
}

#[test]
fn model_image_rejects_invalid_normalized_png_values() {
    assert!(
        ModelImage::png(
            "",
            Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
            1,
            1
        )
        .is_err()
    );
    assert!(ModelImage::png("[Image #1]", Arc::<[u8]>::from([]), 1, 1).is_err());
    assert!(ModelImage::png("[Image #1]", Arc::<[u8]>::from([1, 2, 3]), 1, 1).is_err());
    assert!(
        ModelImage::png(
            "[Image #1]",
            Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
            0,
            1
        )
        .is_err()
    );
    assert!(
        ModelImage::png(
            "[Image #1]",
            Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
            1,
            0
        )
        .is_err()
    );
}

#[test]
fn model_message_allows_images_only_for_user_role() {
    for role in [ModelMessageRole::System, ModelMessageRole::Assistant] {
        let content = ModelContent::user_with_images(
            "inspect [Image #1]",
            vec![
                ModelImage::png(
                    "[Image #1]",
                    Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
                    1,
                    1,
                )
                .expect("valid image"),
            ],
        )
        .expect("valid multimodal content");

        let error = ModelMessage::new(role, content).expect_err("role should reject image input");
        assert!(error.to_string().contains("user role"));
    }
}

#[test]
fn text_content_allows_multiline_compiled_context() {
    let content = ModelContent::text("First line\nSecond line").expect("multiline text is valid");
    assert_eq!(content.as_text(), "First line\nSecond line");
}
