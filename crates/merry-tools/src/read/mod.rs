use std::{
    fs,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use merry_core::PendingToolCall;
use merry_runtime::{
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use serde::Serialize;

use crate::{
    READ_FILE_TOOL,
    config::WorkspaceToolLimits,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_NOT_FOUND, ERROR_FILE_TOO_LARGE,
        ERROR_INVALID_ARGUMENTS, ERROR_NOT_FILE, ERROR_READ_FAILED, failed_outcome,
    },
    file::read_bounded,
    path::{
        ValidatedToolPath, open_file_for_read, resolve_existing_path,
        validate_workspace_path_argument,
    },
    state::WorkspaceToolState,
    trace::{
        WorkspaceTraceFinish, WorkspaceTracePath, WorkspaceTraceTarget, invalid_arguments_outcome,
        trace_workspace_tool_finish, trace_workspace_tool_start,
    },
};

mod bounded;
mod doc;
mod docx;
mod image;
mod input;
mod notebook;
mod ooxml;
mod pdf;
mod pptx;
mod xlsx;

pub use image::WorkspaceReadImageSuccess;
#[cfg(test)]
pub(crate) use image::{
    MAX_EMBEDDED_IMAGE_DIMENSION, MAX_EMBEDDED_IMAGE_PIXELS, MAX_IMAGE_SOURCE_BYTES, png_crc32,
};
pub(crate) use input::{PdfReadFormat, ReadFileInput, spec};
pub use pdf::WorkspaceReadPdfPagesSuccess;
#[cfg(test)]
pub(crate) use pdf::parse_page_range;

const IMAGE_SNIFF_BYTES: usize = 512;
const READ_FILE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) struct ReadFileExecutor {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for ReadFileExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let args = match input::parse(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        READ_FILE_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = WorkspaceTracePath::new(args.path.as_str());
            trace_workspace_tool_start(
                READ_FILE_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
            );
            let state = Arc::clone(&self.state);
            let worker_token = context.cancellation_token().child_token();
            let _cancel_on_drop = worker_token.clone().drop_guard();
            let path_for_timeout = args.path.clone();
            let deadline = Instant::now() + READ_FILE_TIMEOUT;
            let timed_out = Arc::new(AtomicBool::new(false));
            let worker_timed_out = Arc::clone(&timed_out);
            let result = match tokio::task::spawn_blocking(move || {
                read_file_blocking_checked(&state, args, &|| {
                    if worker_token.is_cancelled() {
                        return true;
                    }
                    if Instant::now() >= deadline {
                        worker_timed_out.store(true, Ordering::Relaxed);
                        return true;
                    }
                    false
                })
            })
            .await
            {
                Ok(Err(ToolExecutionError::Cancelled)) if timed_out.load(Ordering::Relaxed) => {
                    Ok(failed_outcome(
                        READ_FILE_TOOL,
                        ERROR_READ_FAILED,
                        format!("read_file timed out after {}s", READ_FILE_TIMEOUT.as_secs()),
                        Some(path_for_timeout),
                    ))
                }
                Ok(result) => result,
                Err(error) => Err(ToolExecutionError::infrastructure(format!(
                    "read_file task failed to join: {error}"
                ))),
            };
            let finish = match &result {
                Ok(outcome) => WorkspaceTraceFinish::Outcome(outcome),
                Err(error) => WorkspaceTraceFinish::from_error(error),
            };
            trace_workspace_tool_finish(
                READ_FILE_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                finish,
            );
            result
        })
    }
}

#[derive(Debug, Serialize)]
struct ReadTextSuccess<'a> {
    ok: bool,
    tool: &'static str,
    path: &'a str,
    start_line: usize,
    end_line: Option<usize>,
    lines: usize,
    bytes: usize,
    truncated: bool,
    content: &'a str,
}

impl ReadTextSuccess<'_> {
    fn model_text(&self) -> String {
        let Some(end_line) = self.end_line else {
            return if self.start_line == 1 {
                format!("{}: empty file", self.path)
            } else {
                format!(
                    "{}: no lines returned from line {} (past end of file)",
                    self.path, self.start_line
                )
            };
        };
        let mut text = format!(
            "{}:{}-{end_line}\n{}",
            self.path, self.start_line, self.content
        );
        if self.truncated {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&format!(
                "[truncated: more file content remains; read from line {}]",
                end_line.saturating_add(1)
            ));
        }
        text
    }
}

