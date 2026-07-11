use super::*;

#[test]
fn runtime_builder_registers_checkpoint_ref_tool_when_auto_compaction_enabled() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-default"))
        .build()
        .expect("runtime should build");
    let name = merry_read_checkpoint_ref_tool_name();

    assert!(
        runtime.inner.tool_registry.registered_tool(&name).is_some(),
        "default automatic compaction should expose the checkpoint ref tool"
    );
}

#[test]
fn runtime_builder_omits_checkpoint_ref_tool_when_auto_compaction_disabled() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-disabled"))
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime should build");
    let name = merry_read_checkpoint_ref_tool_name();

    assert!(
        runtime.inner.tool_registry.registered_tool(&name).is_none(),
        "disabling automatic compaction should hide the checkpoint ref tool"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_includes_checkpoint_ref_tool_when_auto_compaction_enabled() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-provider-checkpoint-ref-tool"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "hello", StepContext::default()).await;
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    let tool_names = request
        .tools()
        .iter()
        .map(|tool| tool.name().as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"merry_read_checkpoint_ref"));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_step_fails_when_checkpoint_ref_tool_requires_unsupported_tool_calls() {
    let provider = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])],
        ModelCapabilities::new(true, false, false, true, Some(64_000), None)
            .expect("valid capabilities"),
    );
    let runtime = Runtime::builder(session_id("runtime-provider-checkpoint-ref-tool-no-tools"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "hello", StepContext::default()).await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("provider_tool_calls_unsupported")
    );
    assert!(
        provider.recorded_requests().is_empty(),
        "providers without tool-call support must fail before request dispatch when runtime tools are registered"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_step_fails_when_final_output_contract_requires_unsupported_tool_calls() {
    let provider = RecordingModelProvider::with_script_and_capabilities(
        vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event(),
        )])],
        ModelCapabilities::new(true, false, false, true, Some(64_000), None)
            .expect("valid capabilities"),
    );
    let runtime = Runtime::builder(session_id("runtime-final-output-no-tool-provider"))
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "return structured output",
        StepContext::default().with_final_output_contract(final_output_contract()),
    )
    .await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("provider_tool_calls_unsupported")
    );
    assert!(
        provider.recorded_requests().is_empty(),
        "providers without tool-call support must fail before request dispatch when final output is tool-backed"
    );
}

fn final_output_contract() -> crate::FinalOutputContract {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Concise final answer."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("test schema should be a JSON schema");
    crate::FinalOutputContract::new(ToolInputSchema::new(schema).expect("valid tool input schema"))
        .expect("valid final output contract")
}

fn checkpoint_ref_pending_tool_call(call_id: &str, ref_id: &str) -> PendingToolCall {
    checkpoint_ref_pending_tool_call_with_arguments(call_id, json!({ "ref": ref_id }))
}

fn checkpoint_ref_pending_tool_call_with_arguments(
    call_id: &str,
    arguments: serde_json::Value,
) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(call_id).expect("valid tool call id"),
        merry_read_checkpoint_ref_tool_name(),
        ToolCallArguments::try_from(arguments).expect("valid checkpoint ref tool arguments"),
    )
}

fn invalid_checkpoint_ref_pending_tool_call(
    call_id: &str,
    arguments: serde_json::Value,
) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(call_id).expect("valid tool call id"),
        merry_read_checkpoint_ref_tool_name(),
        ToolCallArguments::try_from(arguments).expect("valid JSON arguments"),
    )
}

fn citation_checkpoint_for_runtime_tool() -> CompactedCheckpoint {
    let checkpoint_id = CheckpointId::new("checkpoint-runtime-tool").expect("valid checkpoint id");
    let manifest = CheckpointRefManifest::new(
        checkpoint_id.clone(),
        vec![CheckpointRef::new(
            CheckpointRefId::new("bootstrap-ref").expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            EvidenceRef::new(
                ArtifactId::new("runtime-checkpoint-source").expect("valid artifact id"),
                EvidenceLocator::whole_artifact(),
            ),
        )],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(
        r#"{
            "confirmed_decisions": [],
            "rejected_approaches": [],
            "constraints_preferences_boundaries": [],
            "corrected_misunderstandings": [],
            "durable_conclusions": [
                {
                    "id": "c1",
                    "text": "The task depends on bootstrap-ref.",
                    "refs": ["bootstrap-ref"]
                }
            ],
            "open_questions": [],
            "current_progress_and_next_steps": [],
            "exact_details": [],
            "handoffs": []
        }"#,
    )
    .expect("valid candidate");
    let checkpoint = CitationBackedCheckpoint::from_candidate(
        checkpoint_id,
        candidate,
        manifest,
        Default::default(),
    )
    .expect("valid citation checkpoint");
    CompactedCheckpoint::from_citation_backed(checkpoint).expect("valid compacted checkpoint")
}

