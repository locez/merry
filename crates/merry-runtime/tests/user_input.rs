use merry_runtime::{
    MAX_USER_IMAGE_DIMENSION, MAX_USER_IMAGE_PIXELS, MAX_USER_IMAGE_PNG_BYTES,
    MAX_USER_IMAGE_TOTAL_PNG_BYTES, MAX_USER_IMAGES, RuntimeError, StepInput, UserImageInput,
    UserMessageInput, user_image_label,
};
use std::sync::Arc;

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn png_bytes(length: usize) -> Arc<[u8]> {
    assert!(length >= PNG_SIGNATURE.len());
    let mut bytes = vec![0; length];
    bytes[..PNG_SIGNATURE.len()].copy_from_slice(&PNG_SIGNATURE);
    bytes.into()
}

fn image(index: usize) -> UserImageInput {
    UserImageInput::png(
        &user_image_label(index).expect("positive image index"),
        png_bytes(PNG_SIGNATURE.len()),
        2,
        3,
    )
    .expect("valid test image")
}

#[test]
fn user_message_input_preserves_validated_images_and_text_compatibility() {
    let message =
        UserMessageInput::new("inspect [Image #1]", vec![image(1)]).expect("valid image message");
    let input = StepInput::from_user_messages([message.clone()]).expect("valid step input");

    assert_eq!(message.text(), "inspect [Image #1]");
    assert_eq!(message.images().len(), 1);
    assert_eq!(message.images()[0].label(), "[Image #1]");
    assert_eq!(message.images()[0].png_bytes(), PNG_SIGNATURE);
    assert_eq!(message.images()[0].width(), 2);
    assert_eq!(message.images()[0].height(), 3);
    assert_eq!(input.text(), "inspect [Image #1]");
    assert_eq!(input.texts(), ["inspect [Image #1]"]);
    assert_eq!(input.user_messages(), [message]);

    let text_only = StepInput::user_text("hello").expect("existing text constructor remains valid");
    assert!(text_only.user_messages()[0].images().is_empty());
}

#[test]
fn user_image_labels_are_one_based_sequential_and_present_in_text() {
    assert_eq!(user_image_label(1).expect("first label"), "[Image #1]");
    assert!(user_image_label(0).is_err());

    let out_of_order = UserMessageInput::new("inspect [Image #2]", vec![image(2)])
        .expect_err("first image must use the first label");
    assert!(matches!(
        out_of_order,
        RuntimeError::InvalidUserImageInput { .. }
    ));

    let missing = UserMessageInput::new("inspect this", vec![image(1)])
        .expect_err("text must contain the image label");
    assert!(matches!(
        missing,
        RuntimeError::InvalidUserImageInput { .. }
    ));
}

#[test]
fn user_message_input_enforces_image_count_limit() {
    let images = (1..=MAX_USER_IMAGES + 1).map(image).collect::<Vec<_>>();
    let text = (1..=MAX_USER_IMAGES + 1)
        .map(|index| user_image_label(index).expect("positive label index"))
        .collect::<Vec<_>>()
        .join(" ");

    let error = UserMessageInput::new(&text, images).expect_err("count over limit must reject");
    assert!(error.to_string().contains("at most 20 images"));
}

#[test]
fn user_image_input_enforces_single_and_total_png_byte_limits() {
    let single_error =
        UserImageInput::png("[Image #1]", png_bytes(MAX_USER_IMAGE_PNG_BYTES + 1), 1, 1)
            .expect_err("single encoded PNG over limit must reject");
    assert!(single_error.to_string().contains("10 MiB"));

    let shared = png_bytes(MAX_USER_IMAGE_TOTAL_PNG_BYTES / 3 + 1);
    let images = (1..=3)
        .map(|index| {
            UserImageInput::png(
                &user_image_label(index).expect("positive image index"),
                Arc::clone(&shared),
                1,
                1,
            )
            .expect("each image remains below the single-image limit")
        })
        .collect::<Vec<_>>();
    let error = UserMessageInput::new("[Image #1] [Image #2] [Image #3]", images)
        .expect_err("combined encoded PNG bytes over limit must reject");
    assert!(error.to_string().contains("20 MiB"));
}

#[test]
fn user_image_input_enforces_dimension_and_pixel_limits() {
    let dimension_error = UserImageInput::png(
        "[Image #1]",
        png_bytes(PNG_SIGNATURE.len()),
        MAX_USER_IMAGE_DIMENSION + 1,
        1,
    )
    .expect_err("dimension over limit must reject");
    assert!(dimension_error.to_string().contains("8000"));

    let height = u32::try_from(MAX_USER_IMAGE_PIXELS / u64::from(MAX_USER_IMAGE_DIMENSION) + 1)
        .expect("pixel test height fits u32");
    let pixel_error = UserImageInput::png(
        "[Image #1]",
        png_bytes(PNG_SIGNATURE.len()),
        MAX_USER_IMAGE_DIMENSION,
        height,
    )
    .expect_err("pixel count over limit must reject");
    assert!(pixel_error.to_string().contains("32 million pixels"));
}

#[test]
fn user_image_input_rejects_non_png_and_zero_dimensions() {
    assert!(UserImageInput::png("[Image #1]", Arc::<[u8]>::from([1, 2, 3]), 1, 1).is_err());
    assert!(UserImageInput::png("[Image #1]", png_bytes(PNG_SIGNATURE.len()), 0, 1).is_err());
}
