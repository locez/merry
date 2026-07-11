use super::*;
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};

const EMPTY_CANDIDATE: &str = r#"{
  "confirmed_decisions": [],
  "rejected_approaches": [],
  "constraints_preferences_boundaries": [],
  "corrected_misunderstandings": [],
  "durable_conclusions": [],
  "open_questions": [],
  "current_progress_and_next_steps": [],
  "exact_details": [],
  "handoffs": []
}"#;

fn ref_id(value: &str) -> CheckpointRefId {
    CheckpointRefId::new(value).expect("valid ref id")
}

fn manifest(checkpoint_id: &str, ids: &[&str]) -> CheckpointRefManifest {
    let checkpoint_id = CheckpointId::new(checkpoint_id).expect("valid checkpoint id");
    CheckpointRefManifest::new(
        checkpoint_id,
        ids.iter()
            .enumerate()
            .map(|(index, id)| {
                CheckpointRef::new(
                    ref_id(id),
                    CheckpointSourceKind::UserMessage,
                    CheckpointSequenceRange::new(index as u64, index as u64)
                        .expect("valid sequence range"),
                    EvidenceRef::new(
                        ArtifactId::new(&format!("source-{id}")).expect("valid artifact id"),
                        EvidenceLocator::whole_artifact(),
                    ),
                )
            })
            .collect(),
    )
    .expect("valid manifest")
}

fn candidate(json: &str) -> CompactedCheckpointCandidate {
    CompactedCheckpointCandidate::from_json(json).expect("candidate should parse")
}

fn first_checkpoint(json: &str, refs: &[&str]) -> CitationBackedCheckpoint {
    CitationBackedCheckpoint::from_candidate(
        CheckpointId::new("checkpoint-first").expect("valid checkpoint id"),
        candidate(json),
        manifest("checkpoint-first", refs),
        CheckpointValidationPolicy::default(),
    )
    .expect("first checkpoint should validate")
}

#[test]
fn checkpoint_requires_all_eight_arrays_but_allows_them_to_be_empty() {
    let candidate = CompactedCheckpointCandidate::from_json(EMPTY_CANDIDATE)
        .expect("all required arrays are present");

    assert_eq!(candidate.sections().entry_count(), 0);
    assert!(candidate.handoffs().is_empty());

    let missing = EMPTY_CANDIDATE.replace("  \"exact_details\": [],\n", "");
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&missing),
        Err(CheckpointError::InvalidCandidateJson)
    ));
}

#[test]
fn confirmed_and_rejected_entries_require_non_blank_rationale() {
    for section in ["confirmed_decisions", "rejected_approaches"] {
        let json = EMPTY_CANDIDATE.replace(
            &format!("  \"{section}\": []"),
            &format!(
                "  \"{section}\": [{{\"id\":\"e1\",\"text\":\"decision\",\"rationale\":\"  \",\"refs\":[\"h1\"]}}]"
            ),
        );
        assert!(matches!(
            CompactedCheckpointCandidate::from_json(&json),
            Err(CheckpointError::EntryRationaleRequired { entry_id }) if entry_id == "e1"
        ));
    }
}

#[test]
fn optional_rationale_is_preserved_but_must_be_non_blank_when_present() {
    let valid = EMPTY_CANDIDATE.replace(
        "  \"open_questions\": []",
        "  \"open_questions\": [{\"id\":\"q1\",\"text\":\"Which API?\",\"rationale\":\"Needed before implementation.\",\"refs\":[\"h1\"]}]",
    );
    let candidate = candidate(&valid);
    let entry = &candidate
        .sections()
        .entries(CheckpointSection::OpenQuestion)[0];
    assert_eq!(entry.rationale(), Some("Needed before implementation."));

    let invalid = valid.replace("Needed before implementation.", "   ");
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&invalid),
        Err(CheckpointError::BlankField {
            field: "checkpoint entry rationale"
        })
    ));
}