fn checkpoint_ref_source_artifact() -> ArtifactRef {
    ArtifactRef::new(
        ArtifactId::new("runtime-checkpoint-source").expect("valid artifact id"),
        ArtifactKind::Text,
    )
}

fn runtime_builder_with_checkpoint_ref_source(
    session: &str,
    content: impl Into<String>,
) -> RuntimeBuilder {
    Runtime::builder(session_id(session))
        .compacted_checkpoint(citation_checkpoint_for_runtime_tool())
        .compacted_checkpoint_evidence(
            checkpoint_ref_source_artifact(),
            ArtifactContent::text(content),
        )
}

fn runtime_with_checkpoint_ref_source(session: &str, content: String) -> Runtime {
    runtime_builder_with_checkpoint_ref_source(session, content)
        .build()
        .expect("checkpoint and source should build together")
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_returns_default_original_page_json() {
    let pending = checkpoint_ref_pending_tool_call("call-read-checkpoint-ref", "bootstrap-ref");
    let source = format!("{}PAGE_TWO", "x".repeat(5000));
    let runtime =
        runtime_with_checkpoint_ref_source("runtime-checkpoint-ref-tool-success", source.clone());
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call should record");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("checkpoint ref tool should resolve");

    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("tool result artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("checkpoint ref result should be textual JSON"),
    )
    .expect("checkpoint ref result should parse as JSON");
    assert_eq!(
        payload,
        json!({
            "ref": "bootstrap-ref",
            "source_kind": "user_message",
            "artifact_id": "runtime-checkpoint-source",
            "offset": 0,
            "content": &source[..4096],
            "next_offset": 4096,
            "total_bytes": source.len(),
            "done": false
        })
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_page_reads_second_page_from_original_content() {
    let source = format!("{}SENTINEL_AFTER_1200_BYTES", "x".repeat(1200));
    let runtime = runtime_with_checkpoint_ref_source("runtime-checkpoint-ref-page-second", source);
    let ref_id = CheckpointRefId::new("bootstrap-ref").expect("valid ref id");

    let first = runtime
        .read_checkpoint_ref_page(&ref_id, 0, 1024)
        .await
        .expect("first page should read");
    let second = runtime
        .read_checkpoint_ref_page(
            &ref_id,
            first.next_offset().expect("first page should continue"),
            1024,
        )
        .await
        .expect("second page should read");

    assert!(second.content().contains("SENTINEL_AFTER_1200_BYTES"));
    assert_eq!(second.artifact_id(), first.artifact_id());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_not_found_without_checkpoint() {
    assert_checkpoint_ref_not_found(
        "call-missing-checkpoint",
        "bootstrap-ref",
        Runtime::builder(session_id("runtime-checkpoint-ref-tool-no-checkpoint"))
            .build()
            .expect("runtime should build"),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_not_found_for_plain_checkpoint() {
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-plain"))
        .compacted_checkpoint(
            CompactedCheckpoint::new("plain checkpoint").expect("valid checkpoint"),
        )
        .build()
        .expect("runtime should build");
    assert_checkpoint_ref_not_found("call-plain-checkpoint", "bootstrap-ref", runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_not_found_for_unknown_ref() {
    let runtime = runtime_builder_with_checkpoint_ref_source(
        "runtime-checkpoint-ref-tool-unknown",
        "exact checkpoint source",
    )
    .build()
    .expect("runtime should build");
    assert_checkpoint_ref_not_found("call-unknown-checkpoint-ref", "r9", runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_reports_read_failed_when_ref_artifact_is_missing() {
    let pending =
        checkpoint_ref_pending_tool_call("call-missing-checkpoint-artifact", "bootstrap-ref");
    let runtime = Runtime::builder(session_id("runtime-checkpoint-ref-tool-missing-artifact"))
        .build()
        .expect("runtime should build");
    {
        let mut session = runtime.inner.session.lock().await;
        session.set_compacted_checkpoint(citation_checkpoint_for_runtime_tool());
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call should record");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("missing checkpoint evidence should resolve as a domain failure");
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("read failure should include diagnostic")
            .code(),
        "checkpoint_ref_read_failed"
    );
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("failure artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("checkpoint ref failure should be textual JSON"),
    )
    .expect("checkpoint ref failure should parse as JSON");
    assert_eq!(payload["error"], "checkpoint_ref_read_failed");
    assert_eq!(payload["ref"], "bootstrap-ref");
    assert!(
        payload["message"]
            .as_str()
            .expect("read failure should include message")
            .contains("runtime-checkpoint-source")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_invalid_ref_string_reports_not_found() {
    let runtime = runtime_builder_with_checkpoint_ref_source(
        "runtime-checkpoint-ref-tool-invalid-ref-string",
        "exact checkpoint source",
    )
    .build()
    .expect("runtime should build");
    assert_checkpoint_ref_not_found("call-invalid-ref-string", " bad ref ", runtime).await;
}

async fn assert_checkpoint_ref_not_found(call_id: &str, ref_id: &str, runtime: Runtime) {
    let pending = checkpoint_ref_pending_tool_call(call_id, ref_id);
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call should record");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("checkpoint ref tool should resolve as domain failure");
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("not found should include diagnostic")
            .code(),
        "checkpoint_ref_not_found"
    );
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("failure artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("checkpoint ref failure should be textual JSON"),
    )
    .expect("checkpoint ref failure should parse as JSON");
    assert_eq!(
        payload,
        json!({
            "error": "checkpoint_ref_not_found",
            "ref": ref_id,
        })
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_ref_tool_invalid_input_uses_schema_validation() {
    for (call_id, arguments) in [
        ("call-checkpoint-ref-missing-ref", json!({})),
        (
            "call-checkpoint-ref-extra-field",
            json!({ "ref": "bootstrap-ref", "checkpoint_id": "c1" }),
        ),
        ("call-checkpoint-ref-non-string", json!({ "ref": 9 })),
        (
            "call-checkpoint-ref-negative-offset",
            json!({ "ref": "bootstrap-ref", "offset": -1 }),
        ),
        (
            "call-checkpoint-ref-zero-page",
            json!({ "ref": "bootstrap-ref", "max_bytes": 0 }),
        ),
        (
            "call-checkpoint-ref-page-too-large",
            json!({ "ref": "bootstrap-ref", "max_bytes": 16_385 }),
        ),
        (
            "call-checkpoint-ref-fractional-page",
            json!({ "ref": "bootstrap-ref", "max_bytes": 1.5 }),
        ),
    ] {
        let pending = invalid_checkpoint_ref_pending_tool_call(call_id, arguments);
        let runtime = runtime_builder_with_checkpoint_ref_source(
            &format!("runtime-{call_id}"),
            "exact checkpoint source",
        )
        .build()
        .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session
                .record_test_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("invalid input should resolve as schema failure");
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            result
                .diagnostic()
                .expect("schema failure should include diagnostic")
                .code(),
            "tool_input_schema_invalid"
        );
    }
}

#[test]
fn runtime_builder_rejects_manual_checkpoint_ref_tool_registration() {
    let duplicate = RegisteredTool::read_only(
        ToolSpec::new(
            merry_read_checkpoint_ref_tool_name(),
            "Duplicate checkpoint ref tool",
            ToolInputSchema::new(
                Schema::try_from(json!({
                    "type": "object",
                    "additionalProperties": false
                }))
                .expect("test schema should be a JSON object"),
            )
            .expect("valid tool input schema"),
        )
        .expect("valid duplicate tool spec"),
        Arc::new(SuccessfulToolExecutor::new()),
    );

    let error = match Runtime::builder(session_id("runtime-checkpoint-ref-tool-duplicate"))
        .register_tool(duplicate)
        .build()
    {
        Ok(_) => panic!("duplicate built-in tool name should fail"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        RuntimeError::DuplicateToolRegistration { ref name }
            if name.as_str() == "merry_read_checkpoint_ref"
    ));
}
