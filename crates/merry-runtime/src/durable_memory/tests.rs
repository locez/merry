use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use schemars::Schema;

use super::{
    DurableMemory, DurableMemoryError, MemoryLayout, MemoryWriteKind,
    capture::{self, CaptureOutcome, CaptureWrite, ObservationDraft, ObservationType},
    condense::{self, count_external_turns},
    dream::{self, dream_should_run},
    paths::{ARCHIVE_DIR, INBOX_DIR, MANIFEST_FILE},
};
use crate::session::{TranscriptItemSnapshot, UserInputOrigin};

fn workspace(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace dir should be created");
    workspace
}

fn open_memory(temp: &tempfile::TempDir) -> std::sync::Arc<DurableMemory> {
    let workspace = workspace(temp);
    DurableMemory::open(
        MemoryLayout::at(&temp.path().join("memory"), &workspace)
            .expect("memory layout should resolve"),
    )
    .expect("memory store should open")
}

fn denied(error: DurableMemoryError) -> String {
    match error {
        DurableMemoryError::Denied { message } => message,
        other => panic!("expected a memory policy denial, got {other}"),
    }
}

#[test]
fn layout_id_is_stable_and_rejects_relative_roots() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let workspace = workspace(&temp);
    let memory_root = temp.path().join("memory");
    let first = MemoryLayout::at(&memory_root, &workspace).expect("layout should resolve");
    let second = MemoryLayout::at(&memory_root, &workspace).expect("layout should resolve again");

    assert_eq!(first.workspace_id(), second.workspace_id());
    assert_eq!(first.workspace_id().len(), 16);
    assert!(
        first
            .workspace_id()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(matches!(
        MemoryLayout::at(&memory_root, Path::new("relative")),
        Err(DurableMemoryError::RelativePath {
            role: "workspace root",
            ..
        })
    ));
    assert!(matches!(
        MemoryLayout::at(Path::new("relative-memory"), &workspace),
        Err(DurableMemoryError::RelativePath {
            role: "memory root",
            ..
        })
    ));
    let file = temp.path().join("not-a-directory");
    fs::write(&file, "x").expect("file should be written");
    assert!(matches!(
        MemoryLayout::at(&memory_root, &file),
        Err(DurableMemoryError::WorkspaceNotDirectory { .. })
    ));
}

#[test]
fn extraction_outcome_accepts_noop_and_rejects_malformed_objects() {
    assert_eq!(
        capture::parse_model_outcome(r#"{"outcome":"noop","observations":[]}"#)
            .expect("empty noop should parse"),
        CaptureOutcome::Noop
    );
    assert_eq!(
        capture::parse_model_outcome("```json\n{\"outcome\":\"noop\"}\n```")
            .expect("fenced noop should parse"),
        CaptureOutcome::Noop
    );

    let observations = capture::parse_model_outcome(
        r#"{"outcome":"observations","observations":[{"type":"user","topic_hint":"sandbox","statement":"The user wants XDG paths.","keywords":["xdg"],"aliases":[],"body":null}]}"#,
    )
    .expect("observation should parse");
    let CaptureOutcome::Observations(observations) = observations else {
        panic!("expected observations");
    };
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].observation_type, ObservationType::User);
    assert_eq!(observations[0].statement, "The user wants XDG paths.");

    let extra = capture::parse_model_outcome(r#"{"outcome":"noop","observations":[],"extra":1}"#);
    assert!(extra.is_err(), "unknown fields should be rejected");
    let oversized = "a".repeat(1025);
    let oversized = capture::parse_model_outcome(&format!(
        r#"{{"outcome":"observations","observations":[{{"type":"project","topic_hint":null,"statement":"{oversized}","keywords":[],"aliases":[],"body":null}}]}}"#
    ));
    assert!(
        oversized
            .expect_err("oversized statement should be rejected")
            .contains("statement exceeds byte limit")
    );
    let mixed = capture::parse_model_outcome(
        r#"{"outcome":"noop","observations":[{"type":"user","topic_hint":null,"statement":"kept","keywords":[],"aliases":[],"body":null}]}"#,
    );
    assert!(
        mixed
            .expect_err("noop with observations should be rejected")
            .contains("noop cannot contain observations")
    );
}

