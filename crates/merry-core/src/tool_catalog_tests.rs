use super::*;
use crate::{ToolInputSchema, ToolName};
use schemars::Schema;
use serde_json::json;

fn entry(name: &str, operation: &str) -> SessionToolCatalogEntry {
    SessionToolCatalogEntry::new(
        ToolSpec::new(
            ToolName::new(name).unwrap(),
            "Stable tool",
            ToolInputSchema::new(Schema::try_from(json!({"type":"object"})).unwrap()).unwrap(),
        )
        .unwrap(),
        ExternalToolBinding::new(
            ToolAdapterId::new("adapter").unwrap(),
            ToolSourceId::new("source").unwrap(),
            ToolBindingName::new(operation).unwrap(),
            ToolSourceFingerprint::new("endpoint-fingerprint").unwrap(),
        ),
    )
}

#[test]
fn catalog_round_trip_preserves_definitions_bindings_and_order() {
    let catalog =
        SessionToolCatalog::new(vec![entry("zeta", "last"), entry("alpha", "first")]).unwrap();
    let bytes = serde_json::to_vec(&catalog).unwrap();
    let restored: SessionToolCatalog = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(catalog, restored);
    assert_eq!(restored.entries()[0].spec().name().as_str(), "zeta");
    assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
}

#[test]
fn catalog_rejects_duplicate_names_and_duplicate_bindings() {
    assert!(SessionToolCatalog::new(vec![entry("same", "one"), entry("same", "two")]).is_err());
    assert!(SessionToolCatalog::new(vec![entry("one", "same"), entry("two", "same")]).is_err());
}

#[test]
fn catalog_decode_rejects_unknown_fields_versions_and_invalid_identities() {
    let value =
        serde_json::to_value(SessionToolCatalog::new(vec![entry("lookup", "lookup")]).unwrap())
            .unwrap();
    let mut unknown = value.clone();
    unknown["credentials"] = json!("must-not-be-accepted");
    assert!(serde_json::from_value::<SessionToolCatalog>(unknown).is_err());
    let mut future = value.clone();
    future["format_version"] = json!(99);
    assert!(serde_json::from_value::<SessionToolCatalog>(future).is_err());
    let mut invalid = value;
    invalid["entries"][0]["binding"]["source"] = json!("bad\nsource");
    assert!(serde_json::from_value::<SessionToolCatalog>(invalid).is_err());
    let empty = SessionToolCatalog::default();
    assert_eq!(
        serde_json::from_value::<SessionToolCatalog>(serde_json::to_value(&empty).unwrap())
            .unwrap(),
        empty
    );
}