#[cfg(test)]
pub(crate) fn read_file_blocking(
    state: &WorkspaceToolState,
    args: ReadFileInput,
) -> ToolExecutionOutcome {
    read_file_blocking_checked(state, args, &|| false)
        .expect("uncancelled file read should return an outcome")
}

fn read_file_blocking_checked(
    state: &WorkspaceToolState,
    args: ReadFileInput,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }
    let relative = match validate_workspace_path_argument(&args.path, &state.root) {
        Ok(relative) => relative,
        Err(error) => {
            return Ok(failed_outcome(
                READ_FILE_TOOL,
                error.code,
                error.message,
                error.path,
            ));
        }
    };
    let start_line = args.start_line.unwrap_or(1);
    let max_lines = args.max_lines.unwrap_or(state.limits.max_read_lines);
    if start_line == 0 || max_lines == 0 || max_lines > state.limits.max_read_lines {
        return Ok(failed_outcome(
            READ_FILE_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "read_file line range is outside the configured limit",
            Some(relative.display),
        ));
    }
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }
    let resolved = match resolve_existing_path(&relative, state.read_roots()) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            return Ok(failed_outcome(
                READ_FILE_TOOL,
                ERROR_FILE_NOT_FOUND,
                "workspace file was not found",
                Some(relative.display),
            ));
        }
        Err(error) => {
            return Ok(failed_outcome(
                READ_FILE_TOOL,
                error.code,
                error.message,
                Some(relative.display),
            ));
        }
    };
    if let Err(error) = state.validate_read_boundary(&relative, &resolved) {
        return Ok(failed_outcome(
            READ_FILE_TOOL,
            error.code,
            error.message,
            Some(relative.display),
        ));
    }
    let options = ReadFileOptions {
        start_line,
        max_lines,
        pages: args.pages.as_deref(),
        pdf_format: args.format.unwrap_or_default(),
    };
    match read_resolved_file(&relative, &resolved, options, &state.limits, is_cancelled) {
        Ok((outcome, is_text)) => {
            if is_text
                && let Some(memory) = &state.durable_memory
                && let Err(error) = memory.note_read(&resolved)
            {
                tracing::debug!(
                    error = %error,
                    path = %resolved.display(),
                    "durable memory read snapshot was not recorded"
                );
            }
            Ok(outcome)
        }
        Err(BlockingToolError::Cancelled) => Err(ToolExecutionError::Cancelled),
        Err(BlockingToolError::Domain(error)) => Ok(failed_outcome(
            READ_FILE_TOOL,
            error.code,
            error.message,
            Some(relative.display),
        )),
    }
}

struct ReadFileOptions<'a> {
    start_line: usize,
    max_lines: usize,
    pages: Option<&'a str>,
    pdf_format: PdfReadFormat,
}

fn read_resolved_file(
    relative: &ValidatedToolPath,
    path: &Path,
    options: ReadFileOptions<'_>,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(ToolExecutionOutcome, bool), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let mut file = open_file_for_read(path)?;
    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;
    if !metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }

    let sniff = sniff_leading_bytes(&mut file)?;

    if has_extension(path, "svg") && sniff.starts_with(b"\x89PNG\r\n\x1a\n") {
        let bytes = read_bounded(&mut file, limits.max_read_bytes, is_cancelled)?;
        if let Some(svg_text) = image::extract_svg_after_png_prefix(&bytes) {
            return Ok((
                slice_text_outcome(
                    relative,
                    &svg_text,
                    options.start_line,
                    options.max_lines,
                    limits,
                ),
                true,
            ));
        }
    }

    if image::detect_raster_image_format(path, &sniff).is_some() {
        let outcome = image::read_resolved_image(relative, &mut file, is_cancelled)?;
        return Ok((outcome, false));
    }

    if pdf::is_pdf_file(path, &sniff) {
        let outcome = pdf::read_resolved_pdf(
            relative,
            &mut file,
            pdf::PdfReadRequest {
                pages: options.pages,
                format: options.pdf_format,
                start_line: options.start_line,
                max_lines: options.max_lines,
            },
            limits,
            is_cancelled,
        )?;
        return Ok((outcome, false));
    }

    if has_extension(path, "pptx") {
        let outcome = pptx::read_resolved_pptx(
            relative,
            &mut file,
            options.start_line,
            options.max_lines,
            limits,
            is_cancelled,
        )?;
        return Ok((outcome, false));
    }

    if has_extension(path, "docx") {
        let outcome = docx::read_resolved_docx(
            relative,
            &mut file,
            options.start_line,
            options.max_lines,
            limits,
            is_cancelled,
        )?;
        return Ok((outcome, false));
    }

    if has_extension(path, "doc") {
        let outcome = doc::read_resolved_doc(
            relative,
            &mut file,
            options.start_line,
            options.max_lines,
            limits,
            is_cancelled,
        )?;
        return Ok((outcome, false));
    }

    if has_extension(path, "xlsx") {
        let outcome = xlsx::read_resolved_xlsx(
            relative,
            &mut file,
            options.start_line,
            options.max_lines,
            limits,
            is_cancelled,
        )?;
        return Ok((outcome, false));
    }

    if has_extension(path, "ipynb") {
        let outcome = notebook::read_resolved_notebook(
            relative,
            &mut file,
            options.start_line,
            options.max_lines,
            limits,
            is_cancelled,
        )?;
        return Ok((outcome, false));
    }

    let outcome = read_resolved_text(
        relative,
        file,
        metadata.len(),
        options.start_line,
        options.max_lines,
        limits,
        is_cancelled,
    )?;
    Ok((outcome, true))
}