#[test]
fn entry_ids_are_unique_across_sections_and_refs_are_non_empty() {
    let duplicate = EMPTY_CANDIDATE
        .replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"same\",\"text\":\"Question\",\"refs\":[\"h1\"]}]",
        )
        .replace(
            "  \"exact_details\": []",
            "  \"exact_details\": [{\"id\":\"same\",\"text\":\"Exact\",\"refs\":[\"h1\"]}]",
        );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&duplicate),
        Err(CheckpointError::DuplicateEntry { entry_id }) if entry_id == "same"
    ));

    let empty_refs = EMPTY_CANDIDATE.replace(
        "  \"exact_details\": []",
        "  \"exact_details\": [{\"id\":\"x1\",\"text\":\"Exact\",\"refs\":[]}]",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&empty_refs),
        Err(CheckpointError::EntryWithoutRefs { entry_id }) if entry_id == "x1"
    ));
}

#[test]
fn checkpoint_rejects_unknown_entry_refs_and_filters_unused_manifest_refs() {
    let json = EMPTY_CANDIDATE.replace(
        "  \"exact_details\": []",
        "  \"exact_details\": [{\"id\":\"x1\",\"text\":\"Exact\",\"refs\":[\"missing\"]}]",
    );
    let error = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new("checkpoint-unknown-ref").expect("valid id"),
        candidate(&json),
        manifest("checkpoint-unknown-ref", &["h1"]),
        CheckpointValidationPolicy::default(),
    )
    .expect_err("unknown refs must fail");
    assert!(matches!(
        error,
        CheckpointError::UnknownRef { entry_id, ref_id }
            if entry_id == "x1" && ref_id == "missing"
    ));

    let used = json.replace("missing", "h1");
    let checkpoint = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new("checkpoint-filter").expect("valid id"),
        candidate(&used),
        manifest("checkpoint-filter", &["h1", "h2"]),
        CheckpointValidationPolicy::default(),
    )
    .expect("known refs should validate");
    assert_eq!(checkpoint.manifest().refs().len(), 1);
    assert_eq!(checkpoint.manifest().refs()[0].id().as_str(), "h1");
}

#[test]
fn first_checkpoint_rejects_handoffs() {
    let json = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"drop\",\"old_id\":\"old\",\"reason\":\"obsolete\"}]",
    );
    let error = CitationBackedCheckpoint::from_candidate(
        CheckpointId::new("checkpoint-initial-handoff").expect("valid id"),
        candidate(&json),
        manifest("checkpoint-initial-handoff", &[]),
        CheckpointValidationPolicy::default(),
    )
    .expect_err("first checkpoint must not have handoffs");
    assert!(matches!(
        error,
        CheckpointError::InitialCheckpointHasHandoffs
    ));
}

#[test]
fn rolling_checkpoint_rejects_silently_missing_old_id() {
    let previous = first_checkpoint(
        &EMPTY_CANDIDATE.replace(
            "  \"exact_details\": []",
            "  \"exact_details\": [{\"id\":\"d1\",\"text\":\"Exact old value\",\"refs\":[\"h1\"]}]",
        ),
        &["h1"],
    );
    let error = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-next").expect("valid id"),
        candidate(EMPTY_CANDIDATE),
        &previous,
        manifest("checkpoint-next", &["h1"]),
        CheckpointValidationPolicy::default(),
    )
    .expect_err("old entry must be accounted for");
    assert!(matches!(
        error,
        CheckpointError::MissingHandoff { old_id } if old_id == "d1"
    ));
}

