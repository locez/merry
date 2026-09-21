use super::intent;
use crate::{
    ProcessExitStatus, ProcessOutputEnvelope, ProcessPermissionProfileId, ProcessRunnerOutput,
};
use serde_json::json;

#[test]
fn process_envelope_preserves_the_artifact_wire_contract() {
    for (status, wire_status) in [
        (
            ProcessExitStatus::Exited(0),
            json!({"kind": "exited", "code": 0}),
        ),
        (
            ProcessExitStatus::Exited(2),
            json!({"kind": "exited", "code": 2}),
        ),
        (ProcessExitStatus::Cancelled, json!({"kind": "cancelled"})),
        (
            ProcessExitStatus::FailedToStart,
            json!({"kind": "failed_to_start"}),
        ),
        (
            ProcessExitStatus::DomainFailed,
            json!({"kind": "domain_failed"}),
        ),
    ] {
        let output = ProcessRunnerOutput::from_bytes(
            &intent(),
            status,
            b"\0\xffA".to_vec(),
            true,
            b"  error\r\n".to_vec(),
            false,
        )
        .expect("captured output");
        let envelope =
            ProcessOutputEnvelope::new(&output, ProcessPermissionProfileId::READ_ONLY, None);
        let value = serde_json::to_value(envelope).expect("serialize envelope");
        assert_eq!(
            value,
            json!({
                "kind": "process_action",
                "ok": status == ProcessExitStatus::Exited(0),
                "permission_profile_id": "process.read_only",
                "status": wire_status,
                "stdout": {"text": "\0�A", "bytes": 3, "truncated": true, "utf8": false, "bytes_base64": "AP9B"},
                "stderr": {"text": "  error\r\n", "bytes": 9, "truncated": false, "utf8": true},
            })
        );
        let decoded: ProcessOutputEnvelope<'_> =
            serde_json::from_value(value).expect("read envelope");
        assert_eq!(decoded.exit_code(), status.exit_code().map(i64::from));
        assert_eq!(decoded.stdout().text(), output.stdout_text());
        assert_eq!(decoded.stderr().text(), output.stderr_text());
        assert_eq!(decoded.stdout().utf8(), Some(false));
        assert!(decoded.stdout().truncated());
    }
}

#[test]
fn process_envelope_reads_historical_status_and_optional_metadata() {
    let envelope: ProcessOutputEnvelope<'_> = serde_json::from_value(json!({
        "kind": "process_action",
        "status": 7,
        "stdout": {"text": "legacy output\n"},
        "permission_profile_id": "process.retired_profile",
        "permission_review": {"rationale": "recorded admission reason"},
        "future_metadata": {"version": 2},
    }))
    .expect("historical fields and unknown metadata remain readable");
    assert_eq!(envelope.exit_code(), Some(7));
    assert_eq!(envelope.ok(), None);
    assert_eq!(envelope.stdout().text(), "legacy output\n");
    assert_eq!(envelope.stdout().utf8(), None);
    assert!(!envelope.stdout().truncated());
    assert_eq!(envelope.stderr().text(), "");
    assert_eq!(
        envelope.permission_profile_id(),
        Some("process.retired_profile")
    );
    assert_eq!(
        envelope.permission_rationale(),
        Some("recorded admission reason")
    );
}

#[test]
fn process_envelope_rejects_malformed_streams_instead_of_defaulting_to_empty() {
    for stream in [
        json!({}),
        json!({"content": "not empty"}),
        json!({"text": 3}),
    ] {
        for name in ["stdout", "stderr"] {
            let mut value = json!({"kind": "process_action"});
            value[name] = stream.clone();
            assert!(serde_json::from_value::<ProcessOutputEnvelope<'_>>(value).is_err());
        }
    }
    assert!(
        serde_json::from_value::<ProcessOutputEnvelope<'_>>(json!({
            "kind": "another_tool", "stdout": {"text": "unrelated output"},
        }))
        .is_err()
    );
}
