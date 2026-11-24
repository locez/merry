use super::*;
use crate::config::XdgPaths;

#[test]
fn default_tui_session_id_is_filesystem_safe() {
    let session_id = default_tui_session_id();

    assert!(!session_id.as_str().is_empty());
    assert!(
        session_id
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    );
}

#[test]
fn tui_reasoning_preference_overrides_provider_default() {
    let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
    let config = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"
reasoning_effort = "low"
"#,
        ),
        Some(
            r#"
version = 1

[providers.compat]
type = "openai-compatible"
api_key_file = "managed/secrets/compat.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");
    let mut preferences = TuiPreferences::default();
    preferences
        .set_reasoning_effort_for_provider(
            "compat",
            Some(merry_llm::ReasoningEffort::new("high").unwrap()),
        )
        .expect("reasoning preference should be valid");

    let generation = generation_config_with_preferences(Some(&config), &preferences)
        .expect("generation config should build");

    assert_eq!(
        generation.reasoning_effort().map(|effort| effort.as_str()),
        Some("high")
    );
}

#[test]
fn selected_provider_reasoning_effort_is_used_when_preference_is_inherited() {
    let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
    let config = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"
"#,
        ),
        Some(
            r#"
version = 1

[providers.compat]
type = "openai-compatible"
api_key_file = "managed/secrets/compat.key"

[providers.alt]
type = "openai-compatible"
default_model = "alt-model"
reasoning_effort = "high"
api_key_file = "managed/secrets/alt.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");
    let mut preferences = TuiPreferences::default();
    preferences.provider = Some("alt".to_owned());

    let generation = generation_config_with_preferences(Some(&config), &preferences)
        .expect("generation config should build");

    assert_eq!(
        generation.reasoning_effort().map(|effort| effort.as_str()),
        Some("high")
    );
}

