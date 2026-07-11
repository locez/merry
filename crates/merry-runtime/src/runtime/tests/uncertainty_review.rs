use super::*;

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_preflight_rejects_unreadable_evidence() {
    let runtime = Runtime::builder(session_id("uncertainty-preflight"))
        .build()
        .expect("runtime builds");
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("review-preflight-call"))
            .expect("pending tool call records");
    }
    let before = judgment_harness_state(&runtime).await;
    let request = tool_risk_review_request(vec![judgment_evidence(
        "missing request evidence",
        "missing-review-source",
        EvidenceLocator::whole_artifact(),
    )]);
    let source = ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(Vec::new()));

    let error = runtime
        .run_uncertainty_review(&source, request, CancellationToken::new())
        .await
        .expect_err("missing request evidence rejects before source invocation");

    assert!(matches!(
        error,
        JudgmentError::UnreadableEvidence {
            artifact_id,
            source: ArtifactError::MissingArtifact { .. },
        } if artifact_id.as_str() == "missing-review-source"
    ));
    assert_eq!(source.call_count(), 0);
    assert_eq!(judgment_harness_state(&runtime).await, before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_records_one_internal_payload_and_no_public_state() {
    let runtime = Runtime::builder(session_id("uncertainty-success"))
        .build()
        .expect("runtime builds");
    record_memory_artifact(
        &runtime,
        "review-source",
        "lookup input may include credential-like material\n",
    );
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("review-success-call"))
            .expect("pending tool call records");
    }
    let evidence = judgment_evidence(
        "lookup input",
        "review-source",
        EvidenceLocator::whole_artifact(),
    );
    let request = tool_risk_review_request(vec![evidence.clone()]);
    let source = ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(vec![evidence]));
    let public_before = {
        let mut state = judgment_harness_state(&runtime).await;
        state.judgment_records.clear();
        state
    };

    let record = runtime
        .run_uncertainty_review(&source, request, CancellationToken::new())
        .await
        .expect("valid uncertainty review records");

    assert_eq!(source.call_count(), 1);
    assert_eq!(record.id().as_str(), "judgment-record-00000000000000000000");
    assert_eq!(record.request().purpose(), JudgmentPurpose::ToolRiskReview);
    assert_eq!(record.outcome().purpose(), JudgmentPurpose::ToolRiskReview);
    assert_eq!(record.outcome().confidence().as_f32(), 0.95);
    assert_eq!(
        record.outcome().uncertainty(),
        "This advisory review cannot authorize or block tool execution."
    );
    assert_eq!(
        record.outcome().provenance().source_kind(),
        JudgmentSourceKind::Test
    );
    assert_eq!(
        record.outcome().provenance().source_label(),
        "runtime scripted source"
    );
    match record.outcome().recommendation() {
        JudgmentRecommendation::ToolRiskReview { risk, concerns } => {
            assert_eq!(*risk, JudgmentRiskLevel::High);
            assert_eq!(
                concerns,
                &["Input references credential-like material.".to_owned()]
            );
        }
        other => panic!("expected tool risk review recommendation, got {other:?}"),
    }
    assert!(
        record
            .artifacts()
            .request()
            .content()
            .contains("purpose=tool_risk_review\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("recommendation.risk=high\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("confidence=0.950000\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("provenance.payload=test\n")
    );

    let after = judgment_harness_state(&runtime).await;
    assert_eq!(after.judgment_records, vec![record]);
    let public_after = JudgmentHarnessState {
        judgment_records: Vec::new(),
        ..after
    };
    assert_eq!(public_after, public_before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_model_backed_source_records_llm_judgment_and_no_public_state() {
    let runtime = Runtime::builder(session_id("uncertainty-model-backed-success"))
        .build()
        .expect("runtime builds");
    record_memory_artifact(
        &runtime,
        "model-backed-review-source",
        "lookup input includes customer token material\n",
    );
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("model-backed-review-call"))
            .expect("pending tool call records");
    }
    let evidence = judgment_evidence(
        "lookup input",
        "model-backed-review-source",
        EvidenceLocator::whole_artifact(),
    );
    let request = tool_risk_review_request(vec![evidence.clone()]);
    let provider = RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(
        vec![Ok(completed_event_with(
            vec![ModelOutput::text(
                model_tool_risk_judgment_json(
                    "high",
                    "The lookup input may expose credential-like customer material.",
                    0,
                    "lookup input",
                    0.82,
                    "The cited input contains material that should be treated as sensitive before tool policy decides.",
                    "This model judgment is advisory only and cannot authorize or block tool execution.",
                )
                .as_str(),
            )],
            FinishReason::Stop,
        ))],
    )]);
    let source = model_backed_judgment_source(provider.clone(), "runtime model-backed source");
    let public_before = {
        let mut state = judgment_harness_state(&runtime).await;
        state.judgment_records.clear();
        state
    };

    let record = runtime
        .run_uncertainty_review(&source, request, CancellationToken::new())
        .await
        .expect("valid model-backed uncertainty review records");

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let after = judgment_harness_state(&runtime).await;
    assert_eq!(after.judgment_records, vec![record.clone()]);
    assert_eq!(after.judgment_records.len(), 1);
    assert_eq!(
        record.outcome().provenance().source_kind(),
        JudgmentSourceKind::Llm
    );
    assert_eq!(
        record.outcome().provenance().source_label(),
        "runtime model-backed source"
    );
    assert_eq!(record.outcome().evidence(), std::slice::from_ref(&evidence));
    assert_eq!(record.outcome().confidence().as_f32(), 0.82);
    assert_eq!(
        record.outcome().rationale(),
        "The cited input contains material that should be treated as sensitive before tool policy decides."
    );
    assert_eq!(
        record.outcome().uncertainty(),
        "This model judgment is advisory only and cannot authorize or block tool execution."
    );
    match record.outcome().recommendation() {
        JudgmentRecommendation::ToolRiskReview { risk, concerns } => {
            assert_eq!(*risk, JudgmentRiskLevel::High);
            assert_eq!(
                concerns,
                &["The lookup input may expose credential-like customer material.".to_owned()]
            );
        }
        other => panic!("expected tool risk review recommendation, got {other:?}"),
    }
    assert!(
        record
            .artifacts()
            .request()
            .content()
            .contains("purpose=tool_risk_review\n")
    );
    assert!(
        record
            .artifacts()
            .request()
            .content()
            .contains("evidence.0.label=lookup input\n")
    );
    assert!(
        record
            .artifacts()
            .request()
            .content()
            .contains("evidence.0.artifact_id=model-backed-review-source\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("recommendation.risk=high\n")
    );
    assert!(record.artifacts().outcome().content().contains(
        "recommendation.concerns.0=The lookup input may expose credential-like customer material.\n"
    ));
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("confidence=0.820000\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("evidence.0.label=lookup input\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("evidence.0.artifact_id=model-backed-review-source\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("provenance.payload=llm\n")
    );
    assert!(
        record
            .artifacts()
            .outcome()
            .content()
            .contains("provenance.label=runtime model-backed source\n")
    );

    let public_after = JudgmentHarnessState {
        judgment_records: Vec::new(),
        ..after
    };
    assert_eq!(public_after, public_before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_model_backed_source_preflight_rejects_unreadable_evidence_before_provider_call()
 {
    let runtime = Runtime::builder(session_id("uncertainty-model-backed-preflight"))
        .build()
        .expect("runtime builds");
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("model-backed-preflight-call"))
            .expect("pending tool call records");
    }
    let request = tool_risk_review_request(vec![judgment_evidence(
        "missing lookup input",
        "missing-model-backed-review-source",
        EvidenceLocator::whole_artifact(),
    )]);
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text(
                    model_tool_risk_judgment_json(
                        "low",
                        "No model call should be made for unreadable evidence.",
                        0,
                        "missing lookup input",
                        0.2,
                        "Unreadable evidence should fail preflight before semantic judgment.",
                        "No uncertainty should be recorded because the source must not run.",
                    )
                    .as_str(),
                )],
                FinishReason::Stop,
            ),
        )])]);
    let source = model_backed_judgment_source(provider.clone(), "runtime model-backed source");
    let before = judgment_harness_state(&runtime).await;

    let error = runtime
        .run_uncertainty_review(&source, request, CancellationToken::new())
        .await
        .expect_err("missing request evidence rejects before provider invocation");

    assert!(matches!(
        error,
        JudgmentError::UnreadableEvidence {
            artifact_id,
            source: ArtifactError::MissingArtifact { .. },
        } if artifact_id.as_str() == "missing-model-backed-review-source"
    ));
    assert!(provider.recorded_requests().is_empty());
    assert_eq!(judgment_harness_state(&runtime).await, before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_model_backed_source_invalid_model_output_records_nothing() {
    let runtime = Runtime::builder(session_id("uncertainty-model-backed-invalid-output"))
        .build()
        .expect("runtime builds");
    record_memory_artifact(
        &runtime,
        "model-backed-invalid-source",
        "lookup input is readable for invalid model output test\n",
    );
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("model-backed-invalid-call"))
            .expect("pending tool call records");
    }
    let request = tool_risk_review_request(vec![judgment_evidence(
        "lookup input",
        "model-backed-invalid-source",
        EvidenceLocator::whole_artifact(),
    )]);
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text("not strict judgment json")],
                FinishReason::Stop,
            ),
        )])]);
    let source = model_backed_judgment_source(provider.clone(), "runtime model-backed source");
    let before = judgment_harness_state(&runtime).await;

    let error = runtime
        .run_uncertainty_review(&source, request, CancellationToken::new())
        .await
        .expect_err("invalid model output rejects before registry write");

    assert_eq!(error, JudgmentError::InvalidModelJudgmentOutput);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(judgment_harness_state(&runtime).await, before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_rejects_bad_outcome_evidence_without_registry_write() {
    let runtime = Runtime::builder(session_id("uncertainty-bad-outcome"))
        .build()
        .expect("runtime builds");
    record_memory_artifact(&runtime, "review-request-source", "request evidence\n");
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("review-bad-outcome-call"))
            .expect("pending tool call records");
    }
    let request = tool_risk_review_request(vec![judgment_evidence(
        "request source",
        "review-request-source",
        EvidenceLocator::whole_artifact(),
    )]);
    let source =
        ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(vec![judgment_evidence(
            "missing outcome source",
            "missing-outcome-source",
            EvidenceLocator::whole_artifact(),
        )]));
    let before = judgment_harness_state(&runtime).await;

    let error = runtime
        .run_uncertainty_review(&source, request, CancellationToken::new())
        .await
        .expect_err("missing outcome evidence rejects before registry write");

    assert!(matches!(
        error,
        JudgmentError::UnreadableEvidence {
            artifact_id,
            source: ArtifactError::MissingArtifact { .. },
        } if artifact_id.as_str() == "missing-outcome-source"
    ));
    assert_eq!(source.call_count(), 1);
    assert_eq!(judgment_harness_state(&runtime).await, before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_pre_cancelled_token_skips_source_and_state_change() {
    let runtime = Runtime::builder(session_id("uncertainty-pre-cancelled"))
        .build()
        .expect("runtime builds");
    let source = ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(Vec::new()));
    let before = judgment_harness_state(&runtime).await;
    let token = CancellationToken::new();
    token.cancel();

    let error = runtime
        .run_uncertainty_review(&source, tool_risk_review_request(Vec::new()), token)
        .await
        .expect_err("pre-cancelled token rejects");

    assert_eq!(error, JudgmentError::Cancelled);
    assert_eq!(source.call_count(), 0);
    assert_eq!(judgment_harness_state(&runtime).await, before);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_cancelled_while_source_future_in_flight_records_nothing() {
    let runtime = Runtime::builder(session_id("uncertainty-in-flight-cancel"))
        .build()
        .expect("runtime builds");
    {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_test_tool_call_pending(pending_tool_call("review-in-flight-call"))
            .expect("pending tool call records");
    }
    let before = judgment_harness_state(&runtime).await;
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let source = ScriptedJudgmentSource::new(vec![
        ScriptedJudgmentResponse::PendingUntilReleasedOrCancelled {
            started: started_tx,
            release: release_rx,
            outcome: high_tool_risk_outcome(Vec::new()),
        },
    ]);
    let token = CancellationToken::new();
    let review = {
        let runtime = runtime.clone();
        let source = source.clone();
        let token = token.clone();
        tokio::spawn(async move {
            runtime
                .run_uncertainty_review(&source, tool_risk_review_request(Vec::new()), token)
                .await
        })
    };

    started_rx.await.expect("judgment source future starts");
    assert_eq!(source.call_count(), 1);

    token.cancel();
    let error = review
        .await
        .expect("review task should not panic")
        .expect_err("in-flight cancellation rejects");

    assert_eq!(error, JudgmentError::Cancelled);
    assert_eq!(judgment_harness_state(&runtime).await, before);
    drop(release_tx);
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_source_error_or_cancel_records_nothing() {
    for (session, response) in [
        (
            "uncertainty-source-error",
            ScriptedJudgmentResponse::Error(JudgmentError::BlankField {
                field: "scripted source failure",
            }),
        ),
        (
            "uncertainty-source-cancel",
            ScriptedJudgmentResponse::Cancelled,
        ),
    ] {
        let runtime = Runtime::builder(session_id(session))
            .build()
            .expect("runtime builds");
        {
            let mut state = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            state
                .record_test_tool_call_pending(pending_tool_call(&format!("{session}-call")))
                .expect("pending tool call records");
        }
        let before = judgment_harness_state(&runtime).await;
        let source = ScriptedJudgmentSource::new(vec![response]);

        let error = runtime
            .run_uncertainty_review(
                &source,
                tool_risk_review_request(Vec::new()),
                CancellationToken::new(),
            )
            .await
            .expect_err("source failure rejects");

        assert!(matches!(
            error,
            JudgmentError::BlankField {
                field: "scripted source failure",
            } | JudgmentError::Cancelled
        ));
        assert_eq!(source.call_count(), 1);
        assert_eq!(judgment_harness_state(&runtime).await, before);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn uncertainty_review_high_and_unknown_tool_risk_remain_non_authoritative() {
    for (session, outcome) in [
        ("uncertainty-high-risk", high_tool_risk_outcome(Vec::new())),
        (
            "uncertainty-unknown-risk",
            unknown_tool_risk_outcome(Vec::new()),
        ),
    ] {
        let runtime = Runtime::builder(session_id(session))
            .build()
            .expect("runtime builds");
        {
            let mut state = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            state
                .record_test_tool_call_pending(pending_tool_call(&format!("{session}-call")))
                .expect("pending tool call records");
        }
        let public_before = {
            let mut state = judgment_harness_state(&runtime).await;
            state.judgment_records.clear();
            state
        };
        let source = ScriptedJudgmentSource::with_outcome(outcome);

        let record = runtime
            .run_uncertainty_review(
                &source,
                tool_risk_review_request(Vec::new()),
                CancellationToken::new(),
            )
            .await
            .expect("advisory tool risk review records");

        assert_eq!(source.call_count(), 1);
        assert!(matches!(
            record.outcome().recommendation(),
            JudgmentRecommendation::ToolRiskReview {
                risk: JudgmentRiskLevel::High | JudgmentRiskLevel::Unknown,
                ..
            }
        ));
        let after = judgment_harness_state(&runtime).await;
        assert_eq!(after.judgment_records.len(), 1);
        let public_after = JudgmentHarnessState {
            judgment_records: Vec::new(),
            ..after
        };
        assert_eq!(public_after, public_before);
    }
}
