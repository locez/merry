use crate::{OpenAiProtocol, provider::OpenAiEventStreamEvents};
use merry_llm::{ModelEvent, ModelOutputProgress};
use std::time::{Duration, Instant};

fn receive(events: &mut OpenAiEventStreamEvents, now: Instant, seconds: u64, data: &str) {
    events
        .parse_bytes_at(
            format!("data: {data}\n").as_bytes(),
            now + Duration::from_secs(seconds),
        )
        .unwrap();
}

fn drain(events: &mut OpenAiEventStreamEvents) -> (Vec<ModelOutputProgress>, String) {
    let mut samples = Vec::new();
    let mut visible = String::new();
    while let Some(event) = events.pop_pending() {
        match event {
            ModelEvent::OutputProgress {
                progress: Some(progress),
            } => samples.push(progress),
            ModelEvent::OutputTextDelta { delta } => visible.push_str(&delta),
            _ => {}
        }
    }
    (samples, visible)
}

#[test]
fn chat_counts_ds_reasoning_text_and_tool_fragments_without_start_or_done_latency() {
    let now = Instant::now();
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::ChatCompletions);
    for (seconds, data) in [
        (
            1,
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null}}]}"#,
        ),
        (
            10,
            r#"{"choices":[{"index":0,"delta":{"reasoning_content":"思考"}}]}"#,
        ),
        (
            12,
            r#"{"choices":[{"index":0,"delta":{"reasoning_content":"abcd","reasoning":"duplicate"}}]}"#,
        ),
        (14, r#"{"choices":[{"index":0,"delta":{"content":"ok"}}]}"#),
        (
            16,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read","arguments":"{\"path\":"}}]}}]}"#,
        ),
        (
            18,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a\"}"}}]}}]}"#,
        ),
        (
            20,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ),
        (
            22,
            r#"{"choices":[],"usage":{"prompt_tokens":20000,"completion_tokens":2400,"completion_tokens_details":{"reasoning_tokens":2000}}}"#,
        ),
        (30, "[DONE]"),
    ] {
        receive(&mut events, now, seconds, data);
    }
    events.finish_stream().unwrap();
    let (samples, visible) = drain(&mut events);
    assert_eq!(
        samples.first(),
        Some(&ModelOutputProgress::new(6, Duration::ZERO).with_reasoning_observed(true))
    );
    assert_eq!(
        samples.last(),
        Some(&ModelOutputProgress::new(28, Duration::from_secs(8)).with_reasoning_observed(true))
    );
    assert_eq!(samples.len(), 5);
    assert_eq!(visible, "ok");
}

#[test]
fn responses_reasoning_and_tool_output_ignore_summary_duplicates_and_done_snapshots() {
    let now = Instant::now();
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);
    for (seconds, data) in [
        (1, r#"{"type":"response.created"}"#),
        (
            10,
            r#"{"type":"response.reasoning_text.delta","delta":"思考"}"#,
        ),
        (
            11,
            r#"{"type":"response.reasoning_summary_text.delta","delta":"summary"}"#,
        ),
        (
            12,
            r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call-1","name":"read","arguments":""}}"#,
        ),
        (
            14,
            r#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{}"}"#,
        ),
        (
            50,
            r#"{"type":"response.function_call_arguments.done","output_index":1,"arguments":"{}"}"#,
        ),
        (
            60,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call-1","name":"read","arguments":"{}"}}"#,
        ),
        (
            70,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":70}}}"#,
        ),
    ] {
        receive(&mut events, now, seconds, data);
    }
    events.finish_stream().unwrap();
    let (samples, visible) = drain(&mut events);
    assert_eq!(samples.len(), 3);
    assert_eq!(
        samples.last(),
        Some(&ModelOutputProgress::new(12, Duration::from_secs(4)).with_reasoning_observed(true))
    );
    assert!(visible.is_empty());
}

#[test]
fn responses_summary_is_an_estimation_proxy_when_raw_reasoning_is_not_exposed() {
    let now = Instant::now();
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);
    receive(
        &mut events,
        now,
        10,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"plan"}"#,
    );
    receive(
        &mut events,
        now,
        12,
        r#"{"type":"response.output_text.delta","delta":"done"}"#,
    );
    receive(
        &mut events,
        now,
        30,
        r#"{"type":"response.output_text.done","text":"done"}"#,
    );
    let (samples, visible) = drain(&mut events);
    assert_eq!(
        samples.last(),
        Some(
            &ModelOutputProgress::new(8, Duration::from_secs(2))
                .with_timing_quality(merry_core::OutputTimingQuality::PartialOutput)
        )
    );
    assert_eq!(visible, "done");
}

#[test]
fn transport_coalescing_and_late_consumer_do_not_invent_an_output_interval() {
    let now = Instant::now();
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::ChatCompletions);
    events.parse_bytes_at(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"plan\"}}]}\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"}}]}\n", now).unwrap();
    let (samples, visible) = drain(&mut events);
    assert_eq!(
        samples.last(),
        Some(&ModelOutputProgress::new(8, Duration::ZERO).with_reasoning_observed(true))
    );
    assert_eq!(visible, "done");
}

#[test]
fn empty_deltas_heartbeats_and_usage_do_not_start_a_measurement() {
    for protocol in [OpenAiProtocol::Responses, OpenAiProtocol::ChatCompletions] {
        let mut events = OpenAiEventStreamEvents::new(protocol);
        events
            .parse_bytes_at(b": heartbeat\n", Instant::now())
            .unwrap();
        let data = match protocol {
            OpenAiProtocol::Responses => r#"{"type":"response.output_text.delta","delta":""}"#,
            OpenAiProtocol::ChatCompletions => {
                r#"{"choices":[{"index":0,"delta":{"reasoning_content":"","content":""}}]}"#
            }
        };
        receive(&mut events, Instant::now(), 60, data);
        assert!(drain(&mut events).0.is_empty());
    }
}

#[test]
fn split_utf8_lines_are_timed_on_receipt_and_eof_preserves_the_last_chunk_time() {
    let now = Instant::now();
    let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);
    let line = "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"思考\"}\n";
    let split = line.find('思').unwrap() + 1;
    events
        .parse_bytes_at(&line.as_bytes()[..split], now)
        .unwrap();
    events
        .parse_bytes_at(&line.as_bytes()[split..], now + Duration::from_secs(10))
        .unwrap();
    events
        .parse_bytes_at(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}",
            now + Duration::from_secs(12),
        )
        .unwrap();
    assert!(events.finish_stream().is_err());
    let (samples, visible) = drain(&mut events);
    assert_eq!(
        samples.last(),
        Some(&ModelOutputProgress::new(8, Duration::from_secs(2)).with_reasoning_observed(true))
    );
    assert_eq!(visible, "ok");
}
