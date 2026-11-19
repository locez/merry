//! Shared captured-output fields in process artifacts and model result rendering.
//!
//! The process executor writes this envelope and presentation consumers read it.
//! Execution input and recovery metadata remain writer-owned additional fields.
//! Readers accept historical omissions and integer exit statuses, but a present
//! stream must contain text so malformed output cannot silently become empty.

use super::{ProcessExitStatus, ProcessPermissionProfileId, ProcessRunnerOutput};
use crate::PermissionAdmissionReview;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use merry_core::{ToolOutput, ToolOutputKind};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

pub(crate) const TRUNCATION_GUIDANCE: &str =
    "Output is incomplete; inspect a narrower range before drawing conclusions.";
pub(crate) const NON_UTF8_GUIDANCE: &str = "Non-UTF-8 bytes are shown as \u{FFFD}; inspect the source with xxd or a format-aware tool for exact bytes.";
pub(crate) const OUTPUT_RECOVERY_GUIDANCE: &str =
    "Do not repeat a side-effecting command to recover output.";
pub(crate) const STILL_RUNNING_GUIDANCE: &str = "The process is still running, so this turn can continue. Ending this tool call does not stop it. Use read_background_process with this id for later output and stop_background_process to stop it. Merry keeps it until the session ends or that stop.";

/// Shared fields of a full process result artifact, including lossless captures.
///
/// Serialization borrows runner text; deserialization owns it. Unknown fields
/// are ignored for artifact compatibility, not used to make admission decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutputEnvelope<'a> {
    kind: ProcessOutputKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_profile_id: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<ProcessOutputStatus>,
    #[serde(default)]
    stdout: ProcessOutputStream<'a>,
    #[serde(default)]
    stderr: ProcessOutputStream<'a>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_review: Option<ProcessOutputReview<'a>>,
}

impl<'a> ProcessOutputEnvelope<'a> {
    pub(crate) fn new(
        output: &'a ProcessRunnerOutput,
        permission_profile_id: ProcessPermissionProfileId,
        permission_review: Option<&'a PermissionAdmissionReview>,
    ) -> Self {
        Self {
            kind: ProcessOutputKind::ProcessAction,
            ok: Some(output.ok()),
            permission_profile_id: Some(permission_profile_id.as_str().into()),
            status: Some(ProcessOutputStatus::Current(output.status())),
            stdout: ProcessOutputStream::new(
                output.stdout_data(),
                output.stdout_text(),
                output.stdout_truncated(),
            ),
            stderr: ProcessOutputStream::new(
                output.stderr_data(),
                output.stderr_text(),
                output.stderr_truncated(),
            ),
            permission_review: permission_review.map(ProcessOutputReview::from),
        }
    }

    /// Returns recorded success, or `None` if a historical artifact omitted it.
    #[must_use]
    pub fn ok(&self) -> Option<bool> {
        self.ok
    }

    /// Borrows the recorded profile name without requiring it to still exist.
    #[must_use]
    pub fn permission_profile_id(&self) -> Option<&str> {
        self.permission_profile_id.as_deref()
    }

    /// Borrows the admission rationale if the artifact includes one.
    #[must_use]
    pub fn permission_rationale(&self) -> Option<&str> {
        self.permission_review.as_ref()?.rationale.as_deref()
    }

    /// Returns a normal exit code from current or historical status encoding.
    #[must_use]
    pub fn exit_code(&self) -> Option<i64> {
        match self.status.as_ref()? {
            ProcessOutputStatus::Current(status) => status.exit_code().map(i64::from),
            ProcessOutputStatus::Legacy(code) => Some(*code),
        }
    }

