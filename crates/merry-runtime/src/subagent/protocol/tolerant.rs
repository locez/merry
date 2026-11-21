//! Tolerant deserialization helpers for provider-supplied subagent tool inputs.
//!
//! Models occasionally emit sentinel strings (`""`, `"null"`, `"none"`,
//! `"undefined"`), accidentally quote path entries, pass a bare string instead
//! of a 1-element array, or use singular aliases (`agent_id` / `task`). These
//! helpers normalize those harmless wire quirks into strict domain inputs while
//! keeping JSON schemas canonical and crisp.

use merry_core::SubagentId;
use serde::{Deserialize, Deserializer, de};

pub(super) fn is_sentinel_str(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("null")
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("undefined")
}

/// Strips accidental matching single or double quotes around a model-supplied path.
pub(super) fn clean_path_str(raw: String) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        return trimmed[1..trimmed.len() - 1].to_owned();
    }
    raw
}

pub(super) fn deserialize_optional_string_tolerant<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| (!is_sentinel_str(&value)).then_some(value)))
}

pub(super) fn deserialize_optional_enum_tolerant<'de, T, D>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    match raw {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(ref value)) if is_sentinel_str(value) => Ok(None),
        Some(value) => T::deserialize(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

pub(super) fn deserialize_optional_paths_tolerant<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawPaths {
        List(Vec<String>),
        Single(String),
    }

    let raw = Option::<RawPaths>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(RawPaths::Single(value)) => {
            if is_sentinel_str(&value) {
                Ok(None)
            } else {
                Ok(Some(vec![clean_path_str(value)]))
            }
        }
        Some(RawPaths::List(items)) => Ok(Some(items.into_iter().map(clean_path_str).collect())),
    }
}

pub(super) fn deserialize_agent_ids_field<'de, D>(
    deserializer: D,
) -> Result<Vec<SubagentId>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawIds {
        List(Vec<SubagentId>),
        Single(SubagentId),
    }

    match RawIds::deserialize(deserializer)? {
        RawIds::List(ids) => Ok(ids),
        RawIds::Single(id) => Ok(vec![id]),
    }
}

pub(super) fn deserialize_optional_agent_ids_field<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<SubagentId>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawIds {
        List(Vec<SubagentId>),
        Single(SubagentId),
    }

    match Option::<RawIds>::deserialize(deserializer)? {
        None => Ok(None),
        Some(RawIds::List(ids)) => Ok(Some(ids)),
        Some(RawIds::Single(id)) => Ok(Some(vec![id])),
    }
}

pub(super) fn merge_agent_ids<E: de::Error>(
    agent_ids: Option<Vec<SubagentId>>,
    agent_id: Option<SubagentId>,
) -> Result<Vec<SubagentId>, E> {
    match (agent_ids, agent_id) {
        (Some(ids), None) => Ok(ids),
        (None, Some(id)) => Ok(vec![id]),
        (Some(mut ids), Some(id)) => {
            if !ids.contains(&id) {
                ids.push(id);
            }
            Ok(ids)
        }
        (None, None) => Err(de::Error::missing_field("agent_ids")),
    }
}
