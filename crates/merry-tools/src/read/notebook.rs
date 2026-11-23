//! Jupyter Notebook (`.ipynb`) cell, output, and embedded-visualization reader.

use std::{fmt::Write as _, fs};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use merry_runtime::{
    MAX_USER_IMAGE_TOTAL_PNG_BYTES, MAX_USER_IMAGES, ToolExecutionOutcome, UserImageInput,
};
use serde::{Deserialize, de::IgnoredAny};

use crate::{
    config::WorkspaceToolLimits,
    errors::{BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_INVALID_DOCUMENT},
    file::read_bounded,
    path::ValidatedToolPath,
    read::{
        image::{
            MAX_EMBEDDED_IMAGE_DIMENSION, MAX_EMBEDDED_IMAGE_PIXELS, MAX_EMBEDDED_IMAGE_PNG_BYTES,
            normalize_encoded_image,
        },
        slice_text_outcome,
    },
};

/// Maximum `.ipynb` file size accepted by `read_file` (20 MiB).
const MAX_NOTEBOOK_BYTES: usize = 20 * 1024 * 1024;
/// Maximum raw PNG size for one embedded notebook output image (2 MiB).
const MAX_NOTEBOOK_IMAGE_PNG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct NotebookDocument {
    cells: Vec<NotebookCell>,
}

#[derive(Deserialize)]
struct NotebookCell {
    #[serde(default = "default_cell_type")]
    cell_type: String,
    #[serde(default)]
    execution_count: Option<i64>,
    #[serde(default)]
    source: JsonText,
    #[serde(default)]
    outputs: Vec<NotebookOutput>,
}

fn default_cell_type() -> String {
    "unknown".to_owned()
}

#[derive(Deserialize)]
#[serde(tag = "output_type", rename_all = "snake_case")]
enum NotebookOutput {
    Stream {
        #[serde(default = "default_stream_name")]
        name: String,
        #[serde(default)]
        text: JsonText,
    },
    Error {
        #[serde(default = "default_error_name")]
        ename: String,
        #[serde(default)]
        evalue: String,
        #[serde(default)]
        traceback: Vec<String>,
    },
    ExecuteResult {
        #[serde(default)]
        data: OutputBundle,
    },
    DisplayData {
        #[serde(default)]
        data: OutputBundle,
    },
    #[serde(other)]
    Other,
}

fn default_stream_name() -> String {
    "stdout".to_owned()
}

fn default_error_name() -> String {
    "Error".to_owned()
}

#[derive(Default, Deserialize)]
struct OutputBundle {
    #[serde(rename = "text/plain", default)]
    text_plain: Option<JsonText>,
    #[serde(rename = "image/png", default)]
    image_png: Option<JsonText>,
    #[serde(rename = "image/jpeg", default)]
    image_jpeg: Option<JsonText>,
    #[serde(rename = "image/webp", default)]
    image_webp: Option<JsonText>,
    #[serde(rename = "image/gif", default)]
    image_gif: Option<JsonText>,
}

impl OutputBundle {
    fn first_image_base64(&self) -> Option<&JsonText> {
        self.image_png
            .as_ref()
            .or(self.image_jpeg.as_ref())
            .or(self.image_webp.as_ref())
            .or(self.image_gif.as_ref())
    }
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum JsonText {
    Single(String),
    Lines(Vec<String>),
    #[default]
    Empty,
    Other(IgnoredAny),
}

impl JsonText {
    fn into_joined(self) -> String {
        match self {
            Self::Single(s) => s,
            Self::Lines(lines) => lines.concat(),
            Self::Empty | Self::Other(IgnoredAny) => String::new(),
        }
    }

    fn as_joined(&self) -> String {
        match self {
            Self::Single(s) => s.clone(),
            Self::Lines(lines) => lines.concat(),
            Self::Empty | Self::Other(IgnoredAny) => String::new(),
        }
    }
}

pub(super) fn read_resolved_notebook(
    relative: &ValidatedToolPath,
    file: &mut fs::File,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let bytes =
        read_bounded(file, MAX_NOTEBOOK_BYTES, is_cancelled).map_err(|error| match error {
            BlockingToolError::Domain(domain) if domain.code == ERROR_FILE_TOO_LARGE => {
                BlockingToolError::Domain(DomainError::new(
                    ERROR_FILE_TOO_LARGE,
                    "notebook file exceeds the 20 MiB limit",
                ))
            }
            other => other,
        })?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let doc: NotebookDocument = serde_json::from_slice(&bytes).map_err(|error| {
        DomainError::new(
            ERROR_INVALID_DOCUMENT,
            format!("could not parse Jupyter notebook JSON: {error}"),
        )
    })?;

    let mut state = NotebookRenderState {
        relative,
        rendered: String::new(),
        images: Vec::new(),
        total_png_bytes: 0,
    };

    for (idx, cell) in doc.cells.into_iter().enumerate() {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        let cell_num = idx + 1;
        if idx > 0 {
            state.rendered.push('\n');
        }
        match cell.execution_count {
            Some(count) => {
                let _ = writeln!(
                    &mut state.rendered,
                    "--- Cell {cell_num} [{}, execution_count={count}] ---",
                    cell.cell_type
                );
            }
            None => {
                let _ = writeln!(
                    &mut state.rendered,
                    "--- Cell {cell_num} [{}] ---",
                    cell.cell_type
                );
            }
        }

        let source = cell.source.into_joined();
        if !source.is_empty() {
            state.rendered.push_str(&source);
            if !state.rendered.ends_with('\n') {
                state.rendered.push('\n');
            }
        }

        let mut cell_img_idx = 0usize;
        for output in cell.outputs {
            state.render_cell_output(output, cell_num, &mut cell_img_idx, is_cancelled)?;
        }
    }

    let mut outcome = slice_text_outcome(relative, &state.rendered, start_line, max_lines, limits);
    if !state.images.is_empty() {
        outcome = outcome.with_images(state.images);
    }
    Ok(outcome)
}

struct NotebookRenderState<'a> {
    relative: &'a ValidatedToolPath,
    rendered: String,
    images: Vec<UserImageInput>,
    total_png_bytes: usize,
}

