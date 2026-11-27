//! Deterministic Markdown archive formatter for compacted session segments.
//!
//! Produces human- and agent-readable `archive/seg_NNNN.md` files alongside
//! `archive/INDEX.md` with zero LLM calls, reusing the compaction summary and
//! formatting the pruned transcript items deterministically with UTC time ranges.

use crate::session::transcript::TranscriptItem;
use merry_core::{SessionId, ToolCallResultStatus};
use std::{
    collections::BTreeMap,
    fmt::Write,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_TOOL_OUTPUT_MD_BYTES: usize = 2048;
const MAX_TOOL_ARGS_MD_BYTES: usize = 1024;
const MAX_INDEX_SUMMARY_CHARS: usize = 120;
const MAX_KEYWORDS: usize = 10;

/// Sorted lowercase stopword list for zero-allocation binary search keyword filtering.
const STOPWORDS: &[&str] = &[
    "about",
    "after",
    "also",
    "and",
    "are",
    "assistant",
    "been",
    "before",
    "being",
    "between",
    "compacted",
    "could",
    "each",
    "for",
    "from",
    "have",
    "into",
    "just",
    "message",
    "more",
    "only",
    "other",
    "over",
    "session",
    "should",
    "some",
    "such",
    "summary",
    "than",
    "that",
    "the",
    "their",
    "then",
    "there",
    "this",
    "through",
    "turn",
    "under",
    "user",
    "very",
    "was",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "will",
    "with",
    "would",
];

/// Pre-rendered segment data waiting for its assigned sequence number on commit.
#[derive(Debug)]
pub(crate) struct PreparedSegmentMarkdown {
    pub(crate) session_id: SessionId,
    pub(crate) turn_range: (u64, u64),
    pub(crate) time_range_label: String,
    pub(crate) compaction_summary: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) stats: SegmentStats,
    pub(crate) body_turns_markdown: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SegmentStats {
    pub(crate) user_messages: usize,
    pub(crate) assistant_messages: usize,
    pub(crate) tool_calls: usize,
    pub(crate) failed_tool_calls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexRow {
    pub(crate) segment_number: u32,
    pub(crate) time_range_label: String,
    pub(crate) turn_range_label: String,
    pub(crate) summary_excerpt: String,
    pub(crate) keywords_label: String,
    pub(crate) files_label: String,
}

impl PreparedSegmentMarkdown {
    /// Builds deterministic Markdown content and metadata from pruned transcript items.
    pub(crate) fn build(
        session_id: SessionId,
        pruned_items: &[TranscriptItem],
        compacted_through: u64,
        compaction_summary: &str,
        time_range_epoch_secs: Option<(u64, u64)>,
    ) -> Self {
        let min_turn = pruned_items
            .iter()
            .map(|item| item.model_turn_id().as_u64())
            .min()
            .unwrap_or(compacted_through);
        let max_turn = pruned_items
            .iter()
            .map(|item| item.model_turn_id().as_u64())
            .max()
            .unwrap_or(compacted_through);

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let (start_secs, end_secs) = time_range_epoch_secs.unwrap_or((now_secs, now_secs));
        let time_range_label = format_time_range_utc(start_secs, end_secs);

        let mut stats = SegmentStats::default();
        let mut keyword_sources = String::new();
        keyword_sources.push_str(compaction_summary);
        keyword_sources.push('\n');

        let mut body = String::new();
        for item in pruned_items {
            let turn_id = item.model_turn_id().as_u64();
            match item {
                TranscriptItem::UserMessage { text, images, .. } => {
                    stats.user_messages += 1;
                    keyword_sources.push_str(text);
                    keyword_sources.push('\n');
                    let _ = writeln!(body, "### Turn {turn_id} — User\n");
                    body.push_str(text.trim());
                    body.push('\n');
                    if !images.is_empty() {
                        let _ = writeln!(body, "\n_({} attached image(s))_", images.len());
                    }
                    body.push('\n');
                }
                TranscriptItem::AssistantText { text, .. } => {
                    stats.assistant_messages += 1;
                    let _ = writeln!(body, "### Turn {turn_id} — Assistant\n");
                    body.push_str(text.trim());
                    body.push_str("\n\n");
                }
                TranscriptItem::AssistantReasoning { .. } => {}
                TranscriptItem::ToolCall { call, .. } => {
                    stats.tool_calls += 1;
                    keyword_sources.push_str(call.name().as_str());
                    keyword_sources.push('\n');
                    let args_json = serde_json::to_string(call.arguments().as_object())
                        .unwrap_or_else(|_| "{}".to_owned());
                    let (args_preview, truncated) =
                        truncate_utf8_bytes(&args_json, MAX_TOOL_ARGS_MD_BYTES);
                    let _ = writeln!(
                        body,
                        "#### Turn {turn_id} — Tool Call `{}` (`{}`)\n\n```json\n{args_preview}\n```",
                        call.name().as_str(),
                        call.id().as_str(),
                    );
                    if truncated {
                        let _ = writeln!(
                            body,
                            "\n_Arguments truncated ({} bytes total)._",
                            args_json.len()
                        );
                    }
                    body.push('\n');
                }
                TranscriptItem::ToolResult {
                    call_id,
                    result,
                    payload,
                    ..
                } => {
                    let status_label = match result.status() {
                        ToolCallResultStatus::Succeeded => "succeeded",
                        ToolCallResultStatus::Failed => {
                            stats.failed_tool_calls += 1;
                            "failed"
                        }
                    };
                    let full_text = payload.full().as_str();
                    let (output_preview, truncated) =
                        truncate_utf8_bytes(full_text, MAX_TOOL_OUTPUT_MD_BYTES);
                    let _ = writeln!(
                        body,
                        "#### Turn {turn_id} — Tool Result (`{}`, {status_label})\n\n```text\n{output_preview}\n```",
                        call_id.as_str(),
                    );
                    if truncated {
                        let _ = writeln!(
                            body,
                            "\n_Output truncated ({} bytes total; full output retained in matching `.jsonl` segment)._",
                            full_text.len()
                        );
                    }
                    body.push('\n');
                }
            }
        }

        let keywords = extract_keywords(&keyword_sources, MAX_KEYWORDS);
        Self {
            session_id,
            turn_range: (min_turn, max_turn),
            time_range_label,
            compaction_summary: compaction_summary.trim().to_owned(),
            keywords,
            stats,
            body_turns_markdown: body,
        }
    }

    /// Renders the full `seg_NNNN.md` document for `segment_number`.
    pub(crate) fn render_document(&self, segment_number: u32) -> String {
        let mut out = String::with_capacity(self.body_turns_markdown.len() + 320);
        let _ = writeln!(out, "# Session Archive — seg_{segment_number:04}\n");
        let _ = writeln!(out, "- **Session**: `{}`", self.session_id.as_str());
        let _ = writeln!(out, "- **Time (UTC)**: `{}`", self.time_range_label);
        let _ = writeln!(
            out,
            "- **Model Turns**: `{}..={}`",
            self.turn_range.0, self.turn_range.1
        );
        let _ = writeln!(
            out,
            "- **Stats**: {} user message(s), {} assistant response(s), {} tool call(s) ({} failed)",
            self.stats.user_messages,
            self.stats.assistant_messages,
            self.stats.tool_calls,
            self.stats.failed_tool_calls
        );
        out.push_str("- **Keywords**: ");
        if self.keywords.is_empty() {
            out.push_str("_none_");
        } else {
            for (index, kw) in self.keywords.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "`{kw}`");
            }
        }
        out.push_str("\n\n## Compaction Summary\n\n");
        out.push_str(&self.compaction_summary);
        out.push_str("\n\n## Transcript\n\n");
        out.push_str(&self.body_turns_markdown);
        out
    }

    /// Builds the `INDEX.md` table row representing this segment.
    pub(crate) fn index_row(&self, segment_number: u32) -> IndexRow {
        let seg_name = format!("seg_{segment_number:04}");
        let summary_excerpt = one_line_excerpt(&self.compaction_summary, MAX_INDEX_SUMMARY_CHARS);
        let keywords_label = if self.keywords.is_empty() {
            "-".to_owned()
        } else {
            let mut joined = String::new();
            for (index, kw) in self.keywords.iter().take(6).enumerate() {
                if index > 0 {
                    joined.push_str(", ");
                }
                let _ = write!(joined, "`{kw}`");
            }
            joined
        };
        IndexRow {
            segment_number,
            time_range_label: format!("`{}`", self.time_range_label),
            turn_range_label: format!("`{}..={}`", self.turn_range.0, self.turn_range.1),
            summary_excerpt,
            keywords_label,
            files_label: format!("`{seg_name}.md`, `{seg_name}.jsonl`"),
        }
    }
}

