//! Strict provider-neutral JSON conversion at the Python boundary.

use merry::{
    AgentLoopBlockedReason, AgentLoopStatus, CompactionConfig, CompactionOutcome, GenerationConfig,
    HookDecision, HookInvocation, HookInvocationId, HookPoint, HookSubscriptions,
    InteractiveInputItem, InteractiveInputSnapshot, InteractiveSettingsUpdate, ParallelToolCalls,
    ReasoningEffort, RunResult, ServiceTier, ToolHookFilter,
    binding::{
        OwnedAgentRunMessage, OwnedToolInvocationBatch, ToolInvocation, ToolInvocationContent,
        ToolInvocationResult, ToolInvocationSubmission,
    },
};
use merry_core::{
    ErrorInfo, RuntimeEvent, SessionUsage, ToolCallArguments, ToolCallBatchId, ToolCallId,
    ToolInputSchema, ToolName,
};
use schemars::Schema;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::BTreeSet,
    num::{NonZeroU64, NonZeroUsize},
};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeRunMessage<'a> {
    Event {
        event: &'a RuntimeEvent,
    },
    ToolInvocations {
        #[serde(flatten)]
        batch: NativeToolInvocationBatch<'a>,
    },
    HookInvocation {
        #[serde(flatten)]
        invocation: &'a HookInvocation,
    },
}

#[derive(Serialize)]
struct NativeToolInvocationBatch<'a> {
    id: &'a ToolCallBatchId,
    invocations: Vec<NativeToolInvocation<'a>>,
}

#[derive(Serialize)]
struct NativeToolInvocation<'a> {
    id: &'a ToolCallId,
    name: &'a ToolName,
    arguments: &'a ToolCallArguments,
}

pub(crate) fn message_to_json(message: &OwnedAgentRunMessage) -> Result<String, serde_json::Error> {
    let wire = match message {
        OwnedAgentRunMessage::Event(event) => NativeRunMessage::Event {
            event: event.as_ref(),
        },
        OwnedAgentRunMessage::ToolInvocations { batch } => NativeRunMessage::ToolInvocations {
            batch: tool_invocation_batch(batch),
        },
        OwnedAgentRunMessage::HookInvocation { invocation } => NativeRunMessage::HookInvocation {
            invocation: invocation.as_ref(),
        },
        _ => return Err(unsupported_protocol_variant("agent run message")),
    };
    serde_json::to_string(&wire)
}

fn tool_invocation_batch(batch: &OwnedToolInvocationBatch) -> NativeToolInvocationBatch<'_> {
    NativeToolInvocationBatch {
        id: batch.id(),
        invocations: batch.invocations().iter().map(tool_invocation).collect(),
    }
}

fn tool_invocation(invocation: &ToolInvocation) -> NativeToolInvocation<'_> {
    NativeToolInvocation {
        id: invocation.id(),
        name: invocation.name(),
        arguments: invocation.arguments(),
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeRunStatus<'a> {
    Completed,
    Failed {
        diagnostic: &'a ErrorInfo,
    },
    Cancelled {
        diagnostic: &'a ErrorInfo,
    },
    Blocked {
        reason: NativeBlockedReason,
    },
    StoppedByHook {
        hook_point: HookPoint,
        reason: &'a str,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeBlockedReason {
    MaxModelTurnsReached { max_model_turns: usize },
    MultiplePendingToolCalls { pending_count: usize },
    StepCompletedWithPendingToolCall { pending_count: usize },
    StepEndedWithoutTerminalEvent,
    FinalOutputToolNotCalled,
    BridgeToolCallRequested { call_id: String, tool_name: String },
}

#[derive(Serialize)]
struct NativeRunResult<'a> {
    status: NativeRunStatus<'a>,
    events: &'a [RuntimeEvent],
    model_turns_run: usize,
    final_output: Option<&'a str>,
    final_output_json: Option<NativeFinalOutput<'a>>,
    session_usage: Option<&'a SessionUsage>,
}

#[derive(Serialize)]
struct NativeFinalOutput<'a> {
    call_id: &'a ToolCallId,
    json: &'a str,
}

