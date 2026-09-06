use crate::session_store::{
    FileSessionStore, OsStr, SessionId, SessionStoreError, SessionStoreStagePause,
    sessions_dir_from_env,
};
use std::path::PathBuf;

#[test]
fn session_store_default_sessions_dir_uses_xdg_state_home_when_present() {
    let root = sessions_dir_from_env(
        Some(OsStr::new("/tmp/merry-state")),
        Some(OsStr::new("/home/test")),
    )
    .expect("xdg state root resolves");

    assert_eq!(
        root,
        PathBuf::from("/tmp/merry-state")
            .join("merry")
            .join("sessions")
    );
}

#[test]
fn session_store_default_sessions_dir_falls_back_to_local_state_home() {
    let root = sessions_dir_from_env(None, Some(OsStr::new("/home/test")))
        .expect("home fallback resolves");

    assert_eq!(
        root,
        PathBuf::from("/home/test").join(".local/state/merry/sessions")
    );
}

#[tokio::test]
async fn session_store_writes_state_json_with_atomic_replace() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-test").expect("valid session id");

    store
        .write_state_bytes(&session_id, br#"{"format_version":1}"#)
        .await
        .expect("state write succeeds");
    store
        .write_state_bytes(&session_id, br#"{"format_version":1,"second":true}"#)
        .await
        .expect("state rewrite succeeds");

    let bytes = store
        .read_state_bytes(&session_id)
        .await
        .expect("state reads");
    assert_eq!(bytes, br#"{"format_version":1,"second":true}"#);
    assert!(store.state_path(&session_id).ends_with("state.json"));
}

#[tokio::test]
async fn staged_state_bytes_do_not_replace_committed_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-stage").expect("valid session id");
    let old_bytes = br#"{"format_version":1,"value":"old"}"#;
    let new_bytes = br#"{"format_version":1,"value":"new"}"#;
    store
        .write_state_bytes(&session_id, old_bytes)
        .await
        .expect("initial state write succeeds");

    let staged = store
        .stage_state_bytes(&session_id, new_bytes)
        .await
        .expect("state staging succeeds");

    assert_eq!(
        store
            .read_state_bytes(&session_id)
            .await
            .expect("committed state reads"),
        old_bytes
    );
    assert!(staged.temp_path.exists());
    staged.discard().await.expect("staged state discards");
}

#[tokio::test]
async fn discarding_staged_state_keeps_committed_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-discard").expect("valid session id");
    let old_bytes = br#"{"format_version":1,"value":"old"}"#;
    store
        .write_state_bytes(&session_id, old_bytes)
        .await
        .expect("initial state write succeeds");
    let staged = store
        .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"discarded"}"#)
        .await
        .expect("state staging succeeds");
    let temp_path = staged.temp_path.clone();

    staged.discard().await.expect("staged state discards");

    assert_eq!(
        store
            .read_state_bytes(&session_id)
            .await
            .expect("committed state reads"),
        old_bytes
    );
    assert!(!temp_path.exists());
}

#[tokio::test]
async fn committing_staged_state_replaces_committed_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-commit").expect("valid session id");
    let new_bytes = br#"{"format_version":1,"value":"committed"}"#;
    store
        .write_state_bytes(&session_id, br#"{"format_version":1,"value":"old"}"#)
        .await
        .expect("initial state write succeeds");
    let staged = store
        .stage_state_bytes(&session_id, new_bytes)
        .await
        .expect("state staging succeeds");
    let temp_path = staged.temp_path.clone();

    staged
        .commit()
        .await
        .expect("staged state renames")
        .require_durable()
        .expect("committed state is durable");

    assert_eq!(
        store
            .read_state_bytes(&session_id)
            .await
            .expect("committed state reads"),
        new_bytes
    );
    assert!(!temp_path.exists());
}