#[test]
fn selected_provider_does_not_inherit_the_default_provider_service_tier() {
    let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
    let config = MerryConfig::load_with_managed_from_text(
        Some(
            r#"
[providers.default]
provider = "compat"
model = "gpt-test"
service_tier = "priority"
"#,
        ),
        Some(
            r#"
version = 1

[providers.compat]
type = "openai-compatible"
api_key_file = "managed/secrets/compat.key"

[providers.alt]
type = "openai-compatible"
default_model = "alt-model"
api_key_file = "managed/secrets/alt.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");
    let mut preferences = TuiPreferences::default();
    preferences.provider = Some("alt".to_owned());

    let generation = generation_config_with_preferences(Some(&config), &preferences)
        .expect("generation config should build");

    assert_eq!(generation.service_tier(), None);

    preferences.provider = Some("compat".to_owned());
    let generation = generation_config_with_preferences(Some(&config), &preferences)
        .expect("generation config should build");

    assert_eq!(
        generation.service_tier(),
        Some(merry_llm::ServiceTier::Priority)
    );
}

#[test]
fn tui_compaction_strategies_only_change_retained_model_turns() {
    let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[runtime.auto_compaction]
target_output_tokens = 9000
max_accepted_output_bytes = 72000
retained_model_turns = 5
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should exist");

    for (strategy, expected_turns) in [
        (CompactionStrategy::Compact, 3),
        (CompactionStrategy::Balanced, 5),
        (CompactionStrategy::PreserveDetail, 7),
    ] {
        let mut preferences = TuiPreferences::default();
        preferences.compaction_strategy = Some(strategy);
        let policy = automatic_compaction_config_with_preferences(Some(&config), &preferences)
            .expect("TUI compaction config should build")
            .policy();

        assert_eq!(policy.retained_model_turns(), expected_turns);
        assert_eq!(policy.target_output_tokens(), Some(9_000));
        assert_eq!(policy.max_accepted_output_bytes(), Some(72_000));
    }
}

async fn test_tui_runtime_session(
    workspace: &std::path::Path,
    sessions_dir: &std::path::Path,
    session_id: &str,
    provider: Arc<dyn merry_llm::ModelProvider>,
) -> TuiRuntimeSession {
    use crate::coding::{CodingSubagentsConfig, fixed_process_backend};
    use crate::testing::{FakeProcessRunner, model_name};
    use merry_process::ProcessSession;
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, ProcessRunner,
        StaticPermissionedProcessRunnerFactory,
    };

    let session_store = TuiSessionStore::new(sessions_dir.to_path_buf());
    let state_store = session_store.session_state_store();
    let session_id = SessionId::new(session_id).expect("valid session id");
    let reservation = state_store
        .reserve_session(&session_id)
        .await
        .expect("session should reserve");
    let runner: Arc<dyn ProcessRunner> = Arc::new(FakeProcessRunner::succeeding("ok\n"));
    let permissioned_factory = Arc::new(StaticPermissionedProcessRunnerFactory::new(Arc::clone(
        &runner,
    )));
    let (permission_source, permission_requests) = ChannelPermissionAdmissionSource::channel(8);
    let permission_source = Arc::new(permission_source);
    let permission_policy = CodingPermissionPolicy::for_permission_mode(
        PermissionMode::AlwaysApprove.coding_mode(),
        AutoFallback::Deny.coding_fallback(),
        Some(Arc::clone(&permission_source) as Arc<dyn merry_runtime::PermissionAdmissionSource>),
    )
    .expect("permission policy should build");
    let coding_runtime = build_headless_coding_with_policy_composition(
        HeadlessCodingRuntimeInput {
            session_id: session_id.as_str(),
            root: workspace,
            provider,
            model: model_name(),
            process_backend: fixed_process_backend(ProcessSession::from_parts(
                AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
                runner,
                permissioned_factory,
            )),
            extra_tools: Vec::new(),
            automatic_compaction: CompactionConfig::disabled(),
            history_retention: merry_runtime::HistoryRetention::default(),
            retry_policy: None,
            context_compaction: None,
            approval_review: None,
            skill_roots: Vec::new(),
            denied_absolute_paths: Vec::new(),
            subagents: CodingSubagentsConfig::default(),
            workspace_tool_limits: None,
            workspace_writable: true,
        },
        permission_policy,
        merry_runtime::PermissionReviewRules::default(),
        Some(state_store),
        None,
    )
    .expect("coding runtime should build");
    let loop_config = coding_runtime.loop_config();
    let runtime = coding_runtime.into_runtime();
    let interactive = runtime
        .start_interactive_agent_run(StepContext::default(), loop_config)
        .expect("interactive run should start");
    let subagent_activity = runtime.subscribe_subagent_activity();
    let (stream, input, control) = interactive.split();
    let mut metadata = TuiSessionMetadata::new(session_id, workspace.to_path_buf(), now_unix_ms());
    metadata.model = Some("test-model".to_owned());

    TuiRuntimeSession {
        workspace_root: workspace.to_path_buf(),
        model_label: "test-model".to_owned(),
        reasoning_effort_label: None,
        metadata,
        metadata_written: false,
        resumed: false,
        startup_warnings: Vec::new(),
        runtime,
        session_store,
        stream,
        subagent_activity,
        input,
        control,
        skills: Vec::new(),
        merry_config: MerryConfig::load_optional_from_text(
            Some(""),
            &XdgPaths::from_parts(workspace.to_path_buf(), None, None),
        )
        .expect("empty config should parse")
        .expect("empty config should exist"),
        explicit_cli_flags: crate::cli::ExplicitCliFlags::default(),
        configured_permission_mode: PermissionMode::AlwaysApprove,
        configured_auto_fallback: AutoFallback::Deny,
        permission_source,
        permission_requests,
        pending_permission_reviews: VecDeque::new(),
        session_reservation: Some(reservation),
    }
}