pub(super) fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(expected))
}

fn sniff_leading_bytes(file: &mut fs::File) -> Result<Vec<u8>, DomainError> {
    let mut sniff = Vec::with_capacity(IMAGE_SNIFF_BYTES);
    Read::by_ref(file)
        .take(IMAGE_SNIFF_BYTES as u64)
        .read_to_end(&mut sniff)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not seek workspace file"))?;
    Ok(sniff)
}

/// Applies `start_line`, `max_lines`, and `max_read_bytes` to extracted text
/// (used by PDF text mode, PPTX slides, Jupyter notebooks, and SVG fallback).
pub(super) fn slice_text_outcome(
    relative: &ValidatedToolPath,
    full_text: &str,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
) -> ToolExecutionOutcome {
    let mut content = String::new();
    let mut selected_lines = 0usize;
    let mut current_line = 0usize;
    let mut truncated = false;

    for line in full_text.split_inclusive('\n') {
        current_line = current_line.saturating_add(1);
        if current_line < start_line {
            continue;
        }
        if selected_lines == max_lines {
            truncated = true;
            break;
        }
        if content.len().saturating_add(line.len()) > limits.max_read_bytes {
            if selected_lines == 0 {
                let mut end = limits.max_read_bytes.min(line.len());
                while end > 0 && !line.is_char_boundary(end) {
                    end -= 1;
                }
                content.push_str(&line[..end]);
                selected_lines = 1;
            }
            truncated = true;
            break;
        }
        content.push_str(line);
        selected_lines += 1;
    }

    let payload = ReadTextSuccess {
        ok: true,
        tool: READ_FILE_TOOL,
        path: &relative.display,
        start_line,
        end_line: (selected_lines > 0).then(|| start_line.saturating_add(selected_lines - 1)),
        lines: selected_lines,
        bytes: content.len(),
        truncated,
        content: &content,
    };
    ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("read_file success envelope serializes"),
    )
    .with_model_text(payload.model_text())
}

fn read_resolved_text(
    relative: &ValidatedToolPath,
    file: fs::File,
    file_bytes: u64,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let byte_limit = u64::try_from(limits.max_read_bytes)
        .map_err(|_| DomainError::new(ERROR_FILE_TOO_LARGE, "workspace read limit is too large"))?;
    let mut reader = BufReader::new(file.take(byte_limit.saturating_add(1)));
    let mut content = String::new();
    let mut scanned_bytes = 0usize;
    let mut selected_lines = 0usize;
    let mut current_line = 0usize;
    let mut truncated = false;
    loop {
        let line = bounded::read_line(
            &mut reader,
            limits.max_read_bytes - scanned_bytes,
            is_cancelled,
        )?;
        if line.is_empty() {
            break;
        }
        scanned_bytes += line.len();
        current_line = current_line.saturating_add(1);
        if current_line < start_line {
            continue;
        }
        content.push_str(&line);
        selected_lines += 1;
        if selected_lines == max_lines {
            truncated = (scanned_bytes as u64) < file_bytes;
            break;
        }
    }
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let payload = ReadTextSuccess {
        ok: true,
        tool: READ_FILE_TOOL,
        path: &relative.display,
        start_line,
        end_line: (selected_lines > 0).then(|| start_line.saturating_add(selected_lines - 1)),
        lines: selected_lines,
        bytes: content.len(),
        truncated,
        content: &content,
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("read_file success envelope serializes"),
    )
    .with_model_text(payload.model_text()))
}