#[test]
fn keep_requires_byte_identical_entry_in_same_section() {
    let previous_json = EMPTY_CANDIDATE.replace(
        "  \"confirmed_decisions\": []",
        "  \"confirmed_decisions\": [{\"id\":\"d1\",\"text\":\"Keep five turns.\",\"rationale\":\"Preserves continuity.\",\"refs\":[\"h1\",\"h2\"]}]",
    );
    let previous = first_checkpoint(&previous_json, &["h1", "h2"]);
    let keep = |json: String| {
        json.replace(
            "  \"handoffs\": []",
            "  \"handoffs\": [{\"action\":\"keep\",\"old_id\":\"d1\"}]",
        )
    };
    let moved_section = previous_json
        .replace(
            "  \"confirmed_decisions\": [{\"id\":\"d1\",\"text\":\"Keep five turns.\",\"rationale\":\"Preserves continuity.\",\"refs\":[\"h1\",\"h2\"]}]",
            "  \"confirmed_decisions\": []",
        )
        .replace(
            "  \"rejected_approaches\": []",
            "  \"rejected_approaches\": [{\"id\":\"d1\",\"text\":\"Keep five turns.\",\"rationale\":\"Preserves continuity.\",\"refs\":[\"h1\",\"h2\"]}]",
        );
    for (name, changed) in [
        (
            "text",
            keep(previous_json.replace("Keep five turns.", "Keep three turns.")),
        ),
        (
            "rationale",
            keep(previous_json.replace("Preserves continuity.", "Different reason.")),
        ),
        (
            "ref-order",
            keep(previous_json.replace("[\"h1\",\"h2\"]", "[\"h2\",\"h1\"]")),
        ),
        ("section", keep(moved_section)),
    ] {
        let error = CitationBackedCheckpoint::from_rolling_candidate(
            CheckpointId::new(&format!("checkpoint-keep-{name}")).expect("valid id"),
            candidate(&changed),
            &previous,
            manifest(&format!("checkpoint-keep-{name}"), &["h1", "h2"]),
            CheckpointValidationPolicy::default(),
        )
        .expect_err("changed keep must fail");
        assert!(matches!(
            error,
            CheckpointError::InvalidKeep { old_id } if old_id == "d1"
        ));
    }
}

#[test]
fn replace_and_drop_require_valid_targets_and_reasons() {
    let previous = first_checkpoint(
        &EMPTY_CANDIDATE.replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"q1\",\"text\":\"Old question\",\"refs\":[\"h1\"]}]",
        ),
        &["h1"],
    );
    let replace_without_ids = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"replace\",\"old_id\":\"q1\",\"new_ids\":[],\"reason\":\"refined\"}]",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&replace_without_ids),
        Err(CheckpointError::ReplacementWithoutNewIds { old_id }) if old_id == "q1"
    ));

    let drop_without_reason = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"drop\",\"old_id\":\"q1\",\"reason\":\"  \"}]",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&drop_without_reason),
        Err(CheckpointError::BlankField {
            field: "checkpoint handoff reason"
        })
    ));

    let missing_target = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"replace\",\"old_id\":\"q1\",\"new_ids\":[\"q2\"],\"reason\":\"refined\"}]",
    );
    let error = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-replace").expect("valid id"),
        candidate(&missing_target),
        &previous,
        manifest("checkpoint-replace", &["h1"]),
        CheckpointValidationPolicy::default(),
    )
    .expect_err("replace target must exist");
    assert!(matches!(
        error,
        CheckpointError::ReplacementEntryNotFound { old_id, new_id }
            if old_id == "q1" && new_id == "q2"
    ));
}

#[test]
fn rolling_checkpoint_rejects_duplicate_and_unknown_old_ids() {
    let previous = first_checkpoint(
        &EMPTY_CANDIDATE.replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"q1\",\"text\":\"Old question\",\"refs\":[\"h1\"]}]",
        ),
        &["h1"],
    );
    let duplicate = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"drop\",\"old_id\":\"q1\",\"reason\":\"done\"},{\"action\":\"drop\",\"old_id\":\"q1\",\"reason\":\"still done\"}]",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&duplicate),
        Err(CheckpointError::DuplicateHandoff { old_id }) if old_id == "q1"
    ));

    let unknown = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"drop\",\"old_id\":\"other\",\"reason\":\"done\"}]",
    );
    let error = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-unknown-old").expect("valid id"),
        candidate(&unknown),
        &previous,
        manifest("checkpoint-unknown-old", &["h1"]),
        CheckpointValidationPolicy::default(),
    )
    .expect_err("unknown old id must fail");
    assert!(matches!(
        error,
        CheckpointError::UnknownHandoffOldId { old_id } if old_id == "other"
    ));
}

