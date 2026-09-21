//! Shared captured-output fields in process artifacts, not model result formatting.
//!
//! The process executor writes this envelope and presentation consumers read it.
//! Execution input and recovery metadata remain writer-owned additional fields.
//! Readers accept historical omissions and integer exit statuses, but a present
//! stream must contain text so malformed output cannot silently become empty.

use super::{ProcessExitStatus, ProcessPermissionProfileId, ProcessRunnerOutput};
use crate::PermissionAdmissionReview;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

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
