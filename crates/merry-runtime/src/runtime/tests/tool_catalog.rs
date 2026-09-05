use super::*;
use crate::{FileSessionStore, LoadedSession, SessionStoreError};
use merry_core::{
    ExternalToolBinding, ToolAdapterId, ToolBindingName, ToolSourceFingerprint, ToolSourceId,
};

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
async fn external_catalog_round_trip_keeps_actual_provider_tools_and_prefix() {
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
    assert_eq!(
        LoadedSession::load(&store, &id)
            .await
            .unwrap()
            .external_tool_catalog()
            .clone(),
        runtime.external_tool_catalog().await
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
async fn resume_rejects_catalog_removal_addition_reordering_and_definition_changes() {
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
        let result = tools
            .into_iter()
            .fold(Runtime::builder(id.clone()), RuntimeBuilder::register_tool)
            .resume_from_store(store.clone())
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::ExternalToolCatalogMismatch)
        ));
    }
}

#[tokio::test]
async fn an_empty_catalog_stays_empty_after_resume() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = session_id("empty-catalog");
    let runtime = Runtime::builder(id.clone()).build().unwrap();
    runtime.save_session_to(store.clone()).await.unwrap();
    assert!(
        LoadedSession::load(&store, &id)
            .await
            .unwrap()
            .external_tool_catalog()
            .entries()
            .is_empty()
    );
    let result = Runtime::builder(id)
        .register_tool(external_tool("late", "Late"))
        .resume_from_store(store)
        .await;
    assert!(matches!(
        result,
        Err(RuntimeError::ExternalToolCatalogMismatch)
    ));
}

#[tokio::test]
async fn catalog_loader_rejects_missing_or_null_catalogs_instead_of_rediscovering() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = session_id("corrupt-catalog");
    Runtime::builder(id.clone())
        .build()
        .unwrap()
        .save_session_to(store.clone())
        .await
        .unwrap();
    let path = temp.path().join(id.as_str()).join("state.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    for catalog in [None, Some(serde_json::Value::Null)] {
        match catalog {
            None => {
                document
                    .as_object_mut()
                    .unwrap()
                    .remove("external_tool_catalog");
            }
            Some(catalog) => document["external_tool_catalog"] = catalog,
        }
        tokio::fs::write(&path, serde_json::to_vec(&document).unwrap())
            .await
            .unwrap();
        assert!(matches!(
            LoadedSession::load(&store, &id).await,
            Err(RuntimeError::SessionStore {
                source: SessionStoreError::Json { .. }
            })
        ));
        assert!(matches!(
            Runtime::builder(id.clone())
                .resume_from_store(store.clone())
                .await,
            Err(RuntimeError::SessionStore {
                source: SessionStoreError::Json { .. }
            })
        ));
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
    let path = temp.path().join(id.as_str()).join("state.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    document
        .as_object_mut()
        .unwrap()
        .remove("external_tool_catalog");
    for version in [3, 5] {
        document["format_version"] = json!(version);
        let bytes = serde_json::to_vec(&document).unwrap();
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
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
    }
}