#[test]
fn replacement_ids_must_be_unique() {
    let duplicate = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"replace\",\"old_id\":\"q1\",\"new_ids\":[\"q2\",\"q2\"],\"reason\":\"refined\"}]",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&duplicate),
        Err(CheckpointError::DuplicateReplacementEntry { old_id, new_id })
            if old_id == "q1" && new_id == "q2"
    ));
}

#[test]
fn replacement_targets_must_be_new_but_can_merge_multiple_old_entries() {
    let previous_json = EMPTY_CANDIDATE.replace(
        "  \"open_questions\": []",
        "  \"open_questions\": [{\"id\":\"old1\",\"text\":\"First old question\",\"refs\":[\"h1\"]},{\"id\":\"old2\",\"text\":\"Second old question\",\"refs\":[\"h2\"]}]",
    );
    let previous = first_checkpoint(&previous_json, &["h1", "h2"]);
    let points_at_old = EMPTY_CANDIDATE
        .replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"old1\",\"text\":\"First old question\",\"refs\":[\"h1\"]}]",
        )
        .replace(
            "  \"handoffs\": []",
            "  \"handoffs\": [{\"action\":\"keep\",\"old_id\":\"old1\"},{\"action\":\"replace\",\"old_id\":\"old2\",\"new_ids\":[\"old1\"],\"reason\":\"Incorrectly points at retained history.\"}]",
        );
    let error = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-replace-old-target").expect("valid id"),
        candidate(&points_at_old),
        &previous,
        manifest("checkpoint-replace-old-target", &["h1", "h2"]),
        CheckpointValidationPolicy::default(),
    )
    .expect_err("replacement targets must be genuinely new ids");
    assert!(matches!(
        error,
        CheckpointError::ReplacementEntryNotNew { old_id, new_id }
            if old_id == "old2" && new_id == "old1"
    ));

    let merged = EMPTY_CANDIDATE
        .replace(
            "  \"durable_conclusions\": []",
            "  \"durable_conclusions\": [{\"id\":\"merged1\",\"text\":\"Both questions were resolved together.\",\"refs\":[\"h1\",\"h2\"]}]",
        )
        .replace(
            "  \"handoffs\": []",
            "  \"handoffs\": [{\"action\":\"replace\",\"old_id\":\"old1\",\"new_ids\":[\"merged1\"],\"reason\":\"Merged resolution.\"},{\"action\":\"replace\",\"old_id\":\"old2\",\"new_ids\":[\"merged1\"],\"reason\":\"Merged resolution.\"}]",
        );
    CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-many-to-one-replace").expect("valid id"),
        candidate(&merged),
        &previous,
        manifest("checkpoint-many-to-one-replace", &["h1", "h2"]),
        CheckpointValidationPolicy::default(),
    )
    .expect("multiple old entries may merge into one genuinely new entry");
}

#[test]
fn candidate_and_handoff_objects_reject_unknown_fields() {
    let top_level = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"unexpected\": [],\n  \"handoffs\": []",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&top_level),
        Err(CheckpointError::InvalidCandidateJson)
    ));

    let handoff = EMPTY_CANDIDATE.replace(
        "  \"handoffs\": []",
        "  \"handoffs\": [{\"action\":\"keep\",\"old_id\":\"q1\",\"reason\":\"not allowed\"}]",
    );
    assert!(matches!(
        CompactedCheckpointCandidate::from_json(&handoff),
        Err(CheckpointError::InvalidCandidateJson)
    ));
}