pub(crate) fn run_result_to_json(result: &RunResult) -> Result<String, serde_json::Error> {
    let final_output_json = result.final_output_json().map(|output| NativeFinalOutput {
        call_id: output.call_id(),
        json: output.json(),
    });
    let wire = NativeRunResult {
        status: status_to_wire(result.status())?,
        events: result.events(),
        model_turns_run: result.model_turns_run(),
        final_output: result.final_output(),
        final_output_json,
        session_usage: result.session_usage(),
    };
    serde_json::to_string(&wire)
}

fn status_to_wire(status: &AgentLoopStatus) -> Result<NativeRunStatus<'_>, serde_json::Error> {
    match status {
        AgentLoopStatus::Completed => Ok(NativeRunStatus::Completed),
        AgentLoopStatus::Failed { diagnostic } => Ok(NativeRunStatus::Failed { diagnostic }),
        AgentLoopStatus::Cancelled { diagnostic } => Ok(NativeRunStatus::Cancelled { diagnostic }),
        AgentLoopStatus::Blocked { reason } => Ok(NativeRunStatus::Blocked {
            reason: blocked_reason_to_wire(reason)?,
        }),
        AgentLoopStatus::StoppedByHook { hook_point, reason } => {
            Ok(NativeRunStatus::StoppedByHook {
                hook_point: *hook_point,
                reason: reason.as_str(),
            })
        }
        _ => Err(unsupported_protocol_variant("agent loop status")),
    }
}

fn blocked_reason_to_wire(
    reason: &AgentLoopBlockedReason,
) -> Result<NativeBlockedReason, serde_json::Error> {
    match reason {
        AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns } => {
            Ok(NativeBlockedReason::MaxModelTurnsReached {
                max_model_turns: *max_model_turns,
            })
        }
        AgentLoopBlockedReason::MultiplePendingToolCalls { pending_count } => {
            Ok(NativeBlockedReason::MultiplePendingToolCalls {
                pending_count: *pending_count,
            })
        }
        AgentLoopBlockedReason::StepCompletedWithPendingToolCall { pending_count } => {
            Ok(NativeBlockedReason::StepCompletedWithPendingToolCall {
                pending_count: *pending_count,
            })
        }
        AgentLoopBlockedReason::StepEndedWithoutTerminalEvent => {
            Ok(NativeBlockedReason::StepEndedWithoutTerminalEvent)
        }
        AgentLoopBlockedReason::FinalOutputToolNotCalled => {
            Ok(NativeBlockedReason::FinalOutputToolNotCalled)
        }
        AgentLoopBlockedReason::BridgeToolCallRequested { call_id, tool_name } => {
            Ok(NativeBlockedReason::BridgeToolCallRequested {
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
            })
        }
        _ => Err(unsupported_protocol_variant("agent loop blocked reason")),
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum NativeToolResult {
    Succeeded {
        call_id: String,
        content: NativeToolContent,
    },
    Failed {
        call_id: String,
        content: NativeToolContent,
        diagnostic: ErrorInfo,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NativeToolContent {
    Text { text: String },
    Json { json: String },
}

pub(crate) fn parse_tool_results(payload: &str) -> Result<Vec<ToolInvocationResult>, String> {
    let values = serde_json::from_str::<Vec<NativeToolResult>>(payload)
        .map_err(|error| format!("tool result payload is invalid: {error}"))?;
    values
        .into_iter()
        .map(tool_result)
        .collect::<Result<Vec<_>, _>>()
}

fn tool_result(value: NativeToolResult) -> Result<ToolInvocationResult, String> {
    match value {
        NativeToolResult::Succeeded { call_id, content } => Ok(ToolInvocationResult::succeeded(
            parse_call_id(&call_id)?,
            parse_content(content)?,
        )),
        NativeToolResult::Failed {
            call_id,
            content,
            diagnostic,
        } => Ok(ToolInvocationResult::failed(
            parse_call_id(&call_id)?,
            parse_content(content)?,
            diagnostic,
        )),
    }
}

fn parse_call_id(value: &str) -> Result<ToolCallId, String> {
    ToolCallId::new(value).map_err(|error| format!("tool call id is invalid: {error}"))
}

fn parse_content(content: NativeToolContent) -> Result<ToolInvocationContent, String> {
    match content {
        NativeToolContent::Text { text } => Ok(ToolInvocationContent::text(text)),
        NativeToolContent::Json { json } => {
            ToolInvocationContent::json(json).map_err(|error| error.to_string())
        }
    }
}

pub(crate) fn parse_batch_id(value: &str) -> Result<ToolCallBatchId, String> {
    ToolCallBatchId::new(value)
        .map_err(|error| format!("tool invocation batch id is invalid: {error}"))
}

pub(crate) fn parse_input_schema(value: &str) -> Result<ToolInputSchema, String> {
    let schema = serde_json::from_str::<Schema>(value)
        .map_err(|error| format!("tool input schema is invalid JSON: {error}"))?;
    ToolInputSchema::new(schema).map_err(|error| format!("tool input schema is invalid: {error}"))
}

#[derive(Serialize)]
struct NativeSubmission<'a> {
    status: &'a str,
}

pub(crate) fn submission_to_json(
    submission: ToolInvocationSubmission,
) -> Result<String, serde_json::Error> {
    let status = match submission {
        ToolInvocationSubmission::Accepted => "accepted",
        ToolInvocationSubmission::RejectedAndRecorded => "rejected_and_recorded",
        _ => return Err(unsupported_protocol_variant("tool invocation submission")),
    };
    serde_json::to_string(&NativeSubmission { status })
}

#[derive(Serialize)]
struct NativeInteractiveInputItem<'a> {
    id: u64,
    text: &'a str,
}

#[derive(Serialize)]
struct NativeQueuedInput<'a> {
    id: u64,
    text: &'a str,
    position: usize,
}

