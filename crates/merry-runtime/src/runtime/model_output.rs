use super::diagnostic_from_text;
use merry_core::{CoreError, ErrorInfo, PendingToolCall, ToolCallArguments, ToolCallId};
use merry_llm::{ModelError, ModelOutput, ModelToolCall, ProviderErrorKind};

const DIAGNOSTIC_MODEL_TOOL_CALL_INVALID: &str = "model_tool_call_invalid";
const DIAGNOSTIC_MODEL_TOOL_CALL_MISSING: &str = "model_tool_call_missing";
pub(super) const DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT: &str = "model_tool_call_mixed_output";
const DIAGNOSTIC_MODEL_TOOL_CALL_STREAM_MISMATCH: &str = "model_tool_call_stream_mismatch";

pub(super) fn record_streamed_tool_call(
    streamed_tool_calls: &mut Vec<PendingToolCall>,
    call: PendingToolCall,
) -> Result<(), ErrorInfo> {
    match streamed_tool_calls
        .iter()
        .find(|existing| existing.id() == call.id())
    {
        Some(existing) if existing == &call => Ok(()),
        Some(_) => Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_STREAM_MISMATCH,
            "model streamed conflicting payloads for the same tool call id",
        )),
        None => {
            streamed_tool_calls.push(call);
            Ok(())
        }
    }
}

pub(super) fn pending_tool_calls_from_outputs(
    outputs: &[ModelOutput],
    streamed_tool_calls: &[PendingToolCall],
) -> Result<Vec<PendingToolCall>, ErrorInfo> {
    if outputs.is_empty() {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    }

    let tool_call_count = outputs
        .iter()
        .filter(|output| matches!(output, ModelOutput::ToolCall { .. }))
        .count();
    if tool_call_count == 0 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    }

    let completed_calls = outputs
        .iter()
        .filter_map(|output| match output {
            ModelOutput::ToolCall { call } => Some(call),
            ModelOutput::Text { .. } => None,
        })
        .map(pending_tool_call_from_model)
        .collect::<Result<Vec<_>, _>>()?;

    let mut seen = std::collections::BTreeSet::new();
    if completed_calls.iter().any(|call| !seen.insert(call.id())) {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_INVALID,
            "model returned duplicate tool call ids in one response",
        ));
    }

    if streamed_tool_calls.is_empty() || streamed_tool_calls == completed_calls {
        Ok(completed_calls)
    } else {
        Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_STREAM_MISMATCH,
            "model completed with tool calls that differ from the streamed tool calls",
        ))
    }
}

pub(super) fn tool_call_commentary_text(
    outputs: &[ModelOutput],
    streamed_text: &str,
) -> Option<String> {
    if !streamed_text.is_empty() {
        return Some(streamed_text.to_owned());
    }

    let mut text = String::new();
    for output in outputs {
        if let ModelOutput::Text { text: output_text } = output
            && !output_text.is_empty()
        {
            text.push_str(output_text);
        }
    }

    (!text.is_empty()).then_some(text)
}

pub(super) fn pending_tool_call_from_model(
    call: &ModelToolCall,
) -> Result<PendingToolCall, ErrorInfo> {
    let id = ToolCallId::new(call.id().as_str()).map_err(tool_call_conversion_diagnostic)?;
    let arguments = ToolCallArguments::new(call.arguments().as_object().clone());
    Ok(PendingToolCall::new(id, call.name().clone(), arguments))
}

fn tool_call_conversion_diagnostic(error: CoreError) -> ErrorInfo {
    diagnostic_from_text(
        DIAGNOSTIC_MODEL_TOOL_CALL_INVALID,
        format!("model tool call could not be normalized: {error}"),
    )
}

pub(super) fn is_cancelled_model_error(error: &ModelError) -> bool {
    error.kind() == ProviderErrorKind::Cancelled
}

pub(super) fn diagnostic_from_model_error(error: ModelError) -> ErrorInfo {
    let code = match error.kind() {
        ProviderErrorKind::InvalidRequest => "model_invalid_request",
        ProviderErrorKind::InvalidToolCall => "model_invalid_tool_call",
        ProviderErrorKind::Cancelled => "cancelled",
        ProviderErrorKind::Authentication => "model_authentication",
        ProviderErrorKind::RateLimited => "model_rate_limited",
        ProviderErrorKind::Unavailable => "model_unavailable",
        ProviderErrorKind::Protocol => "model_protocol",
        ProviderErrorKind::Other => "model_other",
    };

    diagnostic_from_text(code, error.to_string())
}