#[test]
fn replace_and_drop_must_remove_the_old_entry() {
    let previous_json = EMPTY_CANDIDATE.replace(
        "  \"open_questions\": []",
        "  \"open_questions\": [{\"id\":\"q1\",\"text\":\"Old question\",\"refs\":[\"h1\"]}]",
    );
    let previous = first_checkpoint(&previous_json, &["h1"]);

    for (action, tail, expected) in [
        (
            "replace",
            ",\"new_ids\":[\"q1\"],\"reason\":\"refined\"",
            "replace",
        ),
        ("drop", ",\"reason\":\"obsolete\"", "drop"),
    ] {
        let next = previous_json.replace(
            "  \"handoffs\": []",
            &format!("  \"handoffs\": [{{\"action\":\"{action}\",\"old_id\":\"q1\"{tail}}}]"),
        );
        let error = CitationBackedCheckpoint::from_rolling_candidate(
            CheckpointId::new(&format!("checkpoint-{action}-retains-old")).expect("valid id"),
            candidate(&next),
            &previous,
            manifest(&format!("checkpoint-{action}-retains-old"), &["h1"]),
            CheckpointValidationPolicy::default(),
        )
        .expect_err("replace/drop must remove old entry");
        match expected {
            "replace" => assert!(matches!(
                error,
                CheckpointError::InvalidReplace { old_id } if old_id == "q1"
            )),
            "drop" => assert!(matches!(
                error,
                CheckpointError::InvalidDrop { old_id } if old_id == "q1"
            )),
            _ => unreachable!(),
        }
    }
}

#[test]
fn rolling_checkpoint_accepts_keep_replace_drop_and_unrelated_new_entries() {
    let previous_json = EMPTY_CANDIDATE
        .replace(
            "  \"constraints_preferences_boundaries\": []",
            "  \"constraints_preferences_boundaries\": [{\"id\":\"keep1\",\"text\":\"Rust only.\",\"refs\":[\"h1\"]}]",
        )
        .replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"replace1\",\"text\":\"Old question\",\"refs\":[\"h2\"]},{\"id\":\"drop1\",\"text\":\"Obsolete question\",\"refs\":[\"h3\"]}]",
        );
    let previous = first_checkpoint(&previous_json, &["h1", "h2", "h3"]);
    let next = EMPTY_CANDIDATE
        .replace(
            "  \"constraints_preferences_boundaries\": []",
            "  \"constraints_preferences_boundaries\": [{\"id\":\"keep1\",\"text\":\"Rust only.\",\"refs\":[\"h1\"]}]",
        )
        .replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"replace2\",\"text\":\"Refined question\",\"refs\":[\"h2\"]}]",
        )
        .replace(
            "  \"exact_details\": []",
            "  \"exact_details\": [{\"id\":\"new1\",\"text\":\"/exact/path\",\"refs\":[\"h4\"]}]",
        )
        .replace(
            "  \"handoffs\": []",
            "  \"handoffs\": [{\"action\":\"keep\",\"old_id\":\"keep1\"},{\"action\":\"replace\",\"old_id\":\"replace1\",\"new_ids\":[\"replace2\"],\"reason\":\"Question refined.\"},{\"action\":\"drop\",\"old_id\":\"drop1\",\"reason\":\"No longer relevant.\"}]",
        );
    let checkpoint = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-valid-roll").expect("valid id"),
        candidate(&next),
        &previous,
        manifest("checkpoint-valid-roll", &["h1", "h2", "h3", "h4"]),
        CheckpointValidationPolicy::default(),
    )
    .expect("valid handoffs and unrelated new entries should pass");

    assert_eq!(checkpoint.sections().entry_count(), 3);
    assert_eq!(checkpoint.handoffs().len(), 3);
    assert_eq!(
        checkpoint
            .manifest()
            .refs()
            .iter()
            .map(|reference| reference.id().as_str())
            .collect::<Vec<_>>(),
        ["h1", "h2", "h4"]
    );
}

