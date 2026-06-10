use super::{
    audit::JudgmentRecordId,
    core::{JudgmentEvidence, JudgmentOutcome, JudgmentRecommendation, JudgmentRequest},
    error::JudgmentError,
};
use merry_core::EvidenceLocator;
use std::fmt::Write as _;

const JUDGMENT_PAYLOAD_SCHEMA_VERSION: &str = "merry.judgment.audit.v1";

pub(super) fn validate_record_id(value: &str) -> Result<(), JudgmentError> {
    if value.is_empty() {
        return Err(invalid_record_id(value, "must not be empty"));
    }

    if value.trim().is_empty() {
        return Err(invalid_record_id(value, "must not be whitespace only"));
    }

    if value.trim() != value {
        return Err(invalid_record_id(
            value,
            "must not have leading or trailing whitespace",
        ));
    }

    if value.chars().count() > 128 {
        return Err(invalid_record_id(
            value,
            "is longer than the allowed maximum length",
        ));
    }

    if value.chars().any(char::is_control) {
        return Err(invalid_record_id(
            value,
            "must not contain control characters",
        ));
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_record_id(
            value,
            "must contain only ASCII letters, digits, '-', '_' or '.'",
        ));
    }

    Ok(())
}

fn invalid_record_id(value: &str, reason: &'static str) -> JudgmentError {
    JudgmentError::InvalidRecordId {
        value: value.to_owned(),
        reason,
    }
}

pub(super) fn render_request_payload(
    record_id: &JudgmentRecordId,
    order: u64,
    request: &JudgmentRequest,
) -> String {
    let mut payload = String::new();
    push_field(
        &mut payload,
        "schema_version",
        JUDGMENT_PAYLOAD_SCHEMA_VERSION,
    );
    push_field(&mut payload, "artifact", "request");
    push_field(&mut payload, "record_id", record_id.as_str());
    push_field(&mut payload, "commit_order", &order.to_string());
    push_field(&mut payload, "purpose", request.purpose().as_str());
    push_field(&mut payload, "subject", request.subject());
    push_field(&mut payload, "input", request.input());
    push_field(&mut payload, "source_label", request.source_label());
    push_list(&mut payload, "constraints", request.constraints());
    push_evidence(&mut payload, "evidence", request.evidence());
    payload
}

pub(super) fn render_outcome_payload(
    record_id: &JudgmentRecordId,
    order: u64,
    outcome: &JudgmentOutcome,
) -> String {
    let mut payload = String::new();
    push_field(
        &mut payload,
        "schema_version",
        JUDGMENT_PAYLOAD_SCHEMA_VERSION,
    );
    push_field(&mut payload, "artifact", "outcome");
    push_field(&mut payload, "record_id", record_id.as_str());
    push_field(&mut payload, "commit_order", &order.to_string());
    push_field(&mut payload, "purpose", outcome.purpose().as_str());
    push_recommendation(&mut payload, outcome.recommendation());
    push_field(
        &mut payload,
        "confidence",
        &format!("{:.6}", outcome.confidence().as_f32()),
    );
    push_evidence(&mut payload, "evidence", outcome.evidence());
    push_field(&mut payload, "rationale", outcome.rationale());
    push_field(&mut payload, "uncertainty", outcome.uncertainty());
    push_field(
        &mut payload,
        "provenance.payload",
        outcome.provenance().source_kind().as_str(),
    );
    push_field(
        &mut payload,
        "provenance.label",
        outcome.provenance().source_label(),
    );
    payload
}

fn push_recommendation(payload: &mut String, recommendation: &JudgmentRecommendation) {
    push_field(payload, "recommendation.payload", recommendation.as_str());

    match recommendation {
        JudgmentRecommendation::SummaryDraft { draft } => {
            push_field(payload, "recommendation.draft", draft);
        }
        JudgmentRecommendation::ToolRiskReview { risk, concerns } => {
            push_field(payload, "recommendation.risk", risk.as_str());
            push_list(payload, "recommendation.concerns", concerns);
        }
        JudgmentRecommendation::MemoryRelevant
        | JudgmentRecommendation::MemoryNotRelevant
        | JudgmentRecommendation::NoRecommendation => {}
    }
}

pub(super) fn push_list(payload: &mut String, name: &str, values: &[String]) {
    push_field(payload, &format!("{name}.count"), &values.len().to_string());
    for (index, value) in values.iter().enumerate() {
        push_field(payload, &format!("{name}.{index}"), value);
    }
}

pub(super) fn push_evidence(payload: &mut String, name: &str, evidence: &[JudgmentEvidence]) {
    push_field(
        payload,
        &format!("{name}.count"),
        &evidence.len().to_string(),
    );
    for (index, item) in evidence.iter().enumerate() {
        push_field(payload, &format!("{name}.{index}.label"), item.label());
        push_field(
            payload,
            &format!("{name}.{index}.artifact_id"),
            item.reference().artifact_id.as_str(),
        );
        push_field(
            payload,
            &format!("{name}.{index}.locator"),
            &format_locator(&item.reference().locator),
        );
    }
}

pub(super) fn push_field(payload: &mut String, key: &str, value: &str) {
    writeln!(payload, "{key}={}", escape_payload_value(value))
        .expect("writing to a String cannot fail");
}

fn escape_payload_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }

    escaped
}

fn format_locator(locator: &EvidenceLocator) -> String {
    if locator.is_whole_artifact() {
        return "whole".to_owned();
    }

    if let Some((start, end)) = locator.as_line_range() {
        return format!("line:{start}-{end}");
    }

    if let Some((start, end)) = locator.as_byte_range() {
        return format!("byte:{start}-{end}");
    }

    if let Some(pointer) = locator.as_json_pointer() {
        return format!("json:{pointer}");
    }

    if let Some(name) = locator.as_named_section() {
        return format!("section:{name}");
    }

    unreachable!("all evidence locator variants are covered by public accessors")
}

pub(super) fn validate_non_blank(field: &'static str, value: &str) -> Result<(), JudgmentError> {
    if value.trim().is_empty() {
        return Err(JudgmentError::BlankField { field });
    }

    Ok(())
}

pub(super) fn canonicalize_label_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