#[test]
fn observation_file_records_capture_front_matter() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let memory = open_memory(&temp);
    let draft = ObservationDraft {
        observation_type: ObservationType::Project,
        topic_hint: Some("sandbox".to_owned()),
        statement: "Bubblewrap is the CI sandbox.".to_owned(),
        keywords: vec!["bubblewrap".to_owned()],
        aliases: Vec::new(),
        body: Some("Use it for nested tests.".to_owned()),
    };
    let name = capture::observation_file_name("ses/sion", 2, 4, 1);
    assert_eq!(name, "ses_sion__t2-4__n1.md");
    capture::write_observations(
        memory.layout().workspace_dir(),
        &CaptureWrite {
            session_id: "ses/sion",
            from_turn: 2,
            through_turn: 4,
            extraction_model: "summary-model",
            created_at: 1_700_000_000,
            job_id: "capture-ses-2-4",
            observations: &[draft],
        },
    )
    .expect("observation should be written");
    let text = fs::read_to_string(
        memory
            .layout()
            .workspace_dir()
            .join(INBOX_DIR)
            .join("ses_sion__t2-4__n1.md"),
    )
    .expect("observation file should exist");

    assert!(text.contains("schema_version: 2\n"));
    assert!(text.contains("type: project\n"));
    assert!(text.contains("prompt_version: \"memory-v2-capture-1\"\n"));
    assert!(text.contains("# Bubblewrap is the CI sandbox.\n"));
    assert!(text.contains("Use it for nested tests.\n"));
}

#[test]
fn memory_writes_require_a_fresh_topic_snapshot() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let memory = open_memory(&temp);
    let scope = memory.layout().workspace_dir();
    let topic = scope.join("topics/sandbox.md");
    fs::write(&topic, "# Sandbox\n\nOne fact.\n").expect("topic should be written");
    let observation = scope.join("observations/_inbox/note.md");
    let outside = temp.path().join("outside.md");
    let nested = scope.join("topics/nested/note.md");
    let manifest = scope.join(MANIFEST_FILE);

    assert!(
        memory
            .authorize_write(MemoryWriteKind::Create, &outside, 8)
            .is_ok()
    );
    assert!(
        denied(
            memory
                .authorize_write(MemoryWriteKind::Update, &manifest, 8)
                .unwrap_err()
        )
        .contains("MEMORY.md")
    );
    assert!(
        denied(
            memory
                .authorize_write(MemoryWriteKind::Create, &nested, 8)
                .unwrap_err()
        )
        .contains("directly under")
    );
    assert!(
        denied(
            memory
                .authorize_write(MemoryWriteKind::Update, &observation, 8)
                .unwrap_err()
        )
        .contains("observations are immutable")
    );
    memory
        .authorize_write(MemoryWriteKind::Create, &observation, 32)
        .expect("new observation should be allowed");
    assert!(
        memory
            .authorize_write(MemoryWriteKind::Create, &observation, 16 * 1024 + 1)
            .is_err()
    );

    let before_read = memory.authorize_write(MemoryWriteKind::Update, &topic, 32);
    assert!(denied(before_read.unwrap_err()).contains("read_text the memory file"));
    memory
        .note_read(&topic)
        .expect("topic read should snapshot");
    memory
        .authorize_write(MemoryWriteKind::Update, &topic, 32)
        .expect("update should match the snapshot");
    fs::write(&topic, "# Sandbox\n\nChanged.\n").expect("topic should change");
    memory
        .refresh_after_writes(std::slice::from_ref(&topic))
        .expect("index should refresh");
    let stale = memory.authorize_write(MemoryWriteKind::Delete, &topic, 0);
    assert!(denied(stale.unwrap_err()).contains("changed since it was read"));
    memory.note_read(&topic).expect("reread should snapshot");
    memory
        .authorize_write(MemoryWriteKind::Delete, &topic, 0)
        .expect("delete should match the new snapshot");
}