#[test]
fn render_has_fixed_section_order_and_hides_handoffs() {
    let mut json = EMPTY_CANDIDATE.to_owned();
    for (section, id) in [
        ("confirmed_decisions", "d1"),
        ("rejected_approaches", "r1"),
        ("constraints_preferences_boundaries", "c1"),
        ("corrected_misunderstandings", "m1"),
        ("durable_conclusions", "u1"),
        ("open_questions", "q1"),
        ("current_progress_and_next_steps", "p1"),
        ("exact_details", "x1"),
    ] {
        let rationale = matches!(section, "confirmed_decisions" | "rejected_approaches")
            .then_some(",\"rationale\":\"required reason\"")
            .unwrap_or_default();
        json = json.replace(
            &format!("  \"{section}\": []"),
            &format!(
                "  \"{section}\": [{{\"id\":\"{id}\",\"text\":\"{section} text\"{rationale},\"refs\":[\"h1\"]}}]"
            ),
        );
    }
    let checkpoint = first_checkpoint(&json, &["h1"]);
    let text = checkpoint.render_prompt_text();
    let headings = CheckpointSection::ALL.map(CheckpointSection::as_str);
    for pair in headings.windows(2) {
        assert!(
            text.find(&format!("{}:", pair[0])).expect("first heading")
                < text.find(&format!("{}:", pair[1])).expect("second heading")
        );
    }
    assert!(!text.contains("handoff"));
    assert!(!text.contains("drop reason used only for validation"));
    assert!(text.contains("reason: required reason"));
    assert!(text.contains("refs: [h1]"));
}

#[test]
fn persisted_checkpoint_keeps_prevalidated_pinned_refs() {
    let json = EMPTY_CANDIDATE.replace(
        "  \"exact_details\": []",
        "  \"exact_details\": [{\"id\":\"x1\",\"text\":\"Exact\",\"refs\":[\"h1\"]}]",
    );
    let pinned = [ref_id("h2")].into_iter().collect();
    let checkpoint = CitationBackedCheckpoint::from_candidate_with_pinned_refs(
        CheckpointId::new("checkpoint-pinned").expect("valid id"),
        candidate(&json),
        manifest("checkpoint-pinned", &["h1", "h2"]),
        CheckpointValidationPolicy::default(),
        &pinned,
    )
    .expect("checkpoint should keep explicit pin");

    let loaded = CitationBackedCheckpoint::from_persisted(checkpoint.persisted())
        .expect("persisted checkpoint loads");
    assert_eq!(
        loaded
            .manifest()
            .refs()
            .iter()
            .map(|reference| reference.id().as_str())
            .collect::<Vec<_>>(),
        ["h1", "h2"]
    );
}

#[test]
fn persisted_checkpoint_round_trips_sections_and_handoffs() {
    let previous = first_checkpoint(
        &EMPTY_CANDIDATE.replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"q1\",\"text\":\"Old question\",\"refs\":[\"h1\"]}]",
        ),
        &["h1"],
    );
    let next = EMPTY_CANDIDATE
        .replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"q2\",\"text\":\"Refined question\",\"rationale\":\"More precise.\",\"refs\":[\"h2\"]}]",
        )
        .replace(
            "  \"handoffs\": []",
            "  \"handoffs\": [{\"action\":\"replace\",\"old_id\":\"q1\",\"new_ids\":[\"q2\"],\"reason\":\"Question refined.\"}]",
        );
    let checkpoint = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-persisted-roll").expect("valid id"),
        candidate(&next),
        &previous,
        manifest("checkpoint-persisted-roll", &["h1", "h2"]),
        CheckpointValidationPolicy::default(),
    )
    .expect("rolling checkpoint validates");

    let loaded = CitationBackedCheckpoint::from_persisted(checkpoint.persisted())
        .expect("persisted rolling checkpoint loads");
    assert_eq!(loaded.sections(), checkpoint.sections());
    assert_eq!(loaded.handoffs(), checkpoint.handoffs());
}