#[derive(Serialize)]
struct NativeInteractiveSnapshot<'a> {
    items: Vec<NativeQueuedInput<'a>>,
    paused: bool,
}

pub(crate) fn interactive_input_item_to_json(
    item: &InteractiveInputItem,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&NativeInteractiveInputItem {
        id: item.id().as_u64(),
        text: item.text(),
    })
}

pub(crate) fn interactive_input_snapshot_to_json(
    snapshot: &InteractiveInputSnapshot,
) -> Result<String, serde_json::Error> {
    let items = snapshot
        .items
        .iter()
        .enumerate()
        .map(|(position, item)| NativeQueuedInput {
            id: item.id().as_u64(),
            text: item.text(),
            position,
        })
        .collect();
    serde_json::to_string(&NativeInteractiveSnapshot {
        items,
        paused: snapshot.paused,
    })
}

#[derive(Serialize)]
struct NativeCompactionOutcome<'a> {
    checkpoint_id: &'a str,
    covered_model_turn_count: usize,
    covered_history_item_count: usize,
    retained_history_item_count: usize,
}

pub(crate) fn compaction_outcome_to_json(
    outcome: &CompactionOutcome,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&NativeCompactionOutcome {
        checkpoint_id: outcome.checkpoint_id().as_str(),
        covered_model_turn_count: outcome.covered_model_turn_count(),
        covered_history_item_count: outcome.covered_history_item_count(),
        retained_history_item_count: outcome.retained_history_item_count(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeGenerationConfig {
    max_output_tokens: Option<u64>,
    parallel_tool_calls: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCompactionConfig {
    enabled: bool,
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeInteractiveSettingsUpdate {
    #[serde(default, deserialize_with = "deserialize_optional_nullable_u64")]
    context_window_tokens: Option<Option<u64>>,
    generation: Option<NativeGenerationConfig>,
    compaction: Option<NativeCompactionConfig>,
    max_parallel_tool_calls: Option<usize>,
}

fn deserialize_optional_nullable_u64<'de, D>(
    deserializer: D,
) -> Result<Option<Option<u64>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer).map(Some)
}

pub(crate) fn parse_interactive_settings_update(
    payload: &str,
) -> Result<InteractiveSettingsUpdate, String> {
    let wire = serde_json::from_str::<NativeInteractiveSettingsUpdate>(payload)
        .map_err(|error| format!("interactive settings payload is invalid: {error}"))?;
    let mut update = InteractiveSettingsUpdate::default();
    if let Some(tokens_opt) = wire.context_window_tokens {
        let parsed = tokens_opt
            .map(|value| {
                NonZeroU64::new(value)
                    .ok_or_else(|| "context_window_tokens must be greater than zero".to_owned())
            })
            .transpose()?;
        update = update.with_context_window_tokens(parsed);
    }
    if let Some(generation) = wire.generation {
        let parallel = match generation.parallel_tool_calls.as_deref() {
            None | Some("auto") => ParallelToolCalls::Auto,
            Some("enabled") => ParallelToolCalls::Enabled,
            Some("disabled") => ParallelToolCalls::Disabled,
            Some(other) => {
                return Err(format!("unsupported parallel_tool_calls mode: {other}"));
            }
        };
        let reasoning = generation
            .reasoning_effort
            .as_deref()
            .map(ReasoningEffort::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let tier = generation
            .service_tier
            .as_deref()
            .map(ServiceTier::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let config = GenerationConfig::new(generation.max_output_tokens, false)
            .map_err(|error| error.to_string())?
            .with_parallel_tool_calls(parallel)
            .with_reasoning_effort(reasoning)
            .with_service_tier(tier);
        update = update.with_generation_config(config);
    }
    if let Some(compaction) = wire.compaction {
        let reasoning = compaction
            .reasoning_effort
            .as_deref()
            .map(ReasoningEffort::new)
            .transpose()
            .map_err(|error| error.to_string())?;
        let config = if compaction.enabled {
            CompactionConfig::default()
        } else {
            CompactionConfig::disabled()
        }
        .with_reasoning_effort(reasoning);
        update = update.with_automatic_compaction(config);
    }
    if let Some(limit) = wire.max_parallel_tool_calls {
        let limit = NonZeroUsize::new(limit)
            .ok_or_else(|| "max_parallel_tool_calls must be greater than zero".to_owned())?;
        update = update.with_max_parallel_tool_calls(limit);
    }
    Ok(update)
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum NativeToolHookFilter {
    None,
    All,
    Only { tools: Vec<String> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeHookSubscriptions {
    #[serde(default)]
    before_model_step: bool,
    #[serde(default)]
    after_model_step: bool,
    before_tool_call: NativeToolHookFilter,
    after_tool_call: NativeToolHookFilter,
}

fn parse_tool_hook_filter(filter: NativeToolHookFilter) -> Result<ToolHookFilter, String> {
    match filter {
        NativeToolHookFilter::None => Ok(ToolHookFilter::None),
        NativeToolHookFilter::All => Ok(ToolHookFilter::All),
        NativeToolHookFilter::Only { tools } => {
            let mut set = BTreeSet::new();
            for name in tools {
                let parsed = ToolName::new(&name)
                    .map_err(|error| format!("hook tool filter name is invalid: {error}"))?;
                set.insert(parsed);
            }
            Ok(if set.is_empty() {
                ToolHookFilter::None
            } else {
                ToolHookFilter::Only(set)
            })
        }
    }
}

pub(crate) fn parse_hook_subscriptions(payload: &str) -> Result<HookSubscriptions, String> {
    let wire = serde_json::from_str::<NativeHookSubscriptions>(payload)
        .map_err(|error| format!("hook subscriptions payload is invalid: {error}"))?;
    Ok(HookSubscriptions::none()
        .with_before_model_step(wire.before_model_step)
        .with_after_model_step(wire.after_model_step)
        .with_before_tool_call(parse_tool_hook_filter(wire.before_tool_call)?)
        .with_after_tool_call(parse_tool_hook_filter(wire.after_tool_call)?))
}

pub(crate) fn parse_hook_invocation_id(value: &str) -> Result<HookInvocationId, String> {
    HookInvocationId::new(value).map_err(|error| error.to_string())
}

pub(crate) fn parse_hook_decision(payload: &str) -> Result<HookDecision, String> {
    let decision = serde_json::from_str::<HookDecision>(payload)
        .map_err(|error| format!("hook decision payload is invalid: {error}"))?;
    decision.validate().map_err(|error| error.to_string())?;
    Ok(decision)
}

fn unsupported_protocol_variant(kind: &str) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("unsupported provider-neutral {kind} variant"),
    ))
}
