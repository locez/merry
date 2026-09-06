use crate::assert_json_round_trip;
use merry_llm::{GenerationConfig, ModelCapabilities, ParallelToolCalls, ServiceTier};
use serde_json::json;

#[test]
fn generation_parallel_tool_calls_resolve_against_provider_capabilities() {
    let supported =
        ModelCapabilities::new(true, true, true, true, None, None).expect("valid capabilities");
    let unsupported =
        ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities");

    let default = GenerationConfig::default();
    assert_eq!(default.parallel_tool_calls(), ParallelToolCalls::Auto);
    assert_eq!(
        default
            .clone()
            .resolve_parallel_tool_calls(&supported)
            .expect("auto should resolve")
            .parallel_tool_calls(),
        ParallelToolCalls::Enabled
    );
    assert_eq!(
        default
            .resolve_parallel_tool_calls(&unsupported)
            .expect("auto should resolve")
            .parallel_tool_calls(),
        ParallelToolCalls::Disabled
    );
    assert!(
        GenerationConfig::new(None, true)
            .expect("valid generation config")
            .resolve_parallel_tool_calls(&unsupported)
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(GenerationConfig::default()).expect("generation should serialize"),
        json!({
            "max_output_tokens": null,
            "parallel_tool_calls": "auto"
        })
    );
}

#[test]
fn service_tier_parses_supported_identifiers_and_rejects_unknown_values() {
    for tier in ServiceTier::ALL {
        assert_eq!(
            ServiceTier::new(tier.as_str()).expect("tier is valid"),
            tier
        );
        assert_eq!(tier.to_string(), tier.as_str());
    }
    assert_eq!(
        ServiceTier::ALL.map(ServiceTier::as_str),
        [
            "ultrafast",
            "auto",
            "default",
            "fast",
            "flex",
            "priority",
            "scale"
        ]
    );
    let error = ServiceTier::new("turbo").expect_err("unknown tier must be rejected");
    assert!(
        error.to_string().contains("turbo"),
        "unexpected error: {error}"
    );
    assert!(ServiceTier::new("Priority").is_err());
}

#[test]
fn generation_config_carries_optional_service_tier() {
    let generation = GenerationConfig::default().with_service_tier(Some(ServiceTier::Flex));
    assert_eq!(generation.service_tier(), Some(ServiceTier::Flex));
    assert_eq!(
        serde_json::to_value(&generation).expect("generation should serialize"),
        json!({
            "max_output_tokens": null,
            "parallel_tool_calls": "auto",
            "service_tier": "flex"
        })
    );
    assert_json_round_trip(&generation);
    let parsed: GenerationConfig = serde_json::from_value(json!({
        "max_output_tokens": null,
        "service_tier": "priority"
    }))
    .expect("generation should deserialize");
    assert_eq!(parsed.service_tier(), Some(ServiceTier::Priority));
    assert!(
        serde_json::from_value::<GenerationConfig>(json!({
            "max_output_tokens": null,
            "service_tier": "turbo"
        }))
        .is_err()
    );
}