    /// Borrows captured stdout, empty when omitted in a historical artifact.
    #[must_use]
    pub fn stdout(&self) -> &ProcessOutputStream<'a> {
        &self.stdout
    }

    /// Borrows captured stderr, empty when omitted in a historical artifact.
    #[must_use]
    pub fn stderr(&self) -> &ProcessOutputStream<'a> {
        &self.stderr
    }

    /// Transfers both captures to a consumer without cloning their text.
    #[must_use]
    pub fn into_streams(self) -> (ProcessOutputStream<'a>, ProcessOutputStream<'a>) {
        (self.stdout, self.stderr)
    }

    fn derived_model_text(&self) -> Option<String> {
        let streams = ModelTextStreams {
            stdout: self.stdout.text(),
            stdout_truncated: self.stdout.truncated,
            stdout_utf8: self.stdout.utf8.unwrap_or(true),
            stderr: self.stderr.text(),
            stderr_truncated: self.stderr.truncated,
            stderr_utf8: self.stderr.utf8.unwrap_or(true),
        };
        let status = match self.status.as_ref()? {
            ProcessOutputStatus::Current(status) => *status,
            ProcessOutputStatus::Legacy(code) => {
                let mut text = format!("exit {code}");
                append_model_stream(
                    &mut text,
                    "stdout",
                    streams.stdout,
                    streams.stdout_truncated,
                    streams.stdout_utf8,
                );
                append_model_stream(
                    &mut text,
                    "stderr",
                    streams.stderr,
                    streams.stderr_truncated,
                    streams.stderr_utf8,
                );
                return Some(text);
            }
        };
        let header = match status {
            ProcessExitStatus::Exited(code) => format!("exit {code}"),
            ProcessExitStatus::Cancelled => "cancelled".to_owned(),
            ProcessExitStatus::FailedToStart => "not started".to_owned(),
            ProcessExitStatus::DomainFailed => "failed before normal exit".to_owned(),
            ProcessExitStatus::Running => return None,
        };
        Some(render_model_text_parts(
            header,
            streams,
            recovery_guidance(status),
        ))
    }
}

/// Renders the compact model-facing text view from a live process runner output.
#[must_use]
pub(crate) fn render_process_model_text(output: &ProcessRunnerOutput) -> String {
    let header = match output.status() {
        ProcessExitStatus::Exited(code) => format!("exit {code}"),
        ProcessExitStatus::Cancelled => "cancelled".to_owned(),
        ProcessExitStatus::FailedToStart => "not started".to_owned(),
        ProcessExitStatus::DomainFailed => "failed before normal exit".to_owned(),
        ProcessExitStatus::Running => {
            match (output.running_pid(), output.background_process_id()) {
                (Some(pid), Some(id)) => format!("still running pid {pid} id {id}"),
                (Some(pid), None) => format!("still running pid {pid}"),
                (None, Some(id)) => format!("still running id {id}"),
                (None, None) => "still running".to_owned(),
            }
        }
    };
    render_model_text_parts(
        header,
        ModelTextStreams {
            stdout: output.stdout_text(),
            stdout_truncated: output.stdout_truncated(),
            stdout_utf8: output.stdout_is_utf8(),
            stderr: output.stderr_text(),
            stderr_truncated: output.stderr_truncated(),
            stderr_utf8: output.stderr_is_utf8(),
        },
        recovery_guidance(output.status()),
    )
}

/// Rebuilds the process model text from a stored full output JSON document.
///
/// A still-running process names a pid/id that the envelope does not store, so
/// this returns `None` and the journal retains the model text for that case.
#[must_use]
pub(crate) fn derived_process_model_text(full_json: &str) -> Option<String> {
    let envelope = serde_json::from_str::<ProcessOutputEnvelope<'_>>(full_json).ok()?;
    envelope.derived_model_text()
}

/// Strips non-durable admission and shell-fingerprint metadata before writing a
/// process tool output into `journal.jsonl`.
///
/// Non-process outputs are returned unchanged.
#[must_use]
pub(crate) fn sanitize_process_output_for_journal(full: &ToolOutput) -> ToolOutput {
    if full.kind() != ToolOutputKind::Json {
        return full.clone();
    }
    if serde_json::from_str::<ProcessOutputEnvelope<'_>>(full.as_str()).is_err() {
        return full.clone();
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(full.as_str()) else {
        return full.clone();
    };
    let Some(object) = value.as_object_mut() else {
        return full.clone();
    };
    let removed_profile = object.remove("permission_profile_id").is_some();
    let removed_review = object.remove("permission_review").is_some();
    let removed_evidence = object.remove("input_evidence").is_some();
    if !removed_profile && !removed_review && !removed_evidence {
        return full.clone();
    }
    serde_json::to_string(&value)
        .map(ToolOutput::json)
        .unwrap_or_else(|_| full.clone())
}

pub(crate) fn recovery_guidance(status: ProcessExitStatus) -> Option<&'static str> {
    match status {
        ProcessExitStatus::Cancelled | ProcessExitStatus::DomainFailed => {
            Some("Execution may have had side effects; verify its state before retrying.")
        }
        ProcessExitStatus::FailedToStart => {
            Some("The command did not start; check the error before retrying.")
        }
        ProcessExitStatus::Running => Some(STILL_RUNNING_GUIDANCE),
        ProcessExitStatus::Exited(_) => None,
    }
}