/// Parses previously written rows from `archive/INDEX.md`, preserving escaped
/// `\|` pipes inside summary cells.
pub(crate) fn parse_index_markdown(content: &str) -> Vec<IndexRow> {
    let mut rows = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| `seg_") {
            continue;
        }
        let cells = split_markdown_table_row(trimmed);
        if cells.len() < 6 {
            continue;
        }
        let Some(segment_number) = parse_segment_cell(&cells[0]) else {
            continue;
        };
        rows.push(IndexRow {
            segment_number,
            time_range_label: cells[1].clone(),
            turn_range_label: cells[2].clone(),
            summary_excerpt: cells[3].clone(),
            keywords_label: cells[4].clone(),
            files_label: cells[5].clone(),
        });
    }
    rows
}

/// Renders the full `archive/INDEX.md` document for all surviving segments.
pub(crate) fn render_index_markdown(session_id: &SessionId, rows: &[IndexRow]) -> String {
    let mut out = String::new();
    out.push_str("# Session Archive Index\n\n");
    let _ = writeln!(out, "Session: `{}`\n", session_id.as_str());
    out.push_str("| Segment | Time (UTC) | Model Turns | Summary | Keywords | Files |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for row in rows {
        let _ = writeln!(
            out,
            "| `seg_{:04}` | {} | {} | {} | {} | {} |",
            row.segment_number,
            row.time_range_label,
            row.turn_range_label,
            row.summary_excerpt,
            row.keywords_label,
            row.files_label,
        );
    }
    out
}

/// Formats `[start_secs, end_secs]` as a compact human- and agent-readable UTC time range.
fn format_time_range_utc(start_secs: u64, end_secs: u64) -> String {
    let (lo, hi) = if start_secs <= end_secs {
        (start_secs, end_secs)
    } else {
        (end_secs, start_secs)
    };
    let (sy, sm, sd, sh, smin) = civil_utc_from_epoch_secs(lo);
    let (ey, em, ed, eh, emin) = civil_utc_from_epoch_secs(hi);
    if (sy, sm, sd, sh, smin) == (ey, em, ed, eh, emin) {
        format!("{sy:04}-{sm:02}-{sd:02} {sh:02}:{smin:02} UTC")
    } else if (sy, sm, sd) == (ey, em, ed) {
        format!("{sy:04}-{sm:02}-{sd:02} {sh:02}:{smin:02}~{eh:02}:{emin:02} UTC")
    } else {
        format!(
            "{sy:04}-{sm:02}-{sd:02} {sh:02}:{smin:02} ~ {ey:04}-{em:02}-{ed:02} {eh:02}:{emin:02} UTC"
        )
    }
}

/// Converts Unix epoch seconds to `(year, month, day, hour, minute)` in UTC
/// using Howard Hinnant's civil-from-days algorithm.
fn civil_utc_from_epoch_secs(epoch_secs: u64) -> (i64, u32, u32, u32, u32) {
    let days = (epoch_secs / 86_400) as i64;
    let rem = (epoch_secs % 86_400) as u32;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day, hour, minute)
}

/// Splits one pipe-delimited Markdown table row into trimmed cells without
/// splitting on backslash-escaped pipes (`\|`).
fn split_markdown_table_row(line: &str) -> Vec<String> {
    let inner = line
        .strip_prefix('|')
        .unwrap_or(line)
        .strip_suffix('|')
        .unwrap_or(line);
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            current.push(ch);
            escaped = true;
        } else if ch == '|' {
            cells.push(current.trim().to_owned());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    cells.push(current.trim().to_owned());
    cells
}

fn parse_segment_cell(cell: &str) -> Option<u32> {
    let inner = cell.trim().trim_matches('`');
    inner.strip_prefix("seg_")?.parse::<u32>().ok()
}

