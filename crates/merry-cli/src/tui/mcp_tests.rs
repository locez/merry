use super::{
    keymap::Keymap, project_mcp_startup_warnings, render::render_to_text, state::TuiState,
    theme::TuiTheme,
};
use merry_core::ToolSourceId;
use merry_mcp::{McpDiscoveryStage, McpFailureKind, McpServerDiagnostic, McpServerIssue};

#[test]
fn mcp_warnings_remain_visible_in_the_tui_after_startup() {
    let mut state = TuiState::new(
        std::path::PathBuf::from("workspace"),
        "fixture".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let warning = McpServerDiagnostic::new(
        ToolSourceId::new("offline").unwrap(),
        McpServerIssue::Unavailable {
            stage: McpDiscoveryStage::Initialize,
            failure: McpFailureKind::Connection,
        },
        2,
    );
    project_mcp_startup_warnings(&mut state, &[warning]);
    let first_frame = render_to_text(&state, 120, 35);
    assert!(first_frame.contains("MCP startup warning"));
    assert!(first_frame.contains("MCP offline:"));
    let wrapped_text = first_frame.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        wrapped_text.contains("2 saved tool definitions retained"),
        "{first_frame}"
    );
    assert_eq!(render_to_text(&state, 120, 35), first_frame);
}
