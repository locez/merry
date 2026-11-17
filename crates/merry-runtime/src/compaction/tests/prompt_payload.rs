use crate::compaction::citation_compaction_tail_directive;

#[test]
fn compaction_directive_is_the_full_replace_prompt() {
    let prompt = citation_compaction_tail_directive(None);

    assert!(prompt.contains("faithful, concise summary"));
    assert!(prompt.contains("do NOT emit a separate analysis block"));
    assert!(prompt.contains("<summary>"));
    assert!(prompt.contains("</summary>"));
    assert!(
        !prompt.contains("available_ref_ids"),
        "the summary request must not require citation ids"
    );
    assert!(!prompt.contains("{user_context_section}"));
    assert!(!prompt.contains("**User-provided context for this compaction:**"));
}

#[test]
fn compaction_directive_asks_for_sections_without_rejecting_prose() {
    let prompt = citation_compaction_tail_directive(Some("focus on auth"));

    for section in [
        "1. Primary Request and Intent:",
        "2. Key Technical Concepts:",
        "3. Files and Code Sections:",
        "4. Errors and Fixes:",
        "5. Problem Solving:",
        "6. All User Messages:",
        "7. Pending Tasks:",
        "8. Current Work:",
        "9. Optional Next Step:",
    ] {
        assert!(prompt.contains(section), "missing {section}");
    }
    assert!(prompt.contains("Do NOT call or use any tools"));
    assert!(prompt.contains("**User-provided context for this compaction:**\nfocus on auth"));
    assert!(prompt.contains("<summary>"));
}
