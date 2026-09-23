use super::{RequestTokenCalibration, RequestTokenObservation, TokenEstimateScale};
use crate::token_estimate::{estimate_model_input_tokens, estimate_request_input_tokens};
use merry_core::{ProviderName, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{
    GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName, ModelRequest,
};
use serde_json::json;

fn provider(name: &str) -> ProviderName {
    ProviderName::new(name).expect("provider")
}

fn request(model: &str, instructions: &str, text: &str, tool_description: &str) -> ModelRequest {
    let tool = ToolSpec::new(
        ToolName::new("lookup").expect("tool name"),
        tool_description,
        ToolInputSchema::new(
            schemars::Schema::try_from(json!({"type": "object", "properties": {}}))
                .expect("schema"),
        )
        .expect("input schema"),
    )
    .expect("tool");
    ModelRequest::new_with_continuations_and_stable_prefix(
        ModelName::new(model).expect("model"),
        vec![
            ModelMessage::new(
                ModelMessageRole::System,
                ModelContent::text(instructions).expect("text"),
            )
            .expect("instructions"),
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text(text).expect("text"),
            )
            .expect("input"),
        ],
        vec![tool],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("request")
}

#[test]
fn feedback_matches_the_complete_request_not_just_the_dynamic_body() {
    let provider = provider("provider");
    let request = request(
        "model",
        &"rules ".repeat(1_000),
        "hello",
        &"tool ".repeat(100),
    );
    let base_tokens = estimate_request_input_tokens(&request);
    assert!(base_tokens > estimate_model_input_tokens(request.dynamic_input()) * 100);
    let calibration = RequestTokenCalibration::observe(
        None,
        RequestTokenObservation::new(&provider, &request),
        base_tokens * 2,
    )
    .expect("measurement");
    assert_eq!(
        calibration.scale_for(&provider, &request).estimate(10_000),
        20_000
    );
}

#[test]
fn rising_input_cost_is_corrected_immediately_and_falling_cost_is_smoothed() {
    let provider = provider("provider");
    let request = request("model", "rules", "hello", "lookup");
    let base_tokens = estimate_request_input_tokens(&request);
    let mut calibration = None;
    for multiplier in [2, 3, 4] {
        calibration = RequestTokenCalibration::observe(
            calibration.as_ref(),
            RequestTokenObservation::new(&provider, &request),
            base_tokens * multiplier,
        );
        assert_eq!(
            calibration
                .as_ref()
                .expect("calibration")
                .scale_for(&provider, &request)
                .estimate(base_tokens),
            base_tokens * multiplier,
        );
    }
    let updated = RequestTokenCalibration::observe(
        calibration.as_ref(),
        RequestTokenObservation::new(&provider, &request),
        base_tokens * 2,
    )
    .expect("measurement");
    let estimate = updated.scale_for(&provider, &request).estimate(base_tokens);
    assert!(estimate > base_tokens * 2 && estimate < base_tokens * 4);
}

#[test]
fn consistent_overestimation_converges_without_an_abrupt_drop() {
    let provider = provider("provider");
    let request = request("model", "rules", &"abcd".repeat(1_000), "lookup");
    let base_tokens = estimate_request_input_tokens(&request);
    let actual_tokens = base_tokens / 2;
    let mut calibration = None;
    let mut previous = base_tokens;
    for _ in 0..32 {
        calibration = RequestTokenCalibration::observe(
            calibration.as_ref(),
            RequestTokenObservation::new(&provider, &request),
            actual_tokens,
        );
        let estimated = calibration
            .as_ref()
            .expect("calibration")
            .scale_for(&provider, &request)
            .estimate(base_tokens);
        assert!((actual_tokens..=previous).contains(&estimated));
        previous = estimated;
    }
    assert!(previous <= actual_tokens + 1);
}

#[test]
fn unrelated_provider_model_prefix_and_tool_contracts_do_not_share_feedback() {
    let source_provider = provider("primary");
    let source = request("model", "rules", "first", "lookup");
    let calibration = RequestTokenCalibration::observe(
        None,
        RequestTokenObservation::new(&source_provider, &source),
        estimate_request_input_tokens(&source) * 2,
    )
    .expect("measurement");
    for (provider, candidate) in [
        (
            provider("other"),
            request("model", "rules", "next", "lookup"),
        ),
        (
            provider("primary"),
            request("other-model", "rules", "next", "lookup"),
        ),
        (
            provider("primary"),
            request("model", "new rules", "next", "lookup"),
        ),
        (
            provider("primary"),
            request("model", "rules", "next", "new tool"),
        ),
    ] {
        assert_eq!(
            calibration.scale_for(&provider, &candidate).estimate(100),
            100
        );
    }
    let next = request("model", "rules", "different dynamic input", "lookup");
    assert_eq!(
        calibration.scale_for(&source_provider, &next).estimate(100),
        200
    );
}

#[test]
fn image_requests_do_not_reuse_text_only_feedback() {
    let provider = provider("provider");
    let request = request("model", "rules", "hello", "lookup");
    let calibration = RequestTokenCalibration::observe(
        None,
        RequestTokenObservation::new(&provider, &request),
        estimate_request_input_tokens(&request) * 2,
    )
    .expect("measurement");
    let image = merry_llm::ModelImage::png(
        "[Image #1]",
        std::sync::Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]),
        100,
        100,
    )
    .expect("image");
    let mut input = request.input().to_vec();
    input.push(merry_llm::ModelInputItem::Message(
        ModelMessage::new(
            ModelMessageRole::User,
            ModelContent::user_with_images("inspect [Image #1]", vec![image])
                .expect("image content"),
        )
        .expect("message"),
    ));
    let image_request = ModelRequest::new_with_input_and_stable_prefix(
        request.model().clone(),
        input,
        request.tools().to_vec(),
        GenerationConfig::default(),
        request.stable_prefix_item_count(),
    )
    .expect("image request");
    assert_eq!(
        calibration
            .scale_for(&provider, &image_request)
            .estimate(100),
        100
    );
}

#[test]
fn zero_measurements_are_ignored_and_large_counts_saturate_safely() {
    assert!(TokenEstimateScale::from_usage(0, 100).is_none());
    assert!(TokenEstimateScale::from_usage(100, 0).is_none());
    let scale = TokenEstimateScale::from_usage(3, 4).expect("scale");
    assert_eq!(scale.estimate(0), 0);
    assert_eq!(scale.estimate(1), 2);
    let scale = TokenEstimateScale::from_usage(1, u64::MAX).expect("scale");
    assert_eq!(scale.estimate(1), u64::MAX);
    assert_eq!(scale.estimate(u64::MAX), u64::MAX);
}

#[test]
fn persisted_feedback_round_trips_and_rejects_a_zero_scale() {
    let provider = provider("provider");
    let request = request("model", "rules", "hello", "lookup");
    let calibration = RequestTokenCalibration::observe(
        None,
        RequestTokenObservation::new(&provider, &request),
        estimate_request_input_tokens(&request) * 2,
    )
    .expect("measurement");
    let mut encoded = serde_json::to_value(&calibration).expect("serialize calibration");
    let decoded: RequestTokenCalibration =
        serde_json::from_value(encoded.clone()).expect("restore calibration");
    assert_eq!(decoded.scale_for(&provider, &request).estimate(100), 200);
    encoded["scale"] = json!(0);
    assert!(serde_json::from_value::<RequestTokenCalibration>(encoded).is_err());
}