#[test]
fn persisted_checkpoint_revalidates_current_handoff_invariants() {
    let previous = first_checkpoint(
        &EMPTY_CANDIDATE.replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"q1\",\"text\":\"Old question\",\"refs\":[\"h1\"]}]",
        ),
        &["h1"],
    );
    let next = EMPTY_CANDIDATE
        .replace(
            "  \"open_questions\": []",
            "  \"open_questions\": [{\"id\":\"q2\",\"text\":\"Refined question\",\"refs\":[\"h2\"]}]",
        )
        .replace(
            "  \"handoffs\": []",
            "  \"handoffs\": [{\"action\":\"replace\",\"old_id\":\"q1\",\"new_ids\":[\"q2\"],\"reason\":\"Question refined.\"}]",
        );
    let checkpoint = CitationBackedCheckpoint::from_rolling_candidate(
        CheckpointId::new("checkpoint-persisted-invariants").expect("valid id"),
        candidate(&next),
        &previous,
        manifest("checkpoint-persisted-invariants", &["h1", "h2"]),
        CheckpointValidationPolicy::default(),
    )
    .expect("rolling checkpoint validates");
    let persisted = serde_json::to_value(checkpoint.persisted()).expect("persisted serializes");

    for (name, handoff, expected) in [
        (
            "keep-missing",
            serde_json::json!({"action":"keep","old_id":"q1"}),
            "keep",
        ),
        (
            "replace-retained",
            serde_json::json!({"action":"replace","old_id":"q2","new_ids":["q2"],"reason":"invalid"}),
            "replace",
        ),
        (
            "drop-retained",
            serde_json::json!({"action":"drop","old_id":"q2","reason":"invalid"}),
            "drop",
        ),
        (
            "replace-missing-target",
            serde_json::json!({"action":"replace","old_id":"q1","new_ids":["missing"],"reason":"invalid"}),
            "missing-target",
        ),
    ] {
        let mut invalid = persisted.clone();
        invalid["handoffs"] = serde_json::json!([handoff]);
        let invalid = serde_json::from_value::<PersistedCitationBackedCheckpoint>(invalid)
            .expect("mutated persistence shape parses");
        let error = CitationBackedCheckpoint::from_persisted(invalid)
            .expect_err("invalid persisted handoff must be rejected");
        match expected {
            "keep" => assert!(matches!(
                error,
                CheckpointError::InvalidKeep { old_id } if old_id == "q1"
            )),
            "replace" => assert!(matches!(
                error,
                CheckpointError::InvalidReplace { old_id } if old_id == "q2"
            )),
            "drop" => assert!(matches!(
                error,
                CheckpointError::InvalidDrop { old_id } if old_id == "q2"
            )),
            "missing-target" => assert!(matches!(
                error,
                CheckpointError::ReplacementEntryNotFound { old_id, new_id }
                    if old_id == "q1" && new_id == "missing"
            )),
            _ => unreachable!("unexpected case {name}"),
        }
    }

    let mut invalid = persisted;
    invalid["handoffs"] = serde_json::json!([
        {"action":"keep","old_id":"q2"},
        {"action":"replace","old_id":"q1","new_ids":["q2"],"reason":"invalid"}
    ]);
    let invalid = serde_json::from_value::<PersistedCitationBackedCheckpoint>(invalid)
        .expect("mutated persistence shape parses");
    let error = CitationBackedCheckpoint::from_persisted(invalid)
        .expect_err("replacement target declared as an old id must be rejected");
    assert!(matches!(
        error,
        CheckpointError::ReplacementEntryNotNew { old_id, new_id }
            if old_id == "q1" && new_id == "q2"
    ));
}

#[test]
fn runtime_history_ref_namespace_requires_h_and_decimal_digits() {
    for reserved in ["h0", "h1", "h42", "h001"] {
        assert!(ref_id(reserved).is_runtime_history_ref());
    }
    for available in ["h", "history", "h1x", "bootstrap-ref", "r1"] {
        assert!(!ref_id(available).is_runtime_history_ref());
    }
}
