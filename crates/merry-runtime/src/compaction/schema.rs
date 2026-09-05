use crate::checkpoint::compacted_checkpoint_candidate_schema;
use schemars::Schema;
use serde_json::Value;

pub fn citation_compaction_response_schema() -> Schema {
    compacted_checkpoint_candidate_schema()
}

pub(crate) fn citation_compaction_response_schema_for_refs(
    available_ref_ids: &[&str],
) -> Result<Schema, &'static str> {
    let mut schema = compacted_checkpoint_candidate_schema();
    let defs = schema
        .as_object_mut()
        .and_then(|object| object.get_mut("$defs"))
        .and_then(Value::as_object_mut)
        .ok_or("checkpoint candidate schema is missing $defs")?;
    let entry_schema = defs
        .get_mut("CheckpointEntryWire")
        .and_then(Value::as_object_mut)
        .ok_or("checkpoint candidate schema is missing CheckpointEntryWire")?;
    let properties = entry_schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .ok_or("checkpoint entry schema is missing properties")?;
    let refs_schema = properties
        .get_mut("refs")
        .and_then(Value::as_object_mut)
        .ok_or("checkpoint entry schema is missing refs")?;
    let refs_items = refs_schema
        .get_mut("items")
        .and_then(Value::as_object_mut)
        .ok_or("checkpoint refs schema is missing items")?;
    refs_items.insert(
        "enum".to_owned(),
        Value::Array(
            available_ref_ids
                .iter()
                .map(|ref_id| Value::String((*ref_id).to_owned()))
                .collect(),
        ),
    );
    Ok(schema)
}
