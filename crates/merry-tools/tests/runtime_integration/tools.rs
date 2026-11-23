use super::support::*;

#[tokio::test]
async fn read_success_and_failure_continue_with_compact_body_and_full_artifact() {
    for path in ["note.txt", "missing.txt"] {
        let temp = TempWorkspace::new("read-model-body");
        temp.write_text("note.txt", "alpha\n");
        let provider = ScriptedModelProvider::new(vec![
            vec![Ok(pending_read_file_call(path))],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("done")],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);
        let runtime =
            runtime_with_opt_in_apply_patch_tools_and_provider(temp.path(), provider.clone());
        let events = execute_first_pending_call(&runtime, "read file").await;
        let result = resolved_tool_result(&events);
        let full = runtime
            .read_tool_output(result.call_id())
            .await
            .expect("full output read")
            .expect("full output exists");
        let full: Value = serde_json::from_str(full.as_str()).expect("JSON");
        let events = collect_step(&runtime, "continue").await;
        assert_eq!(
            event_kind_names(&events),
            ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
        );
        let requests = provider.recorded_requests();
        let model_result = requests[1]
            .continuations()
            .first()
            .expect("continuation")
            .result();
        assert_eq!(model_result.status(), result.status());
        if path == "note.txt" {
            assert_eq!(
                model_result.content().as_text(),
                Some("note.txt:1-1\nalpha\n")
            );
            assert_eq!(full["content"], "alpha\n");
        } else {
            let model: Value =
                serde_json::from_str(model_result.content().as_json().expect("failure JSON"))
                    .expect("JSON");
            assert_eq!(model["error"], full["error"]);
            assert_eq!(model["guidance"], full["guidance"]["message"]);
            assert!(model.get("recovery").is_none());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_tool_records_artifact_before_resolving_pending_call() {
    let temp = TempWorkspace::new("event-order");
    temp.write_text("note.txt", "alpha\n");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_file_call("note.txt"));

    let pending_events = collect_step(&runtime, "read note").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("registered read_file tool should execute");

    assert_eq!(event_kind_names(&execution_events), ["ToolCallResolved"]);
    let result = match &execution_events[0].payload {
        RuntimeJournalPayload::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    let output = runtime
        .read_tool_output(result.call_id())
        .await
        .expect("output read")
        .expect("output exists");
    assert_eq!(output.kind(), ToolOutputKind::Json);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn registered_read_file_domain_failure_records_failed_json_before_resolving_pending_call() {
    let temp = TempWorkspace::new("domain-failure");
    let runtime = runtime_with_workspace_tools(temp.path(), pending_read_file_call("missing.txt"));

    let pending_events = collect_step(&runtime, "read missing note").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("pending call should be stored");

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve pending call");

    assert_eq!(event_kind_names(&execution_events), ["ToolCallResolved"]);
    let result = match &execution_events[0].payload {
        RuntimeJournalPayload::ToolCallResolved { result } => result,
        other => panic!("expected tool resolution, got {other:?}"),
    };
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    let output = runtime
        .read_tool_output(result.call_id())
        .await
        .expect("output read")
        .expect("output exists");
    assert_eq!(output.kind(), ToolOutputKind::Json);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should include diagnostic")
            .code(),
        "workspace_file_not_found"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test]
async fn read_file_image_attaches_normalized_png_to_model_continuation_and_persists_compact_artifact()
 {
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    let temp = TempWorkspace::new("read-image-continuation");
    let mut webp_bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 3, Rgba([20, 40, 60, 255])))
        .write_to(&mut webp_bytes, ImageFormat::WebP)
        .expect("webp fixture should encode");
    let webp_bytes = webp_bytes.into_inner();
    temp.write_bytes("assets/mockup.webp", &webp_bytes);

    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_read_file_call("assets/mockup.webp"))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("I inspected the mockup.")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let runtime = runtime_with_opt_in_apply_patch_tools_and_provider(temp.path(), provider.clone());
    let events = execute_first_pending_call(&runtime, "inspect assets/mockup.webp").await;
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);

    let full = runtime
        .read_tool_output(result.call_id())
        .await
        .expect("full output read")
        .expect("full output exists");
    let full_json: Value = serde_json::from_str(full.as_str()).expect("JSON");
    assert_eq!(full_json["ok"], true);
    assert_eq!(full_json["tool"], READ_FILE_TOOL);
    assert_eq!(full_json["kind"], "image");
    assert_eq!(full_json["path"], "assets/mockup.webp");
    assert_eq!(full_json["source_format"], "webp");
    assert_eq!(full_json["width"], 4);
    assert_eq!(full_json["height"], 3);
    assert_eq!(full_json["resized"], false);
    assert_eq!(full_json["source_bytes"], webp_bytes.len());

    let events = collect_step(&runtime, "describe the image").await;
    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );

    let requests = provider.recorded_requests();
    let model_result = requests[1]
        .continuations()
        .first()
        .expect("continuation")
        .result();
    assert_eq!(model_result.status(), ToolCallResultStatus::Succeeded);
    assert!(
        model_result
            .content()
            .as_text()
            .expect("model text")
            .contains("assets/mockup.webp: webp image (4x3,")
    );
    assert_eq!(model_result.images().len(), 1);
    let attached = &model_result.images()[0];
    assert_eq!(attached.label(), "assets/mockup.webp");
    assert_eq!((attached.width(), attached.height()), (4, 3));
    assert!(attached.png_bytes().starts_with(b"\x89PNG\r\n\x1a\n"));
}
