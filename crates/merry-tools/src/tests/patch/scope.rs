//! Coverage for the workspace write scope and forbidden paths a patch must respect.

use super::*;

#[test]
fn apply_patch_respects_configured_write_scope() {
    let temp = TempWorkspace::new("patch-write-scope");
    temp.write_text("allowed/note.txt", "alpha\nold\nomega\n");
    temp.write_text("denied/note.txt", "alpha\nold\nomega\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(temp.path().to_path_buf())
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")])),
    )
    .expect("workspace tools should construct");

    let allowed = patch_outcome(&tools, "allowed/note.txt", "old", "new");
    assert_eq!(allowed.status(), ToolCallResultStatus::Succeeded);

    let denied = patch_outcome(&tools, "denied/note.txt", "old", "new");
    assert_failed_json_for_tool(
        &denied,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("denied/note.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&temp.path().join("denied/note.txt")),
        "alpha\nold\nomega\n"
    );
}

#[test]
fn apply_patch_forbidden_paths_override_write_scope() {
    let temp = TempWorkspace::new("patch-forbidden-scope");
    temp.write_text("allowed/public.txt", "alpha\nold\nomega\n");
    temp.write_text("allowed/secret.txt", "alpha\nold\nomega\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(temp.path().to_path_buf())
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")]))
            .with_forbidden_paths(vec![PathBuf::from("allowed/secret.txt")]),
    )
    .expect("workspace tools should construct");

    let public = patch_outcome(&tools, "allowed/public.txt", "old", "new");
    assert_eq!(public.status(), ToolCallResultStatus::Succeeded);

    let forbidden = patch_outcome(&tools, "allowed/secret.txt", "old", "new");
    assert_failed_json_for_tool(
        &forbidden,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("allowed/secret.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&temp.path().join("allowed/secret.txt")),
        "alpha\nold\nomega\n"
    );
}