fn one_line_excerpt(text: &str, max_chars: usize) -> String {
    let collapsed = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|");
    let mut chars = collapsed.chars();
    let excerpt = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else if excerpt.is_empty() {
        "-".to_owned()
    } else {
        excerpt
    }
}

fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn extract_keywords(source: &str, limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();

    for raw in source.split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-') {
        let token = raw.trim_matches(|ch| ch == '_' || ch == '-').to_lowercase();
        if token.chars().count() < 3 || token.len() > 40 {
            continue;
        }
        if token.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        if STOPWORDS.binary_search(&token.as_str()).is_ok() {
            continue;
        }
        *counts.entry(token).or_insert(0) += 1;
    }

    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(a_word, a_count), (b_word, b_count)| {
        b_count.cmp(a_count).then_with(|| a_word.cmp(b_word))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwords_list_is_strictly_sorted_for_binary_search() {
        for pair in STOPWORDS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "STOPWORDS must stay strictly sorted without duplicates: {:?} >= {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn civil_utc_and_time_range_formatting_match_known_dates() {
        assert_eq!(civil_utc_from_epoch_secs(0), (1970, 1, 1, 0, 0));
        // 2024-02-29 23:59:00 UTC (leap day).
        assert_eq!(
            civil_utc_from_epoch_secs(1_709_251_140),
            (2024, 2, 29, 23, 59)
        );
        // Same-day range collapses the end date.
        assert_eq!(
            format_time_range_utc(1_709_200_200, 1_709_205_720),
            "2024-02-29 09:50~11:22 UTC"
        );
        // Multi-day range shows both full dates.
        assert_eq!(
            format_time_range_utc(1_709_251_140, 1_709_251_260),
            "2024-02-29 23:59 ~ 2024-03-01 00:01 UTC"
        );
    }

    #[test]
    fn index_markdown_round_trips_summaries_containing_escaped_pipes_and_time_ranges() {
        let session_id = SessionId::new("md-index-pipes").expect("valid session id");
        let prepared = PreparedSegmentMarkdown::build(
            session_id.clone(),
            &[],
            3,
            "Compared alpha | beta | gamma modes across pipeline stages.",
            Some((1_709_200_200, 1_709_205_720)),
        );
        let original_row = prepared.index_row(1);
        assert!(original_row.summary_excerpt.contains("\\|"));
        assert_eq!(
            original_row.time_range_label,
            "`2024-02-29 09:50~11:22 UTC`"
        );

        let rendered = render_index_markdown(&session_id, std::slice::from_ref(&original_row));
        let parsed = parse_index_markdown(&rendered);
        assert_eq!(parsed, vec![original_row]);
    }
}
