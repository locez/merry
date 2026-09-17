//! Provider-neutral compaction requests and their cache-preserving source.

use super::{
    CitationCompactionControl, CitationCompactionInput, CitationCompactionPayloadPolicy,
    CitationCompactionPreviousCheckpoint, citation_compaction_tail_directive,
    compaction_payload_block,
};
use merry_llm::{
    GenerationConfig, ModelContent, ModelError, ModelInputItem, ModelMessage, ModelMessageRole,
    ModelName, ModelRequest, ModelResponseFormat, ModelStructuredOutputFormat, ReasoningEffort,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Immutable session request plus the source positions of its visible transcript.
pub(crate) struct CompactionRequestSource {
    request: ModelRequest,
    history_indices: BTreeMap<u64, usize>,
}

impl CompactionRequestSource {
    /// Maps visible history ids onto the already compiled session request.
    pub(crate) fn new(
        request: ModelRequest,
        history_ids: &[u64],
        current_message_count: usize,
    ) -> Result<Self, ModelError> {
        let start = request
            .input()
            .len()
            .checked_sub(history_ids.len().saturating_add(current_message_count))
            .ok_or_else(|| {
                ModelError::invalid_request("compaction transcript exceeds source request")
            })?;
        if start < request.stable_prefix_item_count() {
            return Err(ModelError::invalid_request(
                "compaction transcript overlaps the stable prefix",
            ));
        }
        Ok(Self {
            request,
            history_indices: history_ids
                .iter()
                .enumerate()
                .map(|(offset, id)| (*id, start + offset))
                .collect(),
        })
    }

    /// Drops covered refs that are not present as input items in this source request.
    pub(crate) fn retain_visible_refs(&self, input: &mut CitationCompactionInput) {
        let visible = input
            .manifest
            .refs()
            .iter()
            .filter(|reference| {
                !input
                    .covered_history_ids
                    .contains(&reference.sequence_range().end())
                    || self
                        .history_indices
                        .contains_key(&reference.sequence_range().end())
            })
            .map(|reference| reference.id().as_str())
            .collect::<BTreeSet<_>>();
        input
            .model_supplied_ref_ids
            .retain(|ref_id| visible.contains(ref_id.as_str()));
        input
            .payload
            .available_ref_ids
            .retain(|ref_id| visible.contains(ref_id.as_str()));
    }

    pub(crate) fn request(&self) -> &ModelRequest {
        &self.request
    }
}

/// How one compaction request reuses or rebuilds the session input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionRequestMode {
    /// Keep the session input and tools; append a tail directive and ref index.
    Append,
    /// Rebuild from the stable prefix plus a serialized history payload.
    Payload,
}

/// Source request plus the reuse mode for one fitted attempt.
pub(crate) struct CompactionRequestProjection<'a> {
    pub(crate) source: &'a CompactionRequestSource,
    pub(crate) mode: CompactionRequestMode,
}

#[derive(Serialize)]
struct AppendPayload<'a> {
    policy: &'a CitationCompactionPayloadPolicy,
    control: &'a CitationCompactionControl,
    available_ref_ids: &'a [String],
    previous_checkpoint: &'a Option<CitationCompactionPreviousCheckpoint>,
    covered_history_references: Vec<HistoryReference<'a>>,
}

#[derive(Serialize)]
struct HistoryReference<'a> {
    ref_id: &'a str,
    input_item_index: usize,
}

/// Compiles one compaction request from a session source.
///
/// Append mode keeps the original input and tool catalog so the provider can
/// reuse its prefix cache. Payload mode is the cache-breaking fallback used
/// when that request cannot fit the compaction window.
pub(crate) fn compile_citation_compaction_model_request(
    input: &CitationCompactionInput,
    model: &ModelName,
    source: &CompactionRequestSource,
    mode: CompactionRequestMode,
    reasoning_effort: Option<&ReasoningEffort>,
    output_ceiling_tokens: u64,
) -> Result<ModelRequest, ModelError> {
    let original = source.request();
    let (mut items, payload) = match mode {
        CompactionRequestMode::Append => {
            let references = input
                .manifest()
                .refs()
                .iter()
                .filter(|reference| {
                    input
                        .covered_history_ids()
                        .contains(&reference.sequence_range().end())
                })
                .filter_map(|reference| {
                    source
                        .history_indices
                        .get(&reference.sequence_range().end())
                        .map(|index| HistoryReference {
                            ref_id: reference.id().as_str(),
                            input_item_index: *index,
                        })
                })
                .collect::<Vec<_>>();
            let payload = serde_json::to_string(&AppendPayload {
                policy: &input.payload.policy,
                control: &input.payload.control,
                available_ref_ids: &input.payload.available_ref_ids,
                previous_checkpoint: &input.payload.previous_checkpoint,
                covered_history_references: references,
            })
            .map_err(|error| ModelError::invalid_request(error.to_string()))?;
            (original.input().to_vec(), payload)
        }
        CompactionRequestMode::Payload => (
            original.stable_prefix_input().to_vec(),
            input
                .to_model_payload_json()
                .map_err(|error| ModelError::invalid_request(error.to_string()))?,
        ),
    };
    let response_schema = input
        .model_response_schema()
        .map_err(|error| ModelError::invalid_request(error.to_string()))?;
    let response_format = match mode {
        CompactionRequestMode::Append => original.response_format().cloned(),
        CompactionRequestMode::Payload => Some(ModelResponseFormat::StructuredOutput(
            ModelStructuredOutputFormat::new(
                "compacted_checkpoint_candidate",
                response_schema.clone(),
            )?,
        )),
    };
    let schema_directive = match mode {
        CompactionRequestMode::Append => {
            format!(
                "Return one JSON object matching this schema:\n{}",
                response_schema.as_value()
            )
        }
        CompactionRequestMode::Payload => String::new(),
    };
    let directive = format!(
        "{}\n{}\nSummary soft target: at most {} estimated tokens; hard rendered-summary limit: {} estimated tokens. This is NOT the generation/reasoning budget. The soft target is guidance; the hard limit is mandatory. Both apply AFTER restoring every kept old entry, including text, rationale, refs and framing. Runtime estimates tokens as UTF-8 bytes divided by 4, rounded up.\nCovered history is identified by the payload refs only. In append mode, input_item_index is zero-based in the preceding input items; a tool result ref also covers its matching call. All other history and current input remain raw and MUST NOT be summarized.\n{}",
        citation_compaction_tail_directive(),
        compaction_payload_block(&payload),
        input.resolved_budget().target_output_tokens(),
        input.resolved_budget().output_token_limit(),
        schema_directive,
    );
    items.push(ModelInputItem::Message(ModelMessage::new(
        ModelMessageRole::User,
        ModelContent::text(&directive)?,
    )?));
    let generation = GenerationConfig::new(Some(output_ceiling_tokens), false)?
        .with_reasoning_effort(reasoning_effort.cloned());
    ModelRequest::new_with_input_and_stable_prefix_and_response_format(
        model.clone(),
        items,
        original.tools().to_vec(),
        generation,
        original.stable_prefix_item_count(),
        response_format,
    )
}
