//! Prose compaction summary: prompt, salvage, and continuation text.
//!
//! The prompt is grok-build's full-replace handoff. The model is asked for one
//! `<summary>` block. Acceptance does not require the numbered sections,
//! balanced tags, or JSON. Only an empty cleaned result is retried; a short
//! summary that still carries the task is installed, because a failed
//! compaction would block the next turn.

/// Instructions appended to a compaction request.
///
/// `user_context` is optional text from an explicit compact request. Automatic
/// compaction passes `None`. The covered conversation is already in the request
/// ahead of this text, or in the passive history block when the full request
/// does not fit.
pub fn citation_compaction_tail_directive(user_context: Option<&str>) -> String {
    let user_context_section = match user_context {
        Some(context) => format!(
            "\n\n**User-provided context for this compaction:**\n{context}\n\nPlease incorporate this context into your summary, ensuring it is prominently addressed in the relevant sections.\n\n"
        ),
        None => String::new(),
    };
    include_str!("templates/full_replace_summary_prompt.txt")
        .replace("{user_context_section}", &user_context_section)
}

/// Cleans model output into the text that seeds the next turn.
///
/// Leading `<analysis>` scratchpads are removed. A `<summary>` wrapper becomes
/// plain text. Tags quoted inside the body are neutralized. Plain text with no
/// tags is kept. Control characters other than newline and tab are stripped so
/// a messy answer can still be stored.
pub(crate) fn format_compact_summary(summary: &str) -> String {
    let mut result = summary.to_string();

    while let Some(start) = result.find("<analysis>") {
        let is_leading = match result.find("<summary>") {
            Some(summary_at) => {
                start < summary_at
                    || result[summary_at + "<summary>".len()..start]
                        .trim()
                        .is_empty()
            }
            None => result[..start].trim().is_empty(),
        };
        if !is_leading {
            break;
        }
        match result[start..].find("</analysis>") {
            Some(relative) => {
                let end = start + relative + "</analysis>".len();
                result = format!("{}{}", &result[..start], &result[end..]);
            }
            None => {
                let drop_to = result[start..]
                    .find("<summary>")
                    .map_or(result.len(), |relative| start + relative);
                result = format!("{}{}", &result[..start], &result[drop_to..]);
                break;
            }
        }
    }

    if let Some(start) = result.find("<summary>")
        && let Some(end) = result.rfind("</summary>")
        && end > start
    {
        let before = result[..start].to_string();
        let after = result[end + "</summary>".len()..].to_string();
        let inner = strip_leading_scratchpad(result[start + "<summary>".len()..end].trim());
        result = format!("{before}Summary:\n{inner}{after}");
    } else if let Some(start) = result.find("<summary>") {
        result = result[start + "<summary>".len()..].to_string();
    }

    result = neutralize_control_tokens(&result);
    result = strip_disallowed_controls(&result);
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}

/// A cleaned summary that is too small to replace the covered conversation.
pub(crate) fn is_degenerate_summary(raw_summary: &str) -> bool {
    format_compact_summary(raw_summary).is_empty()
}

/// Continuation text installed in place of the covered turns.
pub(crate) fn format_continuation(cleaned_summary: &str, segment_hint: Option<&str>) -> String {
    let mut text = format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation.\n\n{cleaned_summary}"
    );
    if let Some(hint) = segment_hint {
        text.push_str("\n\n");
        text.push_str(hint);
    }
    text
}

fn strip_leading_scratchpad(inner: &str) -> String {
    let mut text = inner.trim();
    let lead = text.trim_start_matches(['#', '*', '-', '>', ' ', '\t']);
    if !lead.starts_with(|character: char| character.is_ascii_digit())
        && let Some(position) = text.rfind("</analysis>")
    {
        text = text[position + "</analysis>".len()..].trim_start();
    }
    if let Some(rest) = text.strip_prefix("<summary>") {
        text = rest.trim_start();
    }
    text.to_string()
}

fn neutralize_control_tokens(text: &str) -> String {
    text.replace("</summary>", "<\u{200b}/summary>")
        .replace("<summary>", "<\u{200b}summary>")
        .replace("</analysis>", "<\u{200b}/analysis>")
        .replace("<analysis>", "<\u{200b}analysis>")
        .replace("</summary_request>", "<\u{200b}/summary_request>")
        .replace("<summary_request>", "<\u{200b}summary_request>")
}

fn strip_disallowed_controls(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_kept() {
        assert_eq!(
            format_compact_summary("Just plain text summary."),
            "Just plain text summary."
        );
    }

    #[test]
    fn analysis_scratchpad_is_dropped_and_summary_body_kept() {
        let input = "<analysis>\nThinking about the problem...\n</analysis>\n\n<summary>\n1. Primary Request: Fix the bug\n</summary>";
        let result = format_compact_summary(input);
        assert!(!result.contains("Thinking about the problem"));
        assert!(result.contains("Summary:\n1. Primary Request: Fix the bug"));
        assert!(!result.contains("<analysis>"));
        assert!(!result.contains("<summary>"));
    }

    #[test]
    fn unclosed_summary_keeps_the_body() {
        let result = format_compact_summary(
            "<summary>\n1. Primary Request: do the thing\n9. Optional Next Step: continue",
        );
        assert!(result.contains("1. Primary Request: do the thing"));
        assert!(result.contains("9. Optional Next Step: continue"));
    }

    #[test]
    fn quoted_summary_tag_does_not_truncate_later_sections() {
        let raw = "<summary>\n1. Primary Request and Intent: build app\n6. All user messages: respond with ONLY the <summary> block.\n9. Optional Next Step: rerun\n</summary>";
        let result = format_compact_summary(raw);
        assert!(result.contains("1. Primary Request and Intent: build app"));
        assert!(result.contains("9. Optional Next Step: rerun"));
        assert!(!result.contains("<summary>"));
    }

    #[test]
    fn empty_summary_is_degenerate_and_a_short_one_is_kept() {
        assert!(is_degenerate_summary(
            "<analysis>\nonly scratchpad\n</analysis>"
        ));
        assert!(is_degenerate_summary("   \n\t"));
        assert!(!is_degenerate_summary(
            "<summary>\n1. Primary Request: q\n</summary>"
        ));
    }

    #[test]
    fn continuation_names_the_segment_directory_when_one_was_written() {
        let text = format_continuation("kept the auth decision", Some("read /tmp/compaction"));
        assert!(text.contains("continued from a previous conversation"));
        assert!(text.contains("kept the auth decision"));
        assert!(text.contains("read /tmp/compaction"));
    }
}