#[tokio::test]
async fn untouched_session_writes_no_metadata_and_cleans_up_on_discard() {
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let sessions = tempfile::tempdir().expect("sessions tempdir");
    let provider = Arc::new(crate::testing::ScriptedProvider::new(Vec::new()));
    let mut session = test_tui_runtime_session(
        workspace.path(),
        sessions.path(),
        "empty-tui-session",
        provider,
    )
    .await;
    let session_id = session.metadata.session_id.clone();
    let store = TuiSessionStore::new(sessions.path().to_path_buf());

    assert!(!session.is_metadata_written());
    assert!(!store.metadata_path(&session_id).exists());
    assert!(!session.has_persistable_activity().await);

    session.discard_if_empty().await;

    assert!(
        !store
            .session_state_store()
            .session_dir(&session_id)
            .exists()
    );
    assert!(
        store
            .sessions_for_workspace(workspace.path())
            .expect("sessions should list")
            .is_empty()
    );
}

#[tokio::test]
async fn submit_writes_metadata_immediately_so_mid_turn_crash_is_resumable() {
    use futures_util::stream::{self, StreamExt};
    use merry_core::{ProviderName, RuntimeEvent};
    use merry_llm::{
        ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
        ModelProviderFuture, ModelRequest, ModelStreamContext,
    };
    use merry_runtime::{InteractiveRunMessage, UserMessageInput};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MidStreamStallingProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
        calls: AtomicUsize,
    }

    impl ModelProvider for MidStreamStallingProvider {
        fn name(&self) -> &ProviderName {
            &self.name
        }

        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn stream_model<'a>(
            &'a self,
            _request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if call == 0 {
                    let event = crate::testing::process_tool_call("call-1", &["pwd"], Some("."))
                        .expect("tool call event should build");
                    Ok(Box::pin(stream::iter(vec![Ok(event)])) as ModelEventStream)
                } else {
                    // Emit a partial delta and keep the stream open so turn 1 cannot settle
                    // before the simulated mid-stream crash drops the session.
                    let head = stream::iter(vec![
                        Ok(ModelEvent::Started),
                        Ok(ModelEvent::OutputTextDelta {
                            delta: "partial `【完】`".to_owned(),
                        }),
                    ]);
                    Ok(Box::pin(head.chain(stream::pending())) as ModelEventStream)
                }
            })
        }
    }

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let sessions = tempfile::tempdir().expect("sessions tempdir");
    let provider = Arc::new(MidStreamStallingProvider {
        name: ProviderName::new("mid-stream-crash-provider").expect("valid provider name"),
        capabilities: ModelCapabilities::new(true, true, false, true, None, None)
            .expect("valid capabilities"),
        calls: AtomicUsize::new(0),
    });
    let mut session = test_tui_runtime_session(
        workspace.path(),
        sessions.path(),
        "mid-turn-crash-session",
        provider,
    )
    .await;
    let session_id = session.metadata.session_id.clone();
    let store = TuiSessionStore::new(sessions.path().to_path_buf());

    assert!(!store.metadata_path(&session_id).exists());
    session
        .submit_message(
            UserMessageInput::text_only("  fix   crash\nresume  ").expect("valid input"),
            "  fix   crash\nresume  ",
        )
        .await
        .expect("submit should succeed");
    assert!(session.is_metadata_written());
    assert!(store.metadata_path(&session_id).exists());

    // Advance until tool 1 finishes (writing journal.jsonl savepoint) AND step 2 is
    // actively streaming, then drop the session without settling or saving on exit.
    let mut saw_tool_finished = false;
    let mut saw_mid_stream_delta = false;
    while let Some(message) = session
        .stream
        .next_message()
        .await
        .expect("stream should yield")
    {
        match message {
            InteractiveRunMessage::Event(RuntimeEvent::ToolCallFinished { .. }) => {
                saw_tool_finished = true;
            }
            InteractiveRunMessage::Event(RuntimeEvent::AssistantMessageDelta { .. }) => {
                saw_mid_stream_delta = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        saw_tool_finished,
        "tool call should complete before simulated crash"
    );
    assert!(
        saw_mid_stream_delta,
        "step 2 should be mid-stream when simulated crash happens"
    );
    drop(session);

    let listed = store
        .sessions_for_workspace(workspace.path())
        .expect("sessions should list after mid-turn crash");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, session_id);
    assert_eq!(listed[0].topic.as_deref(), Some("fix crash resume"));
    LoadedSession::load(&store.session_state_store(), &session_id)
        .await
        .expect("mid-turn savepoint should load cleanly for resume");
}