impl NotebookRenderState<'_> {
    fn render_cell_output(
        &mut self,
        output: NotebookOutput,
        cell_num: usize,
        cell_img_idx: &mut usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), BlockingToolError> {
        match output {
            NotebookOutput::Stream { name, text } => {
                let text = text.into_joined();
                if !text.is_empty() {
                    let _ = writeln!(self.rendered, "[output:{name}]");
                    self.rendered.push_str(&text);
                    if !self.rendered.ends_with('\n') {
                        self.rendered.push('\n');
                    }
                }
            }
            NotebookOutput::Error {
                ename,
                evalue,
                traceback,
            } => {
                let _ = writeln!(self.rendered, "[output:error] {ename}: {evalue}");
                for line in traceback {
                    let _ = writeln!(self.rendered, "{}", strip_ansi_sequences(&line));
                }
            }
            NotebookOutput::ExecuteResult { data } => {
                self.render_bundle(
                    "execute_result",
                    &data,
                    cell_num,
                    cell_img_idx,
                    is_cancelled,
                )?;
            }
            NotebookOutput::DisplayData { data } => {
                self.render_bundle("display_data", &data, cell_num, cell_img_idx, is_cancelled)?;
            }
            NotebookOutput::Other => {}
        }
        Ok(())
    }

    fn render_bundle(
        &mut self,
        output_type: &str,
        data: &OutputBundle,
        cell_num: usize,
        cell_img_idx: &mut usize,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), BlockingToolError> {
        if let Some(plain) = &data.text_plain {
            let text = plain.as_joined();
            if !text.is_empty() {
                let _ = writeln!(self.rendered, "[output:{output_type}]");
                self.rendered.push_str(&text);
                if !self.rendered.ends_with('\n') {
                    self.rendered.push('\n');
                }
            }
        }

        let Some(raw_image) = data.first_image_base64() else {
            return Ok(());
        };
        if self.images.len() >= MAX_USER_IMAGES {
            let _ = writeln!(
                self.rendered,
                "[output:image omitted: reached {MAX_USER_IMAGES}-image attachment limit]"
            );
            return Ok(());
        }

        let raw_b64 = raw_image.as_joined();
        let cleaned: String = raw_b64.chars().filter(|ch| !ch.is_whitespace()).collect();
        let Ok(decoded_bytes) = STANDARD.decode(&cleaned) else {
            return Ok(());
        };
        let max_bytes = MAX_NOTEBOOK_IMAGE_PNG_BYTES.min(MAX_EMBEDDED_IMAGE_PNG_BYTES);
        match normalize_encoded_image(
            decoded_bytes,
            max_bytes,
            MAX_EMBEDDED_IMAGE_DIMENSION,
            MAX_EMBEDDED_IMAGE_PIXELS,
            is_cancelled,
        ) {
            Ok(normalized) => {
                if self
                    .total_png_bytes
                    .saturating_add(normalized.png_bytes.len())
                    > MAX_USER_IMAGE_TOTAL_PNG_BYTES
                {
                    let _ = writeln!(
                        self.rendered,
                        "[output:image omitted: combined PNG attachment limit exceeded]"
                    );
                    return Ok(());
                }
                *cell_img_idx += 1;
                let label = format!(
                    "{}#cell={cell_num}-img={cell_img_idx}",
                    self.relative.display
                );
                if let Ok(input) = UserImageInput::png(
                    &label,
                    normalized.png_bytes,
                    normalized.width,
                    normalized.height,
                ) {
                    self.total_png_bytes =
                        self.total_png_bytes.saturating_add(input.png_bytes().len());
                    let _ = writeln!(
                        self.rendered,
                        "[output:image attached: {label} ({}x{})]",
                        input.width(),
                        input.height()
                    );
                    self.images.push(input);
                }
            }
            Err(BlockingToolError::Cancelled) => return Err(BlockingToolError::Cancelled),
            Err(BlockingToolError::Domain(_)) => {
                let _ = writeln!(
                    self.rendered,
                    "[output:image skipped: invalid or oversized]"
                );
            }
        }
        Ok(())
    }
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}
