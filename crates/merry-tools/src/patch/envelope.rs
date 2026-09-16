//! Provider-visible result envelope of a successful `apply_patch` call.
//!
//! These types are the single definition of that payload: the tool serializes
//! them and every consumer deserializes them, so a field cannot drift between
//! the writer and a reader in another crate. Fields added after the first
//! release stay optional with `serde(default)` so a resumed session can still
//! read an envelope recorded by an older build.

use serde::{Deserialize, Serialize};

/// Successful `apply_patch` result with one entry per changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePatchSuccess {
    pub ok: bool,
    pub tool: String,
    pub changes: Vec<WorkspacePatchSuccessChange>,
}

/// One file change inside a successful `apply_patch` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePatchSuccessChange {
    /// Workspace-relative path using `/` separators.
    pub path: String,
    /// File operation, absent in envelopes recorded before it was reported,
    /// which only ever described updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<WorkspacePatchOperationKind>,
    pub hunks: usize,
    /// Line counts, absent in envelopes recorded before line counts existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_before: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_after: Option<usize>,
    pub bytes_before: usize,
    pub bytes_after: usize,
    /// Context-only hunks that were dropped from this file's update sections.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ignored_context_hunks: usize,
    /// Hunk lines of the change, empty for an add or delete.
    #[serde(default)]
    pub lines: Vec<WorkspacePatchSuccessLine>,
}

impl WorkspacePatchSuccessChange {
    /// Resolves the file operation.
    ///
    /// An envelope recorded before the operation was reported only ever
    /// described updates, so an absent operation is an update.
    #[must_use]
    pub fn operation(&self) -> WorkspacePatchOperationKind {
        self.op.unwrap_or(WorkspacePatchOperationKind::Update)
    }
}

/// File operation that produced a successful change entry.
///
/// A delete and an update that empties a file both end at zero bytes, so the
/// envelope names the operation instead of leaving callers to infer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePatchOperationKind {
    Add,
    Update,
    Delete,
    /// An operation this build does not know.
    ///
    /// The tool never writes this variant, but a session recorded by a newer
    /// build must still be readable, so an unknown operation is preserved
    /// instead of failing the whole result.
    #[serde(other)]
    Unknown,
}

/// One line of a change entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePatchSuccessLine {
    pub kind: WorkspacePatchSuccessLineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<usize>,
    pub text: String,
}

/// Kind of one line inside a change entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePatchSuccessLineKind {
    Context,
    Remove,
    Add,
    /// A kind this build does not know.
    ///
    /// The serialized envelope is provider-visible output that a newer runtime
    /// may extend, so an unknown kind is preserved as a line the reader ignores
    /// instead of failing the whole result.
    #[serde(other)]
    Unknown,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::{
        WorkspacePatchOperationKind, WorkspacePatchSuccess, WorkspacePatchSuccessChange,
        WorkspacePatchSuccessLine, WorkspacePatchSuccessLineKind,
    };

    #[test]
    fn success_envelope_round_trips_every_field() {
        let envelope = WorkspacePatchSuccess {
            ok: true,
            tool: "apply_patch".to_owned(),
            changes: vec![WorkspacePatchSuccessChange {
                path: "dir/note.txt".to_owned(),
                op: Some(WorkspacePatchOperationKind::Update),
                hunks: 1,
                lines_before: Some(3),
                lines_after: Some(3),
                bytes_before: 22,
                bytes_after: 24,
                ignored_context_hunks: 2,
                lines: vec![WorkspacePatchSuccessLine {
                    kind: WorkspacePatchSuccessLineKind::Add,
                    old_line: None,
                    new_line: Some(2),
                    text: "newer".to_owned(),
                }],
            }],
        };

        let json = serde_json::to_string(&envelope).expect("envelope should serialize");
        let restored: WorkspacePatchSuccess =
            serde_json::from_str(&json).expect("serialized envelope should deserialize");

        assert_eq!(restored, envelope);
        assert_eq!(
            restored.changes[0].operation(),
            WorkspacePatchOperationKind::Update
        );
    }

    #[test]
    fn success_envelope_reads_a_legacy_entry_as_an_update() {
        // Shape recorded before the operation and line counts were reported.
        let json = r#"{"ok":true,"tool":"apply_patch","changes":[
            {"path":"hello.txt","hunks":1,"bytes_before":0,"bytes_after":12}
        ]}"#;

        let restored: WorkspacePatchSuccess =
            serde_json::from_str(json).expect("legacy envelope should deserialize");

        let change = &restored.changes[0];
        assert_eq!(change.operation(), WorkspacePatchOperationKind::Update);
        assert_eq!(change.lines_before, None);
        assert_eq!(change.lines_after, None);
        assert!(change.lines.is_empty());
    }

    #[test]
    fn success_envelope_keeps_an_unknown_line_kind_readable() {
        let json = r#"{"ok":true,"tool":"apply_patch","changes":[{"path":"note.txt","hunks":1,"bytes_before":1,"bytes_after":2,
            "lines":[{"kind":"something-new","text":"line"}]}]}"#;

        let restored: WorkspacePatchSuccess =
            serde_json::from_str(json).expect("future line kinds should not fail the result");

        assert_eq!(
            restored.changes[0].lines[0].kind,
            WorkspacePatchSuccessLineKind::Unknown
        );
    }

    #[test]
    fn success_envelope_keeps_an_unknown_operation_readable() {
        let json = r#"{"ok":true,"tool":"apply_patch","changes":[{"path":"note.txt","op":"something-new","hunks":1,"bytes_before":1,"bytes_after":2}]}"#;

        let restored: WorkspacePatchSuccess =
            serde_json::from_str(json).expect("future operations should not fail the result");

        assert_eq!(
            restored.changes[0].operation(),
            WorkspacePatchOperationKind::Unknown
        );
    }

    #[test]
    fn success_envelope_omits_absent_optional_fields_when_serializing() {
        let envelope = WorkspacePatchSuccess {
            ok: true,
            tool: "apply_patch".to_owned(),
            changes: vec![WorkspacePatchSuccessChange {
                path: "gone.txt".to_owned(),
                op: Some(WorkspacePatchOperationKind::Delete),
                hunks: 0,
                lines_before: Some(4),
                lines_after: Some(0),
                bytes_before: 30,
                bytes_after: 0,
                ignored_context_hunks: 0,
                lines: Vec::new(),
            }],
        };

        let value = serde_json::to_value(&envelope).expect("envelope should serialize");

        assert_eq!(
            value,
            serde_json::json!({
                "ok": true,
                "tool": "apply_patch",
                "changes": [{
                    "path": "gone.txt",
                    "op": "delete",
                    "hunks": 0,
                    "lines_before": 4,
                    "lines_after": 0,
                    "bytes_before": 30,
                    "bytes_after": 0,
                    "lines": []
                }]
            }),
            "the wire shape must stay stable for existing consumers"
        );
    }
}
