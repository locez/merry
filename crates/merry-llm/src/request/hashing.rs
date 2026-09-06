//! Deterministic hashes for stable request prefixes, tools, and dynamic inputs.

use crate::request::{message::ModelInputItem, response_format::ModelResponseFormat};
use merry_core::ToolSpec;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(super) const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

pub(super) const FNV_PRIME: u64 = 0x100000001b3;

/// Stable fingerprint of the provider-neutral tool profile visible to a request.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ToolProfileHash(String);

impl ToolProfileHash {
    /// Borrows the stable hash label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable fingerprint of provider-neutral request content.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct RequestContentHash(String);

impl RequestContentHash {
    /// Borrows the stable hash label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(super) fn tool_profile_hash(tools: &[ToolSpec]) -> ToolProfileHash {
    let mut canonical_tools = tools
        .iter()
        .map(|tool| {
            serde_json::to_string(tool)
                .expect("provider-neutral tool specs must serialize for profile hashing")
        })
        .collect::<Vec<_>>();
    canonical_tools.sort();

    let mut hash = FNV_OFFSET_BASIS;
    for tool in canonical_tools {
        for byte in tool.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
        }
        hash = (hash ^ 0xff).wrapping_mul(FNV_PRIME);
    }

    ToolProfileHash(format!("fnv1a64:{hash:016x}"))
}

pub(super) fn stable_input_prefix_hash(
    input: &[ModelInputItem],
    tools: &[ToolSpec],
    response_format: Option<&ModelResponseFormat>,
) -> RequestContentHash {
    let mut chunks = input
        .iter()
        .map(|item| stable_chunk("input", item))
        .collect::<Vec<_>>();
    let mut tool_chunks = tools
        .iter()
        .map(|tool| stable_chunk("tool", tool))
        .collect::<Vec<_>>();
    tool_chunks.sort();
    chunks.extend(tool_chunks);
    if let Some(response_format) = response_format {
        chunks.push(stable_chunk("response_format", response_format));
    }
    request_content_hash(chunks)
}

pub(super) fn dynamic_input_hash(input: &[ModelInputItem]) -> RequestContentHash {
    let chunks = input
        .iter()
        .map(|item| stable_chunk("input", item))
        .collect::<Vec<_>>();
    request_content_hash(chunks)
}

pub(super) fn stable_chunk<T>(kind: &'static str, value: &T) -> String
where
    T: Serialize,
{
    format!(
        "{kind}:{}",
        serde_json::to_string(value).expect("provider-neutral request content must serialize")
    )
}

pub(super) fn request_content_hash(chunks: Vec<String>) -> RequestContentHash {
    let mut hash = FNV_OFFSET_BASIS;
    for chunk in chunks {
        for byte in chunk.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
        }
        hash = (hash ^ 0xff).wrapping_mul(FNV_PRIME);
    }

    RequestContentHash(format!("fnv1a64:{hash:016x}"))
}
