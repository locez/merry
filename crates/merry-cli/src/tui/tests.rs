use crate::tui::input::{DraftImage, TuiSubmission};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, PendingToolCall, PendingToolCallBatch,
    RuntimeEventSource, SessionId, ToolCallArguments, ToolCallBatchId, ToolCallId, ToolName,
};
use ratatui::style::Color;

fn text_submission(text: &str) -> TuiSubmission {
    TuiSubmission {
        text: text.to_owned(),
        history_text: text.to_owned(),
        images: Vec::new(),
    }
}

fn draft_image(marker: u8) -> DraftImage {
    DraftImage::new([137, 80, 78, 71, 13, 10, 26, 10, marker], 2, 3).expect("valid draft image")
}

fn source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("tui-test").unwrap(), 1)
}

fn text_artifact(id: &str) -> ArtifactRef {
    ArtifactRef::new(ArtifactId::new(id).unwrap(), ArtifactKind::Text)
}

fn pending_call(id: &str, tool_name: &str) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).unwrap(),
        ToolName::new(tool_name).unwrap(),
        ToolCallArguments::new(Default::default()),
    )
}

fn pending_call_with_args(
    id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).unwrap(),
        ToolName::new(tool_name).unwrap(),
        ToolCallArguments::try_from(arguments).unwrap(),
    )
}

fn pending_batch(id: &str, calls: Vec<PendingToolCall>) -> PendingToolCallBatch {
    PendingToolCallBatch::new(ToolCallBatchId::new(id).unwrap(), calls).unwrap()
}

fn find_cell_color(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<Color> {
    find_cell_style(buffer, text).and_then(|style| style.fg)
}

fn rendered_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn find_text_position(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
    let area = buffer.area;
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if let Some(byte_index) = row.find(needle) {
            let x = row[..byte_index].chars().count();
            return Some((u16::try_from(x).ok()?, y));
        }
    }
    None
}

fn find_cell_style(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<ratatui::style::Style> {
    let area = buffer.area;
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        let Some(start) = row.find(text) else {
            continue;
        };
        let x = area.x + u16::try_from(row[..start].chars().count()).ok()?;
        return Some(buffer[(x, y)].style());
    }
    None
}

mod command_palette;

mod command_details;
mod command_display;

mod command_runtime;
mod copy_interactions;

mod composer;

mod event_projection;

mod input_controls;

mod layout;

mod patch_projection;

mod process_projection;

mod provider_interactions;

mod settings;

mod status_usage;

mod submission;

mod text_rendering;

mod reading_position;
mod timeline_navigation;

mod tool_projection;
