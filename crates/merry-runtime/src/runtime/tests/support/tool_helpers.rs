    fn registered_tool_spec() -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new("registered_tool").expect("valid tool name"),
            "Registered test tool",
            ToolInputSchema::new(schema).expect("valid tool schema"),
        )
        .expect("valid tool spec")
    }
    fn policy_tool_spec(name: &str) -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new(name).expect("valid tool name"),
            "Policy test tool",
            ToolInputSchema::new(schema).expect("valid tool schema"),
        )
        .expect("valid tool spec")
    }

    fn required_query_tool_spec(name: &str) -> ToolSpec {
        let schema = Schema::try_from(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "required": ["query"],
            "additionalProperties": false
        }))
        .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new(name).expect("valid tool name"),
            "Validated test tool",
            ToolInputSchema::new(schema).expect("valid tool schema"),
        )
        .expect("valid tool spec")
    }

    fn policy_pending_tool_call(id: &str, name: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        )
    }

    fn permission_pending_tool_call(id: &str, reason: &str, command: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(serde_json::json!({
                "reason": reason,
                "requested": { "network": true },
                "for_action": {
                    "kind": "process",
                    "command": command,
                    "cwd": ".",
                }
            }))
            .expect("valid permission arguments"),
        )
    }

    fn invalid_permission_pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(serde_json::json!({
                "requested": {
                    "paths": [
                        { "path": "/tmp/cache", "access": "ro" },
                        { "path": "/tmp/cache", "access": "rw" }
                    ]
                },
                "for_action": {
                    "kind": "process",
                    "command": "cargo test",
                    "cwd": ".",
                }
            }))
            .expect("valid JSON arguments"),
        )
    }

    fn resolved_tool_result(events: &[RuntimeJournalEvent]) -> &merry_core::ToolCallResult {
        events
            .iter()
            .find_map(|event| match &event.payload {
                RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("tool call should resolve")
    }

    async fn register_policy_pending_tool(
        session: &str,
        tool_name: &str,
        call_id: &str,
        action_kind: ToolActionKind,
        executor: impl ToolExecutor + 'static,
    ) -> (Runtime, PendingToolCall) {
        register_policy_pending_registered_tool(
            session,
            tool_name,
            call_id,
            RegisteredTool::new(policy_tool_spec(tool_name), Arc::new(executor), action_kind),
        )
        .await
    }

    async fn register_policy_pending_registered_tool(
        session: &str,
        tool_name: &str,
        call_id: &str,
        tool: RegisteredTool,
    ) -> (Runtime, PendingToolCall) {
        register_policy_pending_registered_tool_with_builder(
            session,
            tool_name,
            call_id,
            tool,
            RuntimeBuilder::build,
        )
        .await
    }

    async fn register_policy_pending_registered_tool_with_builder(
        session: &str,
        tool_name: &str,
        call_id: &str,
        tool: RegisteredTool,
        configure: impl FnOnce(RuntimeBuilder) -> Result<Runtime, RuntimeError>,
    ) -> (Runtime, PendingToolCall) {
        let spec = policy_tool_spec(tool_name);
        let pending = policy_pending_tool_call(call_id, spec.name().as_str());
        let runtime = configure(Runtime::builder(session_id(session)).register_tool(tool))
            .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session
                .record_test_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }
        (runtime, pending)
    }

    async fn register_permission_pending_tool_with_builder(
        session_id_value: &str,
        call_id: &str,
        configure: impl FnOnce(RuntimeBuilder) -> Result<Runtime, RuntimeError>,
    ) -> (Runtime, PendingToolCall) {
        let pending = permission_pending_tool_call(
            call_id,
            "Need network for this exact command.",
            "cargo test",
        );
        let runtime = configure(
            Runtime::builder(session_id(session_id_value))
                .register_tool(request_permissions_tool().expect("permission tool builds")),
        )
        .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session.record_test_user_message_body(
                "Please run cargo test; if network is blocked, request network for that command.",
            )
            .expect("user records");
            session
                .record_test_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }
        (runtime, pending)
    }

    async fn denied_action_content(
        runtime: &Runtime,
        events: &[RuntimeJournalEvent],
    ) -> serde_json::Value {
        let result = resolved_tool_result(events);
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("denial artifact should be readable");
        let text = content
            .as_text()
            .expect("denial artifact should be textual JSON");
        serde_json::from_str(text).expect("denial artifact should parse as JSON")
    }

    async fn action_audit_records(
        runtime: &Runtime,
    ) -> Vec<crate::action_audit::ActionAuditRecord> {
        let session = runtime.inner.session.lock().await;
        session.action_audit_snapshot().records().to_vec()
    }

    fn lifecycle_kinds(
        runtime_projection: &crate::LedgerProjectionSnapshot,
    ) -> Vec<LedgerFactKind> {
        runtime_projection
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
                LedgerProjection::Fact { .. } => None,
            })
            .collect()
    }

    fn assert_lifecycle_order(
        lifecycle_kinds: &[LedgerFactKind],
        before: LedgerFactKind,
        after: LedgerFactKind,
    ) {
        let before_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == before)
            .expect("before lifecycle kind should exist");
        let after_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == after)
            .expect("after lifecycle kind should exist");
        assert!(
            before_index < after_index,
            "{before:?} should be recorded before {after:?}"
        );
    }

    fn assert_sanitized_policy_denial_content(content: &serde_json::Value, tool_name: &str) {
        assert_eq!(
            content,
            &json!({
                "ok": false,
                "tool": tool_name,
                "error": {
                    "code": DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
                    "message": TOOL_ACTION_POLICY_DENIED_MESSAGE
                }
            })
        );
        assert!(content.get("call_id").is_none());
        assert!(content.get("action_kind").is_none());
        assert!(content.get("policy").is_none());
        assert!(content.get("reason").is_none());
        assert!(content.get("provider").is_none());
        assert!(content.get("provider_response").is_none());
        assert!(content.get("wire").is_none());
        assert!(content.get("previous_response_id").is_none());
    }

    fn event_kind_names_for_tool_execution(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event.payload {
                RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
                RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
                RuntimeJournalPayload::SessionUsageUpdated { .. } => "SessionUsageUpdated",
                RuntimeJournalPayload::SessionStarted => "SessionStarted",
                RuntimeJournalPayload::StepStarted => "StepStarted",
                RuntimeJournalPayload::StepCompleted => "StepCompleted",
                RuntimeJournalPayload::Cancelled { .. } => "Cancelled",
                RuntimeJournalPayload::Failed { .. } => "Failed",
                RuntimeJournalPayload::ToolCallPending { .. } => "ToolCallPending",
                RuntimeJournalPayload::EvidenceReferenced { .. } => "EvidenceReferenced",
                RuntimeJournalPayload::SkillUsed { .. } => "SkillUsed",
                _ => "Other",
            })
            .collect()
    }