#[tokio::test]
async fn independent_stages_for_one_session_do_not_share_temp_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store_a = FileSessionStore::new(temp.path());
    let store_b = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-independent-stages").expect("valid id");
    let bytes_a = br#"{"format_version":1,"writer":"a"}"#;
    let bytes_b = br#"{"format_version":1,"writer":"b"}"#;

    let staged_a = store_a
        .stage_state_bytes(&session_id, bytes_a)
        .await
        .expect("first state stages");
    let staged_b = store_b
        .stage_state_bytes(&session_id, bytes_b)
        .await
        .expect("second state stages");

    assert_ne!(staged_a.temp_path, staged_b.temp_path);
    staged_a
        .commit()
        .await
        .expect("first state renames")
        .require_durable()
        .expect("first state is durable");
    assert_eq!(
        store_a
            .read_state_bytes(&session_id)
            .await
            .expect("first committed state reads"),
        bytes_a
    );
    staged_b
        .commit()
        .await
        .expect("second state renames")
        .require_durable()
        .expect("second state is durable");
    assert_eq!(
        store_b
            .read_state_bytes(&session_id)
            .await
            .expect("second committed state reads"),
        bytes_b
    );
}

#[tokio::test]
async fn injected_commit_failure_keeps_committed_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-commit-failure").expect("valid session id");
    let old_bytes = br#"{"format_version":1,"value":"old"}"#;
    store
        .write_state_bytes(&session_id, old_bytes)
        .await
        .expect("initial state write succeeds");
    let failing_store = store.clone().with_commit_failure_for_tests();
    let staged = failing_store
        .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"new"}"#)
        .await
        .expect("state staging succeeds");

    staged
        .commit()
        .await
        .expect_err("injected commit failure must fail before rename");

    assert_eq!(
        store
            .read_state_bytes(&session_id)
            .await
            .expect("committed state reads"),
        old_bytes
    );
    assert!(store.staged_state_paths_for_tests(&session_id).is_empty());
}

#[tokio::test]
async fn stage_pause_blocks_after_sync_and_only_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pause = SessionStoreStagePause::new();
    let store = FileSessionStore::new(temp.path()).with_stage_pause_for_tests(pause.clone());
    let session_id = SessionId::new("session-store-pause").expect("valid session id");
    let staged_store = store.clone();
    let staged_session_id = session_id.clone();
    let staging = tokio::spawn(async move {
        staged_store
            .stage_state_bytes(
                &staged_session_id,
                br#"{"format_version":1,"value":"first"}"#,
            )
            .await
    });

    pause.wait_until_staged().await;
    assert!(
        store.staged_state_paths_for_tests(&session_id).len() == 1,
        "pause must become observable after the temporary file is synced"
    );
    assert!(!store.state_path(&session_id).exists());
    pause.resume();
    staging
        .await
        .expect("staging task joins")
        .expect("first state staging succeeds")
        .discard()
        .await
        .expect("first staged state discards");

    store
        .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"second"}"#)
        .await
        .expect("pause is consumed after the first stage")
        .discard()
        .await
        .expect("second staged state discards");
}

#[tokio::test]
async fn contains_session_reports_only_committed_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-collision").expect("valid session id");

    assert!(
        !store
            .contains_session(&session_id)
            .await
            .expect("an empty store should be readable"),
        "an unused session id has no saved state"
    );

    let staged = store
        .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"staged"}"#)
        .await
        .expect("state should stage");
    assert!(
        !store
            .contains_session(&session_id)
            .await
            .expect("a staged store should be readable"),
        "staged state is not yet a session another run could destroy"
    );

    staged
        .commit()
        .await
        .expect("staged state should commit")
        .require_durable()
        .expect("commit should be durable");
    assert!(
        store
            .contains_session(&session_id)
            .await
            .expect("a committed store should be readable"),
        "committed state must be reported so a new run cannot replace it"
    );
}

#[tokio::test]
async fn session_reservation_is_exclusive_and_releases_on_drop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-reservation").expect("valid session id");
    let first = store
        .reserve_session(&session_id)
        .await
        .expect("first reservation succeeds");
    assert!(matches!(
        store.reserve_session(&session_id).await,
        Err(SessionStoreError::SessionAlreadyReserved { .. })
    ));
    drop(first);
    store
        .reserve_session(&session_id)
        .await
        .expect("dropping the reservation releases it");
}

#[tokio::test]
async fn concurrent_session_reservations_have_one_winner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-reservation-race").expect("valid session id");
    let (left, right) = tokio::join!(
        store.reserve_session(&session_id),
        store.reserve_session(&session_id)
    );

    assert_ne!(left.is_ok(), right.is_ok());
    assert!(matches!(
        left.as_ref().err().or_else(|| right.as_ref().err()),
        Some(SessionStoreError::SessionAlreadyReserved { .. })
    ));
}
