//! Bounded transcript given to memory extraction.
//!
//! Reasoning and images are omitted. Long tool arguments, tool outputs, and
//! user text are truncated. When the JSON still exceeds the budget, middle
//! items are replaced by `[...` / `...]` markers.

use crate::session::{TranscriptItemSnapshot, UserInputOrigin};
use merry_core::PendingToolCall;

use super::manifest::truncate_utf8;

const TRANSCRIPT_BUDGET: usize = 48 * 1024;
const USER_TEXT_MAX: usize = 8 * 1024;
const ASSISTANT_TEXT_MAX: usize = 8 * 1024;
const TOOL_ARGS_MAX: usize = 2 * 1024;
const TOOL_OUTPUT_MAX: usize = 4 * 1024;
const EDGE_ITEMS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CondensedTranscript {
    pub(crate) json: String,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CondensationStats {
    images_dropped: bool,
    tool_results_trimmed: bool,
    tool_args_trimmed: bool,
    steps_omitted: bool,
    user_text_trimmed: bool,
}

impl CondensationStats {
    fn prompt_note(self) -> Option<String> {
        if !self.images_dropped
            && !self.tool_results_trimmed
            && !self.tool_args_trimmed
            && !self.steps_omitted
            && !self.user_text_trimmed
        {
            return None;
        }
        let mut parts = Vec::new();
        if self.steps_omitted {
            parts.push(
                "The transcript was condensed to fit the extraction budget. Text between \"[...\" and \"...]\" markers was removed, not written by a participant.".to_owned(),
            );
        }
        if self.images_dropped {
            parts.push("Images were omitted.".to_owned());
        }
        if self.tool_results_trimmed {
            parts.push("Tool outputs were truncated.".to_owned());
        }
        if self.tool_args_trimmed {
            parts.push("Tool arguments were truncated.".to_owned());
        }
        if self.user_text_trimmed {
            parts.push("User text was truncated.".to_owned());
        }
        parts.push("Model reasoning was omitted.".to_owned());
        Some(parts.join(" "))
    }
}

pub(crate) fn condense_turn_range(
    items: &[TranscriptItemSnapshot],
    from_turn: u32,
    through_turn: u32,
) -> Result<CondensedTranscript, String> {
    let selected = select_turn_range(items, from_turn, through_turn)?;
    let mut stats = CondensationStats::default();
    let mut rendered = Vec::with_capacity(selected.len());
    for item in selected {
        rendered.push(render_item(item, &mut stats));
    }
    if json_len(&rendered) > TRANSCRIPT_BUDGET && rendered.len() > EDGE_ITEMS * 2 {
        let omitted = rendered.len() - EDGE_ITEMS * 2;
        let mut kept = rendered[..EDGE_ITEMS].to_vec();
        kept.push(serde_json::json!({
            "role": "omitted",
            "text": format!("[...\n{omitted} intermediate steps omitted\n...]")
        }));
        kept.extend(rendered[rendered.len() - EDGE_ITEMS..].iter().cloned());
        rendered = kept;
        stats.steps_omitted = true;
    }
    while json_len(&rendered) > TRANSCRIPT_BUDGET && rendered.len() > 1 {
        rendered.pop();
        stats.steps_omitted = true;
    }
    let json = serde_json::to_string(&rendered)
        .map_err(|error| format!("memory transcript could not be encoded: {error}"))?;
    Ok(CondensedTranscript {
        note: stats.prompt_note(),
        json,
    })
}

pub(crate) fn count_external_turns(items: &[TranscriptItemSnapshot]) -> u32 {
    items
        .iter()
        .filter(|item| {
            matches!(
                item,
                TranscriptItemSnapshot::UserMessage {
                    origin: UserInputOrigin::ExternalUser,
                    ..
                }
            )
        })
        .count() as u32
}

fn select_turn_range(
    items: &[TranscriptItemSnapshot],
    from_turn: u32,
    through_turn: u32,
) -> Result<Vec<&TranscriptItemSnapshot>, String> {
    if from_turn == 0 || through_turn < from_turn {
        return Err("memory capture range is empty".to_owned());
    }
    let mut external = 0u32;
    let mut started = false;
    let mut selected = Vec::new();
    for item in items {
        if let TranscriptItemSnapshot::UserMessage {
            origin: UserInputOrigin::ExternalUser,
            ..
        } = item
        {
            external += 1;
            if external > through_turn && started {
                break;
            }
            if external == from_turn {
                started = true;
            }
        }
        if started {
            selected.push(item);
        }
    }
    if !started {
        return Err("durable transcript does not contain the uncaptured user turn".to_owned());
    }
    Ok(selected)
}

fn render_item(item: &TranscriptItemSnapshot, stats: &mut CondensationStats) -> serde_json::Value {
    match item {
        TranscriptItemSnapshot::UserMessage { text, images, .. } => {
            if !images.is_empty() {
                stats.images_dropped = true;
            }
            let (text, trimmed) = bounded(text, USER_TEXT_MAX);
            stats.user_text_trimmed |= trimmed;
            serde_json::json!({"role": "user", "text": text})
        }
        TranscriptItemSnapshot::AssistantText { text } => {
            let (text, _) = bounded(text, ASSISTANT_TEXT_MAX);
            serde_json::json!({"role": "assistant", "text": text})
        }
        TranscriptItemSnapshot::ToolCall { call } => {
            let (arguments, trimmed) = tool_arguments(call);
            stats.tool_args_trimmed |= trimmed;
            serde_json::json!({
                "role": "tool_call",
                "name": call.name().to_string(),
                "arguments": arguments
            })
        }
        TranscriptItemSnapshot::ToolResult { output, images, .. } => {
            if !images.is_empty() {
                stats.images_dropped = true;
            }
            let (text, trimmed) = bounded(output.as_str(), TOOL_OUTPUT_MAX);
            stats.tool_results_trimmed |= trimmed;
            serde_json::json!({"role": "tool_result", "text": text})
        }
    }
}

fn tool_arguments(call: &PendingToolCall) -> (String, bool) {
    let rendered =
        serde_json::to_string(call.arguments().as_object()).unwrap_or_else(|_| "{}".to_owned());
    bounded(&rendered, TOOL_ARGS_MAX)
}

fn bounded(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    (truncate_utf8(text, max_bytes).to_owned(), true)
}

fn json_len(items: &[serde_json::Value]) -> usize {
    serde_json::to_string(items)
        .map(|json| json.len())
        .unwrap_or(usize::MAX)
}
