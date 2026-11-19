use crate::{
    FileSessionStore, LoadedSession, RuntimeError, SessionStoreError, StepContext,
    runtime::{
        Runtime, RuntimeBuilder,
        tests::support::{
            common::{collect_step, model_name, session_id},
            model_provider::RecordingModelProvider,
            tool_executors::SuccessfulToolExecutor,
            tool_helpers::registered_tool_spec,
        },
    },
    tool::RegisteredTool,
};
use merry_core::{
    ExternalToolBinding, ToolAdapterId, ToolBindingName, ToolName, ToolSourceFingerprint,
    ToolSourceId, ToolSpec,
};
use serde_json::json;
use std::sync::Arc;

fn external_tool(name: &str, description: &str) -> RegisteredTool {
    let spec = ToolSpec::new(
        ToolName::new(name).unwrap(),
        description,
        registered_tool_spec().input_schema().clone(),
    )
    .unwrap();
    RegisteredTool::read_only(spec, Arc::new(SuccessfulToolExecutor::new())).with_external_binding(
        ExternalToolBinding::new(
            ToolAdapterId::new("fixture").unwrap(),
            ToolSourceId::new("server").unwrap(),
            ToolBindingName::new(name).unwrap(),
            ToolSourceFingerprint::new("endpoint").unwrap(),
        ),
    )
}

#[tokio::test]
async fn external_catalog_is_not_persisted_and_resume_uses_live_tools() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = session_id("external-catalog");
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(id.clone())
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(external_tool("zeta", "Stable zeta"))
        .register_tool(external_tool("alpha", "Stable alpha"))
        .build()
        .unwrap();
    collect_step(&runtime, "before resume", StepContext::default()).await;
    runtime.save_session_to(store.clone()).await.unwrap();

    let journal = tokio::fs::read_to_string(temp.path().join(id.as_str()).join("journal.jsonl"))
        .await
        .unwrap();
    assert!(
        !journal.contains("external_tool_catalog"),
        "external_tool_catalog must not be persisted in journal.jsonl"
    );

    let resumed = Runtime::builder(id)
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(external_tool("zeta", "Stable zeta"))
        .register_tool(external_tool("alpha", "Stable alpha"))
        .resume_from_store(store)
        .await
        .unwrap();
    collect_step(&resumed, "after resume", StepContext::default()).await;
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash()
    );
}

#[tokio::test]
async fn resume_accepts_live_external_tool_changes_without_mismatch_error() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = session_id("catalog-drift");
    let original = || vec![external_tool("one", "One"), external_tool("two", "Two")];
    let runtime = original()
        .into_iter()
        .fold(Runtime::builder(id.clone()), RuntimeBuilder::register_tool)
        .build()
        .unwrap();
    runtime.save_session_to(store.clone()).await.unwrap();
    let variants = [
        vec![],
        vec![external_tool("one", "One")],
        vec![
            external_tool("one", "One"),
            external_tool("two", "Two"),
            external_tool("three", "Three"),
        ],
        vec![external_tool("two", "Two"), external_tool("one", "One")],
        vec![external_tool("one", "Changed"), external_tool("two", "Two")],
    ];
    for tools in variants {
        let expected_len = tools.len();
        let resumed = tools
            .into_iter()
            .fold(Runtime::builder(id.clone()), RuntimeBuilder::register_tool)
            .resume_from_store(store.clone())
            .await
            .expect("resume should accept live external tool changes");
        assert_eq!(
            resumed.external_tool_catalog().await.entries().len(),
            expected_len
        );
    }
}

#[tokio::test]
async fn resume_rejects_unsupported_formats_without_migrating_the_document() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = session_id("unsupported-format");
    Runtime::builder(id.clone())
        .build()
        .unwrap()
        .save_session_to(store.clone())
        .await
        .unwrap();
    let path = temp.path().join(id.as_str()).join("journal.jsonl");
    let original = tokio::fs::read_to_string(&path).await.unwrap();
    let mut lines = original
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    for version in [3, 4, 5] {
        header["format_version"] = json!(version);
        lines[0] = serde_json::to_string(&header).unwrap();
        let bytes = format!("{}\n", lines.join("\n"));
        tokio::fs::write(&path, &bytes).await.unwrap();
        assert!(matches!(
            LoadedSession::load(&store, &id).await,
            Err(RuntimeError::SessionStore {
                source: SessionStoreError::UnsupportedFormatVersion { actual }
            }) if actual == version
        ));
        assert!(matches!(
            Runtime::builder(id.clone()).resume_from_store(store.clone()).await,
            Err(RuntimeError::SessionStore {
                source: SessionStoreError::UnsupportedFormatVersion { actual }
            }) if actual == version
        ));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes.as_bytes());
    }
}
