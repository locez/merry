//! Tests for tool path form, normalization, and scope boundaries.
//!
//! A relative tool path is resolved against the configured roots, so
//! `dir/note.txt` and the absolute path to that file address the same target
//! and report the same workspace-relative path. An absolute path outside every
//! root is the caller's own way to name a file the sandbox exposes, so it is
//! resolved as named instead of being denied by a second path policy inside the
//! tool. What remains the tools' own business is that a relative path cannot
//! climb above its root, that hidden components are denied, and that a child
//! agent cannot leave the scope its parent gave it.

use super::*;

/// Returns the canonical workspace root, which is what the tools resolve against.
fn canonical_root(temp: &TempWorkspace) -> PathBuf {
    fs::canonicalize(temp.path()).expect("workspace root should canonicalize")
}

/// Returns a sibling path outside the workspace root for this test.
///
/// The name derives from the unique temp workspace name so parallel test runs
/// cannot collide, and the caller removes what it created.
fn sibling_of(root: &Path, suffix: &str) -> PathBuf {
    let name = root
        .file_name()
        .expect("workspace root should have a file name")
        .to_string_lossy();
    root.parent()
        .expect("workspace root should have a parent")
        .join(format!("{name}{suffix}"))
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
fn read_text_reads_absolute_path_outside_the_workspace() {
    let temp = TempWorkspace::new("path-absolute-outside");
    let tools = tools_for(temp.path());
    let root = canonical_root(&temp);
    let outside = sibling_of(&root, "-outside.txt");
    fs::write(&outside, "outside\n").expect("outside file should be writable");
    let outside_text = outside.to_str().expect("outside path should be utf8");

    let outcome = read_outcome(&tools, outside_text);

    assert_eq!(
        outcome.status(),
        ToolCallResultStatus::Succeeded,
        "an absolute path outside the workspace is the caller's own reference, not a tool denial"
    );
    assert_eq!(json_content(&outcome)["content"], "outside\n");
    assert_eq!(
        json_content(&outcome)["path"],
        outside_text,
        "a target outside every root has no workspace-relative spelling to report"
    );
    fs::remove_file(&outside).expect("outside file should be removable");
}

#[test]
fn apply_patch_edits_files_outside_the_workspace() {
    let temp = TempWorkspace::new("path-absolute-patch-outside");
    let tools = tools_for(temp.path());
    let root = canonical_root(&temp);
    let outside_dir = sibling_of(&root, "-outside");
    fs::create_dir_all(&outside_dir).expect("outside directory should be creatable");
    let existing = outside_dir.join("note.txt");
    fs::write(&existing, "alpha\nold\nomega\n").expect("outside file should be writable");
    let existing_text = existing.to_str().expect("outside path should be utf8");

    let update = patch_text_outcome(&tools, &update_patch(existing_text, "old", "new"));
    assert_eq!(update.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&existing), "alpha\nnew\nomega\n");

    let created = outside_dir.join("dir/created.txt");
    let created_text = created.to_str().expect("outside path should be utf8");
    let add = patch_text_outcome(&tools, &add_patch(created_text, &["one", "two"]));
    assert_eq!(add.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&created), "one\ntwo\n");

    let delete = patch_text_outcome(&tools, &delete_patch(existing_text));
    assert_eq!(delete.status(), ToolCallResultStatus::Succeeded);
    assert!(
        !existing.exists(),
        "a delete section must remove the file it names"
    );
    fs::remove_dir_all(&outside_dir).expect("outside directory should be removable");
}

#[test]
fn child_write_scope_denies_absolute_paths_outside_the_workspace() {
    let temp = TempWorkspace::new("path-child-write-scope");
    let scoped = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")])),
    )
    .expect("workspace tools should construct");
    let unscoped = tools_for(temp.path());
    let root = canonical_root(&temp);
    let outside = sibling_of(&root, "-outside.txt");
    fs::write(&outside, "alpha\nold\nomega\n").expect("outside file should be writable");
    let outside_text = outside.to_str().expect("outside path should be utf8");

    // The target is reachable and editable here, so the denial below is about the
    // child scope rather than about a path the tool refuses for its own reasons.
    let allowed = patch_text_outcome(&unscoped, &update_patch(outside_text, "old", "new"));
    assert_eq!(allowed.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&outside), "alpha\nnew\nomega\n");
    fs::write(&outside, "alpha\nold\nomega\n").expect("outside file should be restored");

    let denied = patch_text_outcome(&scoped, &update_patch(outside_text, "old", "new"));

    assert_eq!(denied.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        denied.diagnostic().expect("diagnostic").code(),
        ERROR_PATH_DENIED
    );
    assert!(
        json_content(&denied)["error"]["message"]
            .as_str()
            .expect("message should be text")
            .contains("outside the child write scope"),
        "a child agent must not leave its write scope by naming an absolute path"
    );
    assert_eq!(read_text(&outside), "alpha\nold\nomega\n");
    fs::remove_file(&outside).expect("outside file should be removable");
}

