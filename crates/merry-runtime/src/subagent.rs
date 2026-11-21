//! Runtime-owned parallel subagent tool contracts.

mod activity;
mod child;
mod handoff;
mod manager;
mod messaging;
mod plan_link;
mod protocol;
mod scheduler;
mod scope;
mod spec;
mod tools;

pub use activity::{SubagentActivityHub, SubagentActivityReceiver};
pub(crate) use child::sanitize_diagnostic_message;
pub use handoff::{
    ContextFileRef, MAX_CONTEXT_FILE_REFS, MAX_CONTEXT_FILES_TOTAL_BYTES, PreloadedContextFile,
    preload_context_files, render_subagent_handoff,
};
pub use manager::{SubagentInspectSession, SubagentManager};
pub use messaging::SendSubagentMessageOptions;
pub(crate) use plan_link::plan_link_runtime_for_controller;
pub use plan_link::{PlanLinkRuntime, PlanSubagentScope};
pub use protocol::{
    BackgroundHandoffNotice, CancelSubagentsInput, RejectedSubagentView, SendSubagentMessageInput,
    SendSubagentMessageOutcome, SendSubagentMessageOutput, SpawnSubagentTaskInput,
    SpawnSubagentsInput, SpawnSubagentsOutput, SpawnWaitMode, SpawnedSubagentStatusLabel,
    SpawnedSubagentView, SubagentMessageDelivery, SubagentResultView, SubagentStatusLabel,
    SubagentStatusView, WaitMode, WaitSubagentsInput, WaitSubagentsOutput,
};
pub use scope::{ChildRuntimeFactory, ChildRuntimeInput, ChildWorkspaceScope};
use spec::validate_task_max_model_turns;
pub use spec::{
    DEFAULT_FOREGROUND_WAIT_BUDGET_SECS, DEFAULT_MAX_MODEL_TURNS, DEFAULT_MIN_MODEL_TURNS,
    SubagentConfig, SubagentError, SubagentTaskSpec, validate_no_write_scope_conflicts,
};
pub use tools::{subagent_registered_tools, subagent_tool_specs};

/// Maximum UTF-8 bytes of a single child conclusion inlined in a completion notification.
const MAX_INLINE_CONCLUSION_BYTES: usize = 8 * 1024;

/// Formats terminal child statuses as a high-signal `<subagent-notification>`
/// harness continuation block so the coordinator can synthesize findings without
/// burning an extra `wait_subagents` round trip.
pub(crate) fn completion_notification_text(statuses: &[SubagentStatusView]) -> String {
    let mut text = String::from(
        "<subagent-notification>\n\
         Runtime notification (not a user message). The following subagent(s) reached a terminal state. Incorporate their verified findings/changes before reporting to the user; do not thank or converse with this notification.\n",
    );
    for status in statuses {
        text.push_str("\n<subagent id=\"");
        text.push_str(status.agent_id.as_str());
        text.push('"');
        if let Some(name) = status.display_name.as_deref() {
            text.push_str(" name=\"");
            text.push_str(&sanitize_inline_text(name));
            text.push('"');
        }
        if let Some(role) = status.role.as_deref() {
            text.push_str(" role=\"");
            text.push_str(&sanitize_inline_text(role));
            text.push('"');
        }
        text.push_str(" status=\"");
        text.push_str(status.status.as_str());
        text.push_str("\">\n");

        text.push_str(&format!(
            "stats: turns={} tool_calls={} elapsed_ms={} raw_output_bytes={}",
            status.turns_used, status.tool_calls, status.elapsed_ms, status.raw_output_bytes
        ));
        if let Some(usage) = status.usage.as_ref() {
            text.push_str(&format!(
                " tokens_in={} tokens_out={}",
                usage.input_tokens, usage.output_tokens
            ));
        }
        text.push('\n');

        text.push_str("summary: ");
        text.push_str(&sanitize_inline_text(&status.summary));
        text.push('\n');

        if !status.changed_paths.is_empty() {
            text.push_str("changed_paths: ");
            text.push_str(&status.changed_paths.join(", "));
            text.push('\n');
        }

        if let Some(diagnostic) = status.diagnostics.as_ref() {
            text.push_str("diagnostic: ");
            text.push_str(diagnostic.code());
            text.push_str(": ");
            text.push_str(&sanitize_inline_text(diagnostic.message()));
            text.push('\n');
        }

        if let Some(result) = status.result.as_ref() {
            text.push_str("conclusion:\n");
            let (inlined, truncated) =
                truncate_utf8_with_flag(&result.conclusion, MAX_INLINE_CONCLUSION_BYTES);
            text.push_str(inlined);
            if !inlined.ends_with('\n') {
                text.push('\n');
            }
            if truncated {
                text.push_str(&format!(
                    "...[truncated; call wait_subagents(agent_ids=[\"{}\"]) to read full output]\n",
                    status.agent_id.as_str()
                ));
            }
        }

        if status.status == SubagentStatusLabel::Blocked {
            text.push_str(&format!(
                "continue: call send_subagent_message(agent_id=\"{}\", extra_model_turns=...) to resume this subagent with its retained transcript.\n",
                status.agent_id.as_str()
            ));
        } else {
            text.push_str(&format!(
                "continue: call send_subagent_message(agent_id=\"{}\", message=...) to continue with this subagent's retained context.\n",
                status.agent_id.as_str()
            ));
        }
        text.push_str("</subagent>\n");
    }
    text.push_str("</subagent-notification>");
    text
}

fn sanitize_inline_text(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn truncate_utf8_with_flag(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Provider-visible tool name for spawning bounded child agents.
pub(crate) const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";
/// Provider-visible tool name for sending messages to or waking child agents.
pub(crate) const SEND_SUBAGENT_MESSAGE_TOOL_NAME: &str = "send_subagent_message";
/// Provider-visible tool name for waiting on child agent statuses/results.
pub(crate) const WAIT_SUBAGENTS_TOOL_NAME: &str = "wait_subagents";
/// Provider-visible tool name for cancelling child agents.
pub(crate) const CANCEL_SUBAGENTS_TOOL_NAME: &str = "cancel_subagents";
/// Construction-context id for the runtime-derived effective workspace scope.
pub(crate) const SUBAGENT_WORKSPACE_SCOPE_CONTEXT_ID: &str = "subagent-workspace-scope";
/// Construction-context id for the child's preloaded files and expected output handoff.
pub const SUBAGENT_HANDOFF_CONTEXT_ID: &str = "subagent-handoff";

const APPLY_PATCH_TOOL_NAME: &str = "apply_patch";

#[cfg(test)]
mod manager_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