struct ModelTextStreams<'a> {
    stdout: &'a str,
    stdout_truncated: bool,
    stdout_utf8: bool,
    stderr: &'a str,
    stderr_truncated: bool,
    stderr_utf8: bool,
}

fn render_model_text_parts(
    mut text: String,
    streams: ModelTextStreams<'_>,
    recovery: Option<&'static str>,
) -> String {
    append_model_stream(
        &mut text,
        "stdout",
        streams.stdout,
        streams.stdout_truncated,
        streams.stdout_utf8,
    );
    append_model_stream(
        &mut text,
        "stderr",
        streams.stderr,
        streams.stderr_truncated,
        streams.stderr_utf8,
    );
    let mut guidance = Vec::with_capacity(4);
    let non_utf8 = !streams.stdout_utf8 || !streams.stderr_utf8;
    let truncated = streams.stdout_truncated || streams.stderr_truncated;
    if non_utf8 {
        guidance.push(NON_UTF8_GUIDANCE);
    }
    if truncated {
        guidance.push(TRUNCATION_GUIDANCE);
    }
    if non_utf8 || truncated {
        guidance.push(OUTPUT_RECOVERY_GUIDANCE);
    }
    if let Some(recovery) = recovery {
        guidance.push(recovery);
    }
    if !guidance.is_empty() {
        append_model_section(&mut text, "guidance", &guidance.join(" "));
    }
    text
}

fn append_model_stream(text: &mut String, name: &str, content: &str, truncated: bool, utf8: bool) {
    if content.is_empty() && !truncated {
        return;
    }
    match (truncated, utf8) {
        (false, true) => append_model_section(text, name, content),
        (true, true) => append_model_section(text, &format!("{name} (truncated)"), content),
        (false, false) => append_model_section(text, &format!("{name} (non-UTF-8)"), content),
        (true, false) => {
            append_model_section(text, &format!("{name} (truncated, non-UTF-8)"), content);
        }
    }
}

fn append_model_section(text: &mut String, name: &str, content: &str) {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(name);
    text.push_str(":\n");
    text.push_str(content);
}

/// One captured stream, with optional metadata absent in older artifacts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutputStream<'a> {
    text: Cow<'a, str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
    #[serde(default)]
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    utf8: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes_base64: Option<String>,
}

impl<'a> ProcessOutputStream<'a> {
    fn new(bytes: &[u8], text: &'a str, truncated: bool) -> Self {
        let utf8 = std::str::from_utf8(bytes).is_ok();
        Self {
            text: Cow::Borrowed(text),
            bytes: Some(bytes.len()),
            truncated,
            utf8: Some(utf8),
            bytes_base64: (!utf8).then(|| BASE64.encode(bytes)),
        }
    }

    /// Borrows captured text, lossily decoded when `utf8()` is `Some(false)`.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns whether the capture limit discarded output bytes.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns encoding validity, or `None` if it was not recorded.
    #[must_use]
    pub fn utf8(&self) -> Option<bool> {
        self.utf8
    }

    /// Takes decoded text, allocating only when this envelope borrowed it.
    #[must_use]
    pub fn into_text(self) -> String {
        self.text.into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessOutputKind {
    ProcessAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum ProcessOutputStatus {
    Current(#[serde(with = "ProcessExitStatusWire")] ProcessExitStatus),
    Legacy(i64),
}

#[derive(Serialize, Deserialize)]
#[serde(
    remote = "ProcessExitStatus",
    tag = "kind",
    content = "code",
    rename_all = "snake_case"
)]
enum ProcessExitStatusWire {
    Exited(i32),
    Cancelled,
    FailedToStart,
    DomainFailed,
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProcessOutputReview<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    risk: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_authorization: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rationale: Option<Cow<'a, str>>,
}

impl<'a> From<&'a PermissionAdmissionReview> for ProcessOutputReview<'a> {
    fn from(review: &'a PermissionAdmissionReview) -> Self {
        Self {
            source: Some(review.source().as_str().into()),
            risk: Some(review.risk().as_str().into()),
            user_authorization: Some(review.user_authorization().as_str().into()),
            rationale: Some(review.rationale().into()),
        }
    }
}
