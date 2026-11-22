use crate::{assert_json_round_trip, assert_uuid_v4_string};
use merry_core::{PlanId, ProviderName, SessionId, SkillId, SubagentId, SubagentTaskId};
use serde_json::json;
use std::str::FromStr;

#[test]
fn ids_validate_and_round_trip_as_json_strings() {
    let session = SessionId::new("session-1").expect("valid session id");
    let uuid_session = SessionId::new("550e8400-e29b-41d4-a716-446655440000")
        .expect("uuid session id should be valid");
    let skill = SkillId::try_from("skill.alpha").expect("valid skill id");
    let provider =
        ProviderName::try_from(String::from("openai-compatible")).expect("valid provider name");

    assert_eq!(session.as_str(), "session-1");
    assert_eq!(
        uuid_session.as_str(),
        "550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(skill.as_str(), "skill.alpha");
    assert_eq!(provider.as_str(), "openai-compatible");

    assert_eq!(
        serde_json::to_value(&session).expect("session serializes"),
        json!("session-1")
    );

    assert_json_round_trip(&session);
    assert_json_round_trip(&uuid_session);
    assert_json_round_trip(&skill);
    assert_json_round_trip(&provider);

    for invalid in ["", "   ", " has-leading", "has-trailing ", "has\nnewline"] {
        assert!(
            SessionId::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
        assert!(serde_json::from_value::<SessionId>(json!(invalid)).is_err());
        assert!(serde_json::from_value::<SkillId>(json!(invalid)).is_err());
        assert!(serde_json::from_value::<ProviderName>(json!(invalid)).is_err());
    }

    for invalid in [
        "bad/session",
        "bad\\session",
        "bad:session",
        "bad space",
        ".",
        "..",
    ] {
        assert!(
            SessionId::new(invalid).is_err(),
            "{invalid:?} should reject as a filesystem-safe session id"
        );
        assert!(serde_json::from_value::<SessionId>(json!(invalid)).is_err());
    }

    let overlong = "a".repeat(129);
    assert!(SkillId::new(&overlong).is_err());
    assert!(serde_json::from_value::<SkillId>(json!(overlong)).is_err());
}

#[test]
fn session_id_random_generates_distinct_uuid_v4_ids() {
    let first = SessionId::random();
    let second = SessionId::random();

    assert_ne!(first, second);
    assert_uuid_v4_string(first.as_str());
    assert_uuid_v4_string(second.as_str());
    assert_json_round_trip(&first);
    assert_json_round_trip(&second);
}

#[test]
fn subagent_ids_validate_and_round_trip_as_json_strings() {
    let agent = SubagentId::new("subagent-1").expect("valid subagent id");
    let task = SubagentTaskId::from_str("subagent-task_1").expect("valid subagent task id");

    assert_eq!(agent.as_str(), "subagent-1");
    assert_eq!(task.to_string(), "subagent-task_1");
    assert_eq!(
        serde_json::to_value(&agent).expect("subagent id serializes"),
        json!("subagent-1")
    );
    assert_eq!(
        serde_json::to_value(&task).expect("subagent task id serializes"),
        json!("subagent-task_1")
    );

    assert_json_round_trip(&agent);
    assert_json_round_trip(&task);

    for invalid in ["", "   ", " has-leading", "has-trailing ", "has\nnewline"] {
        assert!(
            SubagentId::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
        assert!(
            serde_json::from_value::<SubagentTaskId>(json!(invalid)).is_err(),
            "{invalid:?} should reject during deserialize"
        );
    }

    let overlong = "a".repeat(129);
    assert!(SubagentId::new(&overlong).is_err());
    assert!(serde_json::from_value::<SubagentTaskId>(json!(overlong)).is_err());
}

#[test]
fn plan_identifiers_validate_and_round_trip_as_json_strings() {
    let plan = PlanId::new("plan-1").expect("valid plan id");
    assert_eq!(plan.as_str(), "plan-1");
    assert_json_round_trip(&plan);

    for invalid in ["", "   ", " leading", "trailing ", "has\nnewline"] {
        assert!(PlanId::new(invalid).is_err());
    }
}