#[test]
fn manifest_ranks_read_topics_and_keeps_the_byte_cap() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let memory = open_memory(&temp);
    let scope = memory.layout().workspace_dir();
    let alpha = scope.join("topics/alpha.md");
    let zeta = scope.join("topics/zeta.md");
    fs::write(&alpha, "# Alpha\n\nFirst.\n").expect("alpha should be written");
    fs::write(&zeta, "# Zeta\n\nSecond.\n").expect("zeta should be written");
    memory.note_read(&zeta).expect("zeta read should count");
    memory
        .refresh_after_writes(std::slice::from_ref(&zeta))
        .expect("index should refresh");
    let ranked = memory.render_context().expect("context should render");
    let zeta_at = ranked.find("Zeta").expect("zeta should be listed");
    let alpha_at = ranked.find("Alpha").expect("alpha should be listed");
    assert!(zeta_at < alpha_at);
    assert!(ranked.contains("## Global memory manifest"));
    assert!(ranked.contains("No topics have been recorded yet."));

    for index in 0..60 {
        let name = format!("topic-{index:03}-{}.md", "n".repeat(160));
        let path = scope.join("topics").join(name);
        fs::write(&path, format!("# {}\n", "T".repeat(7_000))).expect("topic should be written");
    }
    memory
        .refresh_after_writes(&[alpha])
        .expect("capped index should refresh");
    let capped = fs::read_to_string(scope.join(MANIFEST_FILE)).expect("index should exist");
    assert!(capped.len() <= 8 * 1024);
    assert!(capped.contains("more topics are not listed."));
}

#[test]
fn dream_creates_a_topic_and_archives_the_whole_claim() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let memory = open_memory(&temp);
    let scope = memory.layout().workspace_dir().to_path_buf();
    let inbox = scope.join(INBOX_DIR);
    let cited = inbox.join("cited.md");
    let uncited = inbox.join("uncited.md");
    fs::write(&cited, "# Cited\n").expect("cited note should be written");
    fs::write(&uncited, "# Uncited\n").expect("uncited note should be written");
    let claimed = vec![
        dream::ClaimedNote {
            relative_path: format!("{INBOX_DIR}/cited.md"),
            absolute_path: cited.clone(),
            modified: SystemTime::now(),
        },
        dream::ClaimedNote {
            relative_path: format!("{INBOX_DIR}/uncited.md"),
            absolute_path: uncited.clone(),
            modified: SystemTime::now(),
        },
    ];
    let rejected = memory.apply_dream(&claimed, r#"{"operations":[]}"#);
    assert!(rejected.is_err());
    assert!(cited.is_file());
    assert!(uncited.is_file());

    memory
        .apply_dream(
            &claimed,
            r##"{"operations":[{"op":"create","path":"topics/sandbox.md","content":"# Sandbox\n\nOne fact.\n","evidence":["observations/_inbox/cited.md"]}]}"##,
        )
        .expect("dream plan should apply");

    let topic = fs::read_to_string(scope.join("topics/sandbox.md")).expect("topic should exist");
    assert!(topic.starts_with("# Sandbox\n"));
    assert!(!cited.exists());
    assert!(!uncited.exists());
    let archived = fs::read_dir(scope.join(ARCHIVE_DIR))
        .expect("archive should exist")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "md")
        })
        .count();
    assert_eq!(archived, 2);
}

