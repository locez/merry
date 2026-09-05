use super::*;
use crate::config::XdgPaths;
use merry_core::{SessionId, ToolSourceId};
use merry_mcp::{McpDiscoveryStage, McpFailureKind, McpServerIssue};
use merry_runtime::{FileSessionStore, LoadedSession, Runtime, RuntimeError};

#[tokio::test]
async fn a_new_session_without_mcp_has_no_warnings_or_tools() {
    let result = discover_configured_mcp_tools(None, McpSession::New)
        .await
        .unwrap();
    assert!(result.tools.is_empty());
    assert!(result.warnings.is_empty());
}

#[tokio::test]
async fn resume_uses_the_empty_saved_catalog_instead_of_new_configured_servers() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path().join("sessions"));
    let id = SessionId::new("empty-saved").unwrap();
    Runtime::builder(id.clone())
        .build()
        .unwrap()
        .save_session_to(store.clone())
        .await
        .unwrap();
    let paths = XdgPaths::from_parts(temp.path().to_path_buf(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some("[mcp.new]\nurl = 'http://127.0.0.1:9/mcp'"),
        &paths,
    )
    .unwrap()
    .unwrap();
    let loaded = LoadedSession::load(&store, &id).await.unwrap();
    let result = discover_configured_mcp_tools(
        Some(&config),
        McpSession::Resumed {
            catalog: loaded.external_tool_catalog(),
        },
    )
    .await
    .unwrap();
    assert!(result.tools.is_empty());
    assert_eq!(
        result.warnings[0].issue(),
        &McpServerIssue::NotInSessionCatalog
    );
}

#[tokio::test]
async fn unsupported_session_format_fails_without_discovery_or_migration() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = SessionId::new("unsupported-format").unwrap();
    Runtime::builder(id.clone())
        .build()
        .unwrap()
        .save_session_to(store.clone())
        .await
        .unwrap();
    let path = temp.path().join(id.as_str()).join("state.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    document["format_version"] = serde_json::json!(3);
    document
        .as_object_mut()
        .unwrap()
        .remove("external_tool_catalog");
    let bytes = serde_json::to_vec(&document).unwrap();
    tokio::fs::write(&path, &bytes).await.unwrap();
    let error = LoadedSession::load(&store, &id)
        .await
        .err()
        .expect("unsupported formats must fail");
    assert!(
        matches!(error, RuntimeError::SessionStore { source } if source.to_string().contains("session document format version 3 is not supported"))
    );
    assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
}

#[tokio::test]
async fn corrupt_resume_state_does_not_fall_back_to_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(temp.path());
    let id = SessionId::new("corrupt").unwrap();
    tokio::fs::create_dir(temp.path().join(id.as_str()))
        .await
        .unwrap();
    tokio::fs::write(temp.path().join(id.as_str()).join("state.json"), b"broken")
        .await
        .unwrap();
    let error = LoadedSession::load(&store, &id)
        .await
        .err()
        .expect("corrupt state must fail");
    assert!(matches!(error, RuntimeError::SessionStore { .. }));
}

#[tokio::test]
async fn warning_writer_reports_the_server_and_retained_count_and_observes_io_errors() {
    let warnings = vec![McpServerDiagnostic::new(
        ToolSourceId::new("offline").unwrap(),
        McpServerIssue::Unavailable {
            stage: McpDiscoveryStage::Initialize,
            failure: McpFailureKind::Timeout,
        },
        2,
    )];
    let mut bytes = Vec::new();
    write_startup_warnings(&mut bytes, &warnings).await.unwrap();
    let rendered = String::from_utf8(bytes).unwrap();
    assert!(rendered.starts_with("Warning: MCP offline:"));
    assert!(rendered.contains("2 saved tool definitions retained"));
    assert!(rendered.contains("Merry continues"));
    let (mut writer, reader) = tokio::io::duplex(16);
    drop(reader);
    assert!(
        write_startup_warnings(&mut writer, &warnings)
            .await
            .is_err()
    );
}
