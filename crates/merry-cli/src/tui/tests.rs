use crate::tui::input::{DraftImage, TuiSubmission};
use merry::events::{
    PendingToolCall, PendingToolCallBatch, RuntimeEventSource, SessionId, ToolCallArguments,
    ToolCallBatchId, ToolCallId, ToolName,
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
        let mut cell_offsets = Vec::with_capacity(usize::from(area.width));
        for x in area.x..area.x + area.width {
            let start = row.len();
            row.push_str(buffer[(x, y)].symbol());
            cell_offsets.push((start, x));
        }
        if let Some(byte_index) = row.find(needle)
            && let Some(&(_, x)) = cell_offsets
                .iter()
                .rev()
                .find(|&&(start, _)| start <= byte_index)
        {
            return Some((x, y));
        }
    }
    None
}

fn find_cell_style(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<ratatui::style::Style> {
    let (x, y) = find_text_position(buffer, text)?;
    Some(buffer[(x, y)].style())
}

mod click_targets;
mod command_palette;
mod comments;

mod command_details;
mod command_display;

mod command_runtime;
mod copy_interactions;

mod composer;

mod event_projection;

mod input_controls;

mod layout;
mod link_and_table_rendering;

mod output_preview;
mod output_rate;

mod patch_projection;

mod process_projection;

mod read_output;

mod provider_interactions;

mod settings;
mod settings_permissions;
mod settings_workbench;

mod status_usage;
mod sticky_user;

mod submission;

mod text_rendering;

mod thinking_display;

mod reading_position;
mod timeline_navigation;

mod tool_projection;
