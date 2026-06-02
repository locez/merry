use merry_core::{MerryErrorDomain, MerryErrorInfo, MerryRetryability};

#[test]
fn binding_error_info_shape_matches_sdk_contract() {
    let info = MerryErrorInfo::builder(
        "runtime.invalid_session_id",
        MerryErrorDomain::Runtime,
        "invalid session id",
        MerryRetryability::UserActionRequired,
    )
    .hint("Use a non-empty session id.")
    .build()
    .expect("valid error info");

    let value = serde_json::to_value(&info).expect("serializes");

    assert_eq!(value["code"], "runtime.invalid_session_id");
    assert_eq!(value["domain"], "runtime");
    assert_eq!(value["retryability"], "user_action_required");
    assert_eq!(value["hint"], "Use a non-empty session id.");
}