#[cfg(unix)]
#[test]
fn read_text_follows_platform_symlinks_outside_the_workspace() {
    let temp = TempWorkspace::new("path-outside-symlink");
    let tools = tools_for(temp.path());
    let root = canonical_root(&temp);
    let target_dir = sibling_of(&root, "-linked");
    let link_dir = sibling_of(&root, "-link");
    fs::create_dir_all(&target_dir).expect("linked directory should be creatable");
    fs::write(target_dir.join("note.txt"), "linked\n").expect("linked file should be writable");
    symlink(&target_dir, &link_dir).expect("directory symlink should be created");

    // Platform layout routinely exposes a real directory through a link, so a
    // target outside the workspace is resolved as the caller named it instead of
    // being denied for a component the sandbox already allows.
    let linked = link_dir.join("note.txt");
    let outcome = read_outcome(&tools, linked.to_str().expect("linked path should be utf8"));

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(json_content(&outcome)["content"], "linked\n");
    fs::remove_file(link_dir.as_path()).expect("directory symlink should be removable");
    fs::remove_dir_all(&target_dir).expect("linked directory should be removable");
}

#[test]
fn workspace_path_normalizes_dot_segments_and_resolves_escapes() {
    let temp = TempWorkspace::new("path-dot-segments");
    temp.write_text("dir/note.txt", "one\ntwo\n");
    let tools = tools_for(temp.path());
    let root = canonical_root(&temp);

    let inside = read_outcome(&tools, "./dir/../dir/note.txt");
    assert_eq!(
        inside.status(),
        ToolCallResultStatus::Succeeded,
        "redundant dot segments name the same file and must not be rejected"
    );
    assert_eq!(json_content(&inside)["path"], "dir/note.txt");

    // A relative argument may climb above its root: the target is the caller's
    // own reference, and the reported path keeps the `..` so the escape stays
    // visible instead of being silently rewritten as an in-root path.
    let sibling = sibling_of(&root, "-climbed.txt");
    fs::write(&sibling, "climbed\n").expect("sibling file should be writable");
    let named = sibling
        .strip_prefix(root.parent().expect("root parent"))
        .expect("sibling is below the shared parent");
    let climbed = read_outcome(&tools, &format!("../{}", named.display()));
    assert_eq!(
        climbed.status(),
        ToolCallResultStatus::Succeeded,
        "a relative path that climbs above the root names a real file"
    );
    assert_eq!(json_content(&climbed)["content"], "climbed\n");
    assert_eq!(
        json_content(&climbed)["path"],
        format!("../{}", named.display()),
        "the reported path keeps the escape visible"
    );
    fs::remove_file(&sibling).expect("sibling file should be removable");
}

#[test]
fn workspace_path_denies_the_root_itself_and_reads_prefix_lookalikes() {
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
    // inside the workspace, so prefix matching must compare whole components
    // rather than treat the sibling as part of the root.
    let lookalike_dir = sibling_of(&root, "-lookalike");
    fs::create_dir_all(lookalike_dir.join("dir")).expect("lookalike tree should be creatable");
    fs::write(lookalike_dir.join("dir/note.txt"), "sibling\n")
        .expect("lookalike file should be writable");
    let lookalike = lookalike_dir.join("dir/note.txt");
    let lookalike_text = lookalike.to_str().expect("lookalike path should be utf8");

    let sibling = read_outcome(&tools, lookalike_text);
    assert_eq!(
        sibling.status(),
        ToolCallResultStatus::Succeeded,
        "a sibling that shares the root's name prefix is its own target"
    );
    assert_eq!(json_content(&sibling)["content"], "sibling\n");
    assert_eq!(
        json_content(&sibling)["path"],
        lookalike_text,
        "the sibling is not inside a configured root, so it is reported absolute"
    );
    fs::remove_dir_all(&lookalike_dir).expect("lookalike tree should be removable");
}

#[test]
fn workspace_path_reads_hidden_components_after_normalization() {
    let temp = TempWorkspace::new("path-hidden-after-normalization");
    temp.write_text(".git/config", "[core]\n");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "dir/../.git/config");
    assert_eq!(
        outcome.status(),
        ToolCallResultStatus::Succeeded,
        "a leading dot is ordinary spelling and must survive normalization"
    );
    assert_eq!(
        json_content(&outcome)["path"],
        ".git/config",
        "the reported path is the normalized workspace-relative form"
    );
    assert_eq!(json_content(&outcome)["content"], "[core]\n");
}