#[test]
fn dream_runs_for_twenty_notes_or_one_day_old_note() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let memory = open_memory(&temp);
    let inbox = memory.layout().workspace_dir().join(INBOX_DIR);
    let now = SystemTime::now();

    write_note(
        &inbox.join("old.md"),
        Some(Duration::from_secs(25 * 60 * 60)),
    );
    let old = memory.scan_inbox().expect("old inbox should scan");
    assert!(dream_should_run(&old, now));

    fs::remove_file(inbox.join("old.md")).expect("old note should be removed");
    for index in 0..19 {
        write_note(&inbox.join(format!("fresh-{index}.md")), None);
    }
    let nineteen = memory.scan_inbox().expect("nineteen notes should scan");
    assert!(!dream_should_run(&nineteen, SystemTime::now()));
    write_note(&inbox.join("fresh-19.md"), None);
    let twenty = memory.scan_inbox().expect("twenty notes should scan");
    assert!(dream_should_run(&twenty, SystemTime::now()));

    write_note(&inbox.join("huge.md"), None);
    let huge = inbox.join("huge.md");
    fs::write(&huge, vec![b'a'; 16 * 1024 + 1]).expect("oversized note should be written");
    let scan = memory.scan_inbox().expect("oversized inbox should scan");
    assert_eq!(scan.oversized, 1);
    assert!(
        scan.eligible
            .iter()
            .all(|note| note.relative_path != format!("{INBOX_DIR}/huge.md"))
    );
}

#[test]
fn capture_and_dream_schemas_are_json_objects() {
    Schema::try_from(capture::extraction_schema()).expect("capture schema should be accepted");
    Schema::try_from(dream::dream_plan_schema()).expect("dream schema should be accepted");
}

#[test]
fn instructions_name_the_file_tools_and_protect_the_index() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let text = open_memory(&temp).instructions();
    assert!(text.contains("`read_text`"));
    assert!(text.contains("`apply_patch`"));
    assert!(text.contains("never read or edit MEMORY.md"));
    assert!(text.contains("pending observations"));
    assert!(text.contains("topics/"));
    assert!(text.contains("observations/_inbox/"));
}

#[test]
fn injected_context_lists_pending_observations_without_rewriting_the_index() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let memory = open_memory(&temp);
    let inbox = memory.layout().workspace_dir().join(INBOX_DIR);
    fs::write(inbox.join("hello.md"), "# 你好\n\n你好\n").expect("note should be written");
    let context = memory.render_context().expect("context should render");
    assert!(context.contains("## Pending observations"));
    assert!(context.contains("你好"));
    assert!(context.contains("observations/_inbox/hello.md"));
    assert_eq!(context.matches("## Pending observations").count(), 1);
    let manifest = fs::read_to_string(memory.layout().workspace_dir().join(MANIFEST_FILE))
        .expect("manifest should exist");
    assert!(!manifest.contains("Pending observations"));
    assert!(!manifest.contains("你好"));
}

#[test]
fn condensed_transcript_marks_omitted_steps() {
    let text = "u".repeat(8 * 1024 + 1);
    let items = (0..8)
        .map(|_| TranscriptItemSnapshot::UserMessage {
            text: text.clone(),
            images: Vec::new(),
            origin: UserInputOrigin::ExternalUser,
        })
        .collect::<Vec<_>>();
    assert_eq!(count_external_turns(&items), 8);
    let condensed = condense::condense_turn_range(&items, 1, 8).expect("range should condense");
    let note = condensed.note.expect("condensation should be explained");
    assert!(note.contains(
        "The transcript was condensed to fit the extraction budget. Text between \"[...\" and \"...]\" markers was removed, not written by a participant."
    ));
    assert!(note.contains("User text was truncated."));
    assert!(note.contains("Model reasoning was omitted."));
    assert!(condensed.json.contains("[..."));
    assert!(condensed.json.len() <= 48 * 1024);
}

fn write_note(path: &Path, age: Option<Duration>) {
    fs::write(path, "# Note\n").expect("note should be written");
    if let Some(age) = age {
        let file = fs::File::options()
            .write(true)
            .open(path)
            .expect("note should open");
        let modified = SystemTime::now()
            .checked_sub(age)
            .expect("age should fit in the clock");
        file.set_modified(modified)
            .expect("note mtime should be set");
    }
}
