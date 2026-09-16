//! Tests for workspace path form and normalization.
//!
//! A tool path may be relative to a workspace root or absolute inside one, and
//! both spellings must address the same file without widening what may be read
//! or written. The reported path stays workspace-relative so tool output never
//! carries a host path back to the model.

use super::*;

/// Returns the canonical workspace root, which is what the tools resolve against.
fn canonical_root(temp: &TempWorkspace) -> PathBuf {
    fs::canonicalize(temp.path()).expect("workspace root should canonicalize")
}

#[test]
fn read_text_accepts_absolute_path_inside_the_workspace() {
    let temp = TempWorkspace::new("path-absolute-read");
    temp.write_text("dir/note.txt", "one\ntwo\n");
    let tools = tools_for(temp.path());
    let absolute = canonical_root(&temp).join("dir/note.txt");

    let outcome = read_outcome(
        &tools,
        absolute.to_str().expect("absolute path should be utf8"),
    );

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        json_content(&outcome)["path"],
        "dir/note.txt",
        "an absolute argument must be reported as a workspace-relative path"
    );
    assert!(
        !outcome
            .content()
            .as_text()
            .expect("json content")
            .contains(canonical_root(&temp).to_str().expect("root utf8")),
        "tool output must not include absolute host roots"
    );
}

#[test]
fn apply_patch_accepts_absolute_path_inside_the_workspace() {
    let temp = TempWorkspace::new("path-absolute-patch");
    temp.write_text("dir/note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let absolute = canonical_root(&temp).join("dir/note.txt");
    let patch = update_patch(
        absolute.to_str().expect("absolute path should be utf8"),
        "old",
        "new",
    );

    let outcome = patch_text_outcome(&tools, &patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        json_content(&outcome)["changes"][0]["path"],
        "dir/note.txt",
        "the change must be reported relative to the workspace root"
    );
    assert_eq!(
        read_text(&temp.path().join("dir/note.txt")),
        "alpha\nnew\nomega\n"
    );
}

#[test]
fn workspace_path_denies_absolute_path_outside_the_workspace() {
    let temp = TempWorkspace::new("path-absolute-outside");
    let tools = tools_for(temp.path());
    let root = canonical_root(&temp);
    let outside = root
        .parent()
        .expect("workspace root should have a parent")
        .join("merry-tools-outside-the-workspace.txt");
    let outside_text = outside.to_str().expect("outside path should be utf8");

    let outcome = read_outcome(&tools, outside_text);

    // The denial must not echo a host path back through a tool result.
    assert_failed_json(&outcome, ERROR_PATH_DENIED, None, root.as_path());
    assert!(
        json_content(&outcome)["error"]["message"]
            .as_str()
            .expect("message should be text")
            .contains("outside every configured workspace root"),
        "the denial must explain that an absolute path outside the workspace is not addressable"
    );
}

#[test]
fn workspace_path_normalizes_dot_segments_and_denies_escapes() {
    let temp = TempWorkspace::new("path-dot-segments");
    temp.write_text("dir/note.txt", "one\ntwo\n");
    let tools = tools_for(temp.path());

    let inside = read_outcome(&tools, "./dir/../dir/note.txt");
    assert_eq!(
        inside.status(),
        ToolCallResultStatus::Succeeded,
        "redundant dot segments name the same file and must not be rejected"
    );
    assert_eq!(json_content(&inside)["path"], "dir/note.txt");

    let escape = read_outcome(&tools, "dir/../../outside.txt");
    assert_failed_json(
        &escape,
        ERROR_PATH_DENIED,
        Some("dir/../../outside.txt"),
        temp.path(),
    );
    assert!(
        json_content(&escape)["error"]["message"]
            .as_str()
            .expect("message should be text")
            .contains("escapes the workspace root"),
        "a path that climbs above the root must be denied with an explicit reason"
    );
}

#[test]
fn workspace_path_denies_the_root_itself_and_prefix_lookalikes() {
    let temp = TempWorkspace::new("path-root-lookalike");
    temp.write_text("dir/note.txt", "one\ntwo\n");
    let tools = tools_for(temp.path());
    let root = canonical_root(&temp);
    let root_text = root.to_str().expect("root path should be utf8");

    let as_root = read_outcome(&tools, root_text);
    assert_failed_json(&as_root, ERROR_PATH_DENIED, None, root.as_path());
    assert!(
        json_content(&as_root)["error"]["message"]
            .as_str()
            .expect("message should be text")
            .contains("not the root itself"),
        "the root is a directory to browse, never a file to read"
    );

    // A sibling directory that merely shares the root's name prefix is not
    // inside the workspace, so prefix matching must compare whole components.
    let lookalike = format!("{root_text}-lookalike/dir/note.txt");
    let outside = read_outcome(&tools, &lookalike);
    assert_failed_json(&outside, ERROR_PATH_DENIED, None, root.as_path());
    assert!(
        json_content(&outside)["error"]["message"]
            .as_str()
            .expect("message should be text")
            .contains("outside every configured workspace root"),
        "a path that only shares a prefix with the root is outside the workspace"
    );
}

#[test]
fn workspace_path_still_denies_hidden_components_after_normalization() {
    let temp = TempWorkspace::new("path-hidden-after-normalization");
    temp.write_text(".git/config", "[core]\n");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "dir/../.git/config");

    assert_failed_json(
        &outcome,
        ERROR_PATH_DENIED,
        Some("dir/../.git/config"),
        temp.path(),
    );
    assert!(
        json_content(&outcome)["error"]["message"]
            .as_str()
            .expect("message should be text")
            .contains("hidden paths are not allowed"),
        "normalizing dot segments must not bypass the hidden-path rule"
    );
}
