use super::support::*;

#[tokio::test(flavor = "current_thread")]
async fn coding_loop_harness_inspects_patches_verifies_and_completes() {
    let temp = TempWorkspace::new("coding-loop-harness");
    temp.write_text(
        "src/lib.rs",
        "pub const GREETING_LABEL: &str = \"old\";\n\npub fn greeting() -> &'static str {\n    GREETING_LABEL\n}\n\npub fn context_001() -> &'static str { \"context-001\" }\npub fn context_002() -> &'static str { \"context-002\" }\npub fn context_003() -> &'static str { \"context-003\" }\npub fn context_004() -> &'static str { \"context-004\" }\npub fn context_005() -> &'static str { \"context-005\" }\npub fn context_006() -> &'static str { \"context-006\" }\npub fn context_007() -> &'static str { \"context-007\" }\npub fn context_008() -> &'static str { \"context-008\" }\npub fn context_009() -> &'static str { \"context-009\" }\npub fn context_010() -> &'static str { \"context-010\" }\n",
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(pending_process_call(
            "coding-loop-rg-files",
            "rg --files",
        ))],
        vec![Ok(pending_read_file_call("src/lib.rs"))],
        vec![Ok(pending_patch_call(
            "src/lib.rs",
            "pub const GREETING_LABEL: &str = \"old\";",
            "pub const GREETING_LABEL: &str = \"new\";",
        ))],
        vec![Ok(pending_process_call(
            "coding-loop-cargo-test",
            "cargo test -p merry-runtime",
        ))],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text(
                    "changed greeting to new and verified tests",
                )],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let provider_handle = provider.clone();
    let runner = Arc::new(ScriptedProcessRunner::new(vec![
        ScriptedProcessResponse::success("src/lib.rs\n"),
        ScriptedProcessResponse::success("test result: ok. 1 passed; 0 failed\n"),
    ]));
    let runtime = runtime_with_coding_loop_tools(temp.path(), provider, runner.clone());

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Fix the greeting and verify it.").expect("valid user task"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(8).expect("valid model-turn budget"),
        )
        .await
        .expect("coding loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 5);
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        fs::read_to_string(temp.path().join("src/lib.rs"))
            .expect("patched workspace file should read"),
        "pub const GREETING_LABEL: &str = \"new\";\n\npub fn greeting() -> &'static str {\n    GREETING_LABEL\n}\n\npub fn context_001() -> &'static str { \"context-001\" }\npub fn context_002() -> &'static str { \"context-002\" }\npub fn context_003() -> &'static str { \"context-003\" }\npub fn context_004() -> &'static str { \"context-004\" }\npub fn context_005() -> &'static str { \"context-005\" }\npub fn context_006() -> &'static str { \"context-006\" }\npub fn context_007() -> &'static str { \"context-007\" }\npub fn context_008() -> &'static str { \"context-008\" }\npub fn context_009() -> &'static str { \"context-009\" }\npub fn context_010() -> &'static str { \"context-010\" }\n"
    );

    let observed_argv = runner
        .observed_intents()
        .into_iter()
        .map(|intent| intent.argv().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        observed_argv,
        vec![
            vec!["bash".to_owned(), "-lc".to_owned(), "rg --files".to_owned()],
            vec![
                "bash".to_owned(),
                "-lc".to_owned(),
                "cargo test -p merry-runtime".to_owned()
            ],
        ]
    );

    let requests = provider_handle.recorded_requests();
    assert_eq!(requests.len(), 5);
    assert!(requests[0].continuations().is_empty());
    for request in requests.iter().skip(1) {
        assert_continuation_request_body(request, "Fix the greeting and verify it.");
    }
    let expected_continuation_ids = [
        "coding-loop-rg-files",
        "workspace-read-call",
        "workspace-patch-call",
        "coding-loop-cargo-test",
    ];
    for (index, request) in requests.iter().skip(1).enumerate() {
        let ids = request
            .continuations()
            .iter()
            .map(|continuation| continuation.call().id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            expected_continuation_ids[..=index],
            "request {index} should replay all raw tool exchanges in order"
        );
    }

    let lifecycle = lifecycle_kinds(&runtime.ledger_projection().await);
    let artifact_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ArtifactRecorded).then_some(index))
        .collect::<Vec<_>>();
    let resolved_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ToolCallResolved).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(artifact_indexes.len(), 7);
    assert_eq!(resolved_indexes.len(), 4);
    for (artifact_index, resolved_index) in artifact_indexes
        .iter()
        .take(resolved_indexes.len())
        .zip(resolved_indexes.iter())
    {
        assert!(
            artifact_index < resolved_index,
            "tool result artifact must be recorded before tool resolution"
        );
    }
}
