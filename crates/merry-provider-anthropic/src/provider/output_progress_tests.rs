use super::AnthropicEventStreamEvents;
use merry_llm::{ModelEvent, ModelOutputProgress};
use std::time::{Duration, Instant};

#[test]
fn thinking_text_and_tool_json_share_receive_timing_without_signature_or_usage_tails() {
    let now = Instant::now();
    let mut events = AnthropicEventStreamEvents::new();
    for (seconds, data) in [
        (
            1,
            r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"output_tokens":0}}}"#,
        ),
        (
            2,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ),
        (
            10,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"思考"}}"#,
        ),
        (
            11,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"not model output"}}"#,
        ),
        (12, r#"{"type":"content_block_stop","index":0}"#),
        (
            13,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":"ok"}}"#,
        ),
        (
            14,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"ay"}}"#,
        ),
        (15, r#"{"type":"content_block_stop","index":1}"#),
        (
            16,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call-1","name":"read","input":{}}}"#,
        ),
        (
            18,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        ),
        (19, r#"{"type":"content_block_stop","index":2}"#),
        (
            40,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2400}}"#,
        ),
        (50, r#"{"type":"message_stop"}"#),
    ] {
        events
            .parse_bytes_at(
                format!("data: {data}\n").as_bytes(),
                now + Duration::from_secs(seconds),
            )
            .unwrap();
    }
    let mut samples = Vec::new();
    let mut visible = String::new();
    let mut usage = None;
    while let Some(event) = events.pop_pending() {
        match event {
            ModelEvent::OutputProgress {
                progress: Some(progress),
            } => samples.push(progress),
            ModelEvent::OutputTextDelta { delta } => visible.push_str(&delta),
            ModelEvent::Completed { response } => usage = response.usage(),
            _ => {}
        }
    }
    assert_eq!(visible, "okay");
    assert_eq!(samples.len(), 5);
    assert_eq!(
        samples.first(),
        Some(
            &ModelOutputProgress::new(6, Duration::ZERO)
                .with_reasoning_observed(true)
                .with_timing_quality(merry_core::OutputTimingQuality::PartialOutput)
        )
    );
    assert_eq!(
        samples.last(),
        Some(
            &ModelOutputProgress::new(16, Duration::from_secs(8))
                .with_reasoning_observed(true)
                .with_timing_quality(merry_core::OutputTimingQuality::PartialOutput)
        )
    );
    assert_eq!(usage.unwrap().output_tokens, 2400);
}

#[test]
fn initial_thinking_counts_but_redacted_data_and_empty_deltas_do_not() {
    let now = Instant::now();
    let mut events = AnthropicEventStreamEvents::new();
    for (seconds, data) in [
        (
            0,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"opaque"}}"#,
        ),
        (1, r#"{"type":"ping"}"#),
        (
            2,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"plan"}}"#,
        ),
        (
            3,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":""}}"#,
        ),
        (
            10,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"text","text":"done"}}"#,
        ),
    ] {
        events
            .parse_bytes_at(
                format!("data: {data}\n").as_bytes(),
                now + Duration::from_secs(seconds),
            )
            .unwrap();
    }
    let mut samples = Vec::new();
    while let Some(event) = events.pop_pending() {
        if let ModelEvent::OutputProgress {
            progress: Some(progress),
        } = event
        {
            samples.push(progress);
        }
    }
    assert_eq!(
        samples,
        vec![
            ModelOutputProgress::new(4, Duration::ZERO)
                .with_reasoning_observed(true)
                .with_timing_quality(merry_core::OutputTimingQuality::PartialOutput),
            ModelOutputProgress::new(8, Duration::from_secs(8))
                .with_reasoning_observed(true)
                .with_timing_quality(merry_core::OutputTimingQuality::PartialOutput)
        ]
    );
}
