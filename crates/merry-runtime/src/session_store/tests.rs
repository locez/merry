use crate::{
    session::SessionState,
    session_store::{
        FileSessionStore, OsStr, SessionId, SessionStoreError, SessionStoreStagePause,
        sessions_dir_from_env,
    },
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
async fn staged_journal_append_does_not_mutate_disk_until_commit_and_discard_is_clean() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-stage").expect("valid session id");
    let header =
        b"{\"type\":\"header\",\"format_version\":1,\"session_id\":\"session-store-stage\"}\n";
    let first = b"{\"type\":\"commit\",\"state\":{\"next_sequence\":1}}\n";
    let discarded = b"{\"type\":\"commit\",\"state\":{\"next_sequence\":2}}\n";

    store
        .stage_journal_append(
            &session_id,
            header.to_vec(),
            first.to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("first append stages")
        .commit()
        .await
        .expect("first append commits")
        .require_durable()
        .expect("first append is durable");

    let staged = store
        .stage_journal_append(
            &session_id,
            header.to_vec(),
            discarded.to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("second append stages");

    let expected = [header.as_slice(), first.as_slice()].concat();
    assert_eq!(
        store
            .read_journal_bytes(&session_id)
            .await
            .expect("committed journal reads"),
        expected
    );

    staged.discard().await.expect("staged append discards");
    assert_eq!(
        store
            .read_journal_bytes(&session_id)
            .await
            .expect("committed journal remains unchanged after discard"),
        expected
    );
}

#[tokio::test]
async fn injected_commit_failure_keeps_committed_journal_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-store-commit-failure").expect("valid session id");
    let header = b"{\"type\":\"header\"}\n";
    let old_commit = b"{\"type\":\"commit\",\"v\":1}\n";
    store
        .stage_journal_append(
            &session_id,
            header.to_vec(),
            old_commit.to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("initial append stages")
        .commit()
        .await
        .expect("initial append commits")
        .require_durable()
        .expect("initial append is durable");

    let failing_store = store.clone().with_commit_failure_for_tests();
    let staged = failing_store
        .stage_journal_append(
            &session_id,
            header.to_vec(),
            b"{\"type\":\"commit\",\"v\":2}\n".to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("failing append stages");

    staged
        .commit()
        .await
        .expect_err("injected commit failure must fail before appending");

    assert_eq!(
        store
            .read_journal_bytes(&session_id)
            .await
            .expect("committed journal reads"),
        [header.as_slice(), old_commit.as_slice()].concat()
    );
}

#[tokio::test]
async fn stage_pause_blocks_before_commit_and_only_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pause = SessionStoreStagePause::new();
    let store = FileSessionStore::new(temp.path()).with_stage_pause_for_tests(pause.clone());
    let session_id = SessionId::new("session-store-pause").expect("valid session id");
    let staged_store = store.clone();
    let staged_session_id = session_id.clone();
    let staging = tokio::spawn(async move {
        staged_store
            .stage_journal_append(
                &staged_session_id,
                b"{\"type\":\"header\"}\n".to_vec(),
                b"{\"type\":\"commit\",\"v\":1}\n".to_vec(),
                &[],
                None,
                None,
            )
            .await
    });

    pause.wait_until_staged().await;
    assert!(!store.journal_path(&session_id).exists());
    pause.resume();
    staging
        .await
        .expect("staging task joins")
        .expect("first staging succeeds")
        .discard()
        .await
        .expect("first staged append discards");

    store
        .stage_journal_append(
            &session_id,
            b"{\"type\":\"header\"}\n".to_vec(),
            b"{\"type\":\"commit\",\"v\":2}\n".to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("pause is consumed after the first stage")
        .discard()
        .await
        .expect("second staged append discards");
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
        .stage_journal_append(
            &session_id,
            b"{\"type\":\"header\"}\n".to_vec(),
            b"{\"type\":\"commit\"}\n".to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("journal append should stage");
    assert!(
        !store
            .contains_session(&session_id)
            .await
            .expect("a staged store should be readable"),
        "staged state is not yet a committed session"
    );

    staged
        .commit()
        .await
        .expect("staged append should commit")
        .require_durable()
        .expect("commit should be durable");
    assert!(
        store
            .contains_session(&session_id)
            .await
            .expect("a committed store should be readable"),
        "committed state must be reported so a new run cannot reuse the id"
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

#[tokio::test]
async fn release_empty_reservation_removes_untouched_dir_and_keeps_populated_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());

    let empty_id = SessionId::new("empty-reserved-session").expect("valid session id");
    let empty_reservation = store
        .reserve_session(&empty_id)
        .await
        .expect("empty session reserves");
    let empty_dir = store.session_dir(&empty_id);
    assert!(empty_dir.join(super::SESSION_LOCK_FILE_NAME).exists());

    store
        .release_empty_reservation(&empty_id, empty_reservation)
        .await;
    assert!(
        !empty_dir.exists(),
        "untouched empty session directory should be removed"
    );

    let populated_id = SessionId::new("populated-reserved-session").expect("valid session id");
    let populated_reservation = store
        .reserve_session(&populated_id)
        .await
        .expect("populated session reserves");
    let populated_dir = store.session_dir(&populated_id);
    tokio::fs::write(populated_dir.join("meta.json"), b"{}\n")
        .await
        .expect("meta placeholder writes");

    store
        .release_empty_reservation(&populated_id, populated_reservation)
        .await;
    assert!(
        populated_dir.join("meta.json").exists(),
        "session directory with persisted files must be preserved"
    );
}

#[tokio::test]
async fn journal_appends_write_header_once_and_serialize_concurrent_commits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store_a = FileSessionStore::new(temp.path());
    let store_b = store_a.clone();
    let session_id = SessionId::new("session-journal-append").expect("valid session id");
    let header =
        b"{\"type\":\"header\",\"format_version\":1,\"session_id\":\"session-journal-append\"}\n";
    let first = b"{\"type\":\"commit\",\"v\":1}\n";
    let second = b"{\"type\":\"commit\",\"v\":2}\n";

    let staged_a = store_a
        .stage_journal_append(
            &session_id,
            header.to_vec(),
            first.to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("first append stages");
    let staged_b = store_b
        .stage_journal_append(
            &session_id,
            header.to_vec(),
            second.to_vec(),
            &[],
            None,
            None,
        )
        .await
        .expect("second append stages");

    let (res_a, res_b) = tokio::join!(staged_a.commit(), staged_b.commit());
    res_a
        .expect("first commit succeeds")
        .require_durable()
        .expect("first commit durable");
    res_b
        .expect("second commit succeeds")
        .require_durable()
        .expect("second commit durable");

    let text = String::from_utf8(
        store_a
            .read_journal_bytes(&session_id)
            .await
            .expect("journal reads"),
    )
    .expect("journal is UTF-8");
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        3,
        "expected exactly 1 header + 2 commits: {text}"
    );
    assert_eq!(
        lines[0],
        std::str::from_utf8(header).expect("utf8").trim_end()
    );
    assert!(
        lines
            .iter()
            .filter(|line| line.contains("\"type\":\"header\""))
            .count()
            == 1
    );
    assert!(lines.contains(&"{\"type\":\"commit\",\"v\":1}"));
    assert!(lines.contains(&"{\"type\":\"commit\",\"v\":2}"));
}

#[tokio::test]
async fn load_truncates_incomplete_trailing_line_so_subsequent_append_stays_valid() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = SessionId::new("session-truncated-tail").expect("valid session id");
    let mut session = SessionState::new(session_id.clone());
    let turn1 = session.begin_model_turn().expect("turn 1 begins");
    session
        .record_user_message_body(turn1, "first message")
        .expect("turn 1 user message");
    session
        .close_model_response(turn1, false)
        .expect("turn 1 closes");
    session.save_to(&store).await.expect("initial save");

    // Simulate a crash mid-append: half a JSON object with no trailing newline.
    let mut corrupted = store
        .read_journal_bytes(&session_id)
        .await
        .expect("journal reads");
    corrupted.extend_from_slice(b"{\"type\":\"commit\",\"items\":[{\"type\":\"user_mes");
    store
        .overwrite_journal_bytes(&session_id, &corrupted)
        .await
        .expect("corrupted tail writes");

    let mut resumed = SessionState::load_from(&store, &session_id)
        .await
        .expect("load ignores incomplete trailing line and truncates it");
    let turn2 = resumed.begin_model_turn().expect("turn 2 begins");
    resumed
        .record_user_message_body(turn2, "second message after recovery")
        .expect("turn 2 user message");
    resumed
        .close_model_response(turn2, false)
        .expect("turn 2 closes");
    resumed
        .save_to(&store)
        .await
        .expect("append after recovery succeeds");

    let reloaded = SessionState::load_from(&store, &session_id)
        .await
        .expect("reloaded journal after recovery append must remain valid");
    assert_eq!(
        reloaded.transcript_items_for_tests(),
        vec![
            "user:first message".to_owned(),
            "user:second message after recovery".to_owned()
        ]
    );
}
