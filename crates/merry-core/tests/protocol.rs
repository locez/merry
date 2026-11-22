use merry_core::{PlanDocument, PlanDocumentStatus, PlanDocumentSummary, PlanId};
use schemars::{JsonSchema, Schema};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

fn assert_json_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(value).expect("value should serialize");
    let decoded = serde_json::from_str::<T>(&encoded).expect("value should deserialize");
    assert_eq!(&decoded, value);
}

fn assert_schema_compiles<T: JsonSchema>() {
    let _schema = schemars::schema_for!(T);
}

fn json_schema(value: Value) -> Schema {
    Schema::try_from(value).expect("test schema should be JSON schema")
}

fn assert_uuid_v4_string(value: &str) {
    assert_eq!(value.len(), 36);
    for (index, byte) in value.bytes().enumerate() {
        match index {
            8 | 13 | 18 | 23 => assert_eq!(byte, b'-'),
            14 => assert_eq!(byte, b'4'),
            19 => assert!(
                matches!(byte, b'8' | b'9' | b'a' | b'b'),
                "uuid variant nibble should be RFC 4122"
            ),
            _ => assert!(byte.is_ascii_hexdigit()),
        }
    }
}

fn sample_plan_document() -> PlanDocument {
    PlanDocument {
        summary: PlanDocumentSummary {
            id: PlanId::new("plan-1").expect("valid plan id"),
            title: "Plan Mode Redesign".to_owned(),
            file_name: "001-plan-mode-redesign.md".to_owned(),
            status: PlanDocumentStatus::AwaitingApproval,
            revision: 2,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_005_000,
        },
        content: "# Plan Mode Redesign\n\n- Keep Plan Mode read-only.\n".to_owned(),
    }
}

#[path = "protocol/events.rs"]
mod events;

#[path = "protocol/identifiers.rs"]
mod identifiers;

#[path = "protocol/plans.rs"]
mod plans;

#[path = "protocol/subagents.rs"]
mod subagents;

#[path = "protocol/tools.rs"]
mod tools;

#[path = "protocol/usage.rs"]
mod usage;

#[path = "protocol/validation.rs"]
mod validation;
