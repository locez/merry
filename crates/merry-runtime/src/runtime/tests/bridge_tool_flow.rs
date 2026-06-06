use super::*;

#[test]
fn default_action_policy_matches_mvp_hard_policy() {
    let policy = DefaultActionPolicy;

    let read_only = policy.decide(ToolActionKind::ReadOnly);
    assert_eq!(read_only.action_kind(), ToolActionKind::ReadOnly);
    assert_eq!(read_only.risk_tier(), ActionRiskTier::ReadOnly);
    assert_eq!(read_only.disposition(), ActionPolicyDisposition::Allow);
    assert!(read_only.is_allowed());

    let runtime_control = policy.decide(ToolActionKind::RuntimeControl);
    assert_eq!(
        runtime_control.action_kind(),
        ToolActionKind::RuntimeControl
    );
    assert_eq!(runtime_control.risk_tier(), ActionRiskTier::RuntimeControl);
    assert_eq!(
        runtime_control.disposition(),
        ActionPolicyDisposition::Allow
    );
    assert!(runtime_control.is_allowed());

    for (action_kind, risk_tier) in [
        (ToolActionKind::WorkspaceWrite, ActionRiskTier::EditElevated),
        (ToolActionKind::CommandExec, ActionRiskTier::ProcessHigh),
        (ToolActionKind::Network, ActionRiskTier::Forbidden),
    ] {
        let decision = policy.decide(action_kind);
        assert_eq!(decision.action_kind(), action_kind);
        assert_eq!(decision.risk_tier(), risk_tier);
        assert_eq!(decision.disposition(), ActionPolicyDisposition::Deny);
        assert!(!decision.is_allowed());
    }
}

#[test]
fn bridge_tool_registration_requires_explicit_builder_opt_in() {
    let result = Runtime::builder(session_id("runtime-bridge-tool-gate-deny"))
        .register_tool(RegisteredTool::bridge(policy_tool_spec("bridge_lookup")))
        .build();

    match result {
        Ok(_) => panic!("bridge tools require explicit builder opt-in"),
        Err(RuntimeError::BridgeToolsNotAllowed { name }) => {
            assert_eq!(name.as_str(), "bridge_lookup");
        }
        Err(other) => panic!("expected bridge gate error, got {other:?}"),
    }
}

#[test]
fn allow_bridge_tools_accepts_bridge_registered_tool() {
    let runtime = Runtime::builder(session_id("runtime-bridge-tool-gate-allow"))
        .allow_bridge_tools()
        .register_tool(RegisteredTool::bridge(policy_tool_spec("bridge_lookup")))
        .build()
        .expect("explicit bridge opt-in should allow bridge tool registration");

    assert!(
        runtime
            .inner
            .tool_registry
            .registered_tool(&ToolName::new("bridge_lookup").expect("valid tool name"))
            .is_some()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_tool_call_emits_bridge_request_event() {
    let call = model_tool_call("call-bridge-tool");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(vec![ModelOutput::tool_call(call)], FinishReason::ToolCalls),
        )])]);
    let runtime = Runtime::builder(session_id("runtime-bridge-tool-event"))
        .allow_bridge_tools()
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(RegisteredTool::bridge(policy_tool_spec("lookup")))
        .build()
        .expect("bridge opt-in should allow bridge tool registration");

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "BridgeToolCallRequested"
        ]
    );
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RuntimeEventKind::BridgeToolCallRequested { call }
            if call.id().as_str() == "call-bridge-tool"
                && call.name().as_str() == "lookup"
    )));
    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![pending_tool_call("call-bridge-tool")]
    );
    assert_eq!(provider.recorded_requests().len(), 1);
}
