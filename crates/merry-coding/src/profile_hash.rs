//! Stable identity of a coding-agent composition profile.
//!
//! The hash answers one question: would two runs send the same provider-visible
//! prefix and the same tool contract? Everything that shapes that prefix is
//! folded into one byte string in a fixed field order, so a change to prompt
//! text, layouts, retry policy, workspace roots, context summaries, project
//! rules, skills, or any registered tool specification produces a different
//! identity. Dynamic context is deliberately excluded, because a task anchor or
//! checkpoint must not look like a different profile.
//!
//! Field names and their order are part of the contract: they make two
//! different values distinguishable (`ab` + `c` hashes differently from `a` +
//! `bc`), and reordering or renaming a field is a compatibility change.

use std::fmt;

use merry_runtime::{RuntimeProfile, ToolActionKind, ToolConcurrency, ToolRunner};
use serde_json::Error as JsonError;

use crate::{
    CODING_AGENT_DYNAMIC_CONTEXT_LAYOUT, CODING_AGENT_PROFILE_ID,
    CODING_AGENT_STABLE_PREFIX_LAYOUT, CodingAgentRunPolicy,
};

/// Stable identity of a shared coding-agent composition profile.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodingAgentProfileHash(String);

impl CodingAgentProfileHash {
    /// Borrows the stable profile hash label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CodingAgentProfileHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Hashes every provider-visible part of one coding-agent composition.
///
/// `workspace_hash_material` is produced by the workspace profile builder,
/// which owns the workspace fields and their own field names. Failures come
/// from serializing a tool specification, which is the only value here that
/// cannot be folded in as bytes directly.
pub(crate) fn coding_agent_profile_hash(
    profile: &RuntimeProfile,
    run_policy: CodingAgentRunPolicy,
    workspace_hash_material: &[u8],
) -> Result<CodingAgentProfileHash, JsonError> {
    let mut material = Vec::new();
    append_hash_field(&mut material, "profile-id", CODING_AGENT_PROFILE_ID);
    material.extend_from_slice(workspace_hash_material);
    append_hash_field(
        &mut material,
        "stable-layout",
        CODING_AGENT_STABLE_PREFIX_LAYOUT,
    );
    append_hash_field(
        &mut material,
        "dynamic-layout",
        CODING_AGENT_DYNAMIC_CONTEXT_LAYOUT,
    );
    append_hash_field(
        &mut material,
        "prompt-base",
        profile.prompt_profile().base_instructions(),
    );
    append_hash_field(
        &mut material,
        "prompt-progress",
        profile.prompt_profile().progress_commentary_instructions(),
    );
    for block in profile.prompt_profile().stable_blocks() {
        append_hash_field(&mut material, "prompt-block-tag", block.tag());
        append_hash_field(&mut material, "prompt-block-text", block.text());
    }
    append_hash_field(
        &mut material,
        "run-max-model-turns",
        &run_policy.max_model_turns().to_string(),
    );
    append_hash_field(
        &mut material,
        "run-final-report",
        run_policy.final_report().as_str(),
    );
    append_model_retry_policy(&mut material, profile.model_retry_policy().as_ref());
    append_hash_field(
        &mut material,
        "progress-commentary",
        on_or_off(profile.progress_commentary()),
    );
    append_hash_field(
        &mut material,
        "bridge-tools",
        on_or_off(profile.allow_bridge_tools()),
    );
    append_hash_field(
        &mut material,
        "workspace-patches",
        on_or_off(profile.allow_low_risk_apply_patches()),
    );
    append_hash_field(
        &mut material,
        "low-risk-process",
        on_or_off(profile.low_risk_process_runner().is_some()),
    );
    append_hash_field(
        &mut material,
        "read-only-process",
        on_or_off(profile.read_only_shell_process_runner().is_some()),
    );
    append_hash_field(
        &mut material,
        "accepted-process",
        on_or_off(profile.accepted_local_workspace_process_runner().is_some()),
    );
    append_hash_field(
        &mut material,
        "permissioned-process",
        on_or_off(profile.permissioned_process_runner_factory().is_some()),
    );

    for (id, text) in profile.initial_context_summaries() {
        append_hash_field(&mut material, "initial-context-id", id);
        append_hash_field(&mut material, "initial-context-text", text);
    }
    if let Some(project_rules) = profile.project_rules() {
        append_hash_field(
            &mut material,
            "project-rules-source",
            project_rules.source_path(),
        );
        append_hash_field(
            &mut material,
            "project-rules-hash",
            project_rules.content_hash(),
        );
        append_hash_field(
            &mut material,
            "project-rules-stable-text",
            &project_rules.to_stable_prefix_message_text(),
        );
    }
    if let Some(skill_catalog) = profile.skill_catalog()
        && let Some(text) = skill_catalog.to_stable_prefix_message_text()
    {
        append_hash_field(&mut material, "skill-catalog", &text);
    }

    // Task anchors and checkpoints are intentionally excluded: they are dynamic
    // runtime context and must not invalidate the stable profile identity.
    for tool in profile.registered_tools() {
        let spec = serde_json::to_string(tool.spec())?;
        append_hash_field(&mut material, "tool-spec", &spec);
        append_hash_field(
            &mut material,
            "tool-action-kind",
            tool_action_kind_label(tool.action_kind()),
        );
        append_hash_field(
            &mut material,
            "tool-runner",
            tool_runner_label(tool.runner()),
        );
        append_hash_field(
            &mut material,
            "tool-concurrency",
            tool_concurrency_label(tool.concurrency()),
        );
        append_hash_field(
            &mut material,
            "tool-proposals",
            on_or_off(tool.proposals_enabled()),
        );
    }

    Ok(CodingAgentProfileHash(format!(
        "fnv1a64:{:016x}",
        fnv1a64(&material)
    )))
}

/// Folds the retry policy in, or records that the runtime default applies.
fn append_model_retry_policy(
    material: &mut Vec<u8>,
    retry_policy: Option<&merry_llm::ModelRetryPolicy>,
) {
    let Some(retry_policy) = retry_policy else {
        append_hash_field(material, "retry-policy", "runtime-default");
        return;
    };
    append_hash_field(material, "retry-enabled", on_or_off(retry_policy.enabled()));
    append_hash_field(
        material,
        "retry-max-attempts",
        &retry_policy.max_attempts().to_string(),
    );
    append_hash_field(
        material,
        "retry-initial-delay-nanos",
        &retry_policy.initial_delay().as_nanos().to_string(),
    );
    append_hash_field(
        material,
        "retry-max-delay-nanos",
        &retry_policy.max_delay().as_nanos().to_string(),
    );
    append_hash_field(
        material,
        "retry-max-elapsed-nanos",
        &retry_policy.max_elapsed().as_nanos().to_string(),
    );
    append_hash_field(material, "retry-jitter", on_or_off(retry_policy.jitter()));
}

/// Appends one length-prefixed, name-tagged field.
///
/// The name keeps two adjacent fields from colliding, and the length keeps a
/// value from absorbing the next field's name.
fn append_hash_field(material: &mut Vec<u8>, name: &str, value: &str) {
    material.extend_from_slice(name.as_bytes());
    material.push(0);
    material.extend_from_slice(&(value.len() as u64).to_be_bytes());
    material.extend_from_slice(value.as_bytes());
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn on_or_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn tool_action_kind_label(kind: ToolActionKind) -> &'static str {
    match kind {
        ToolActionKind::ReadOnly => "read_only",
        ToolActionKind::RuntimeControl => "runtime_control",
        ToolActionKind::WorkspaceWrite => "workspace_write",
        ToolActionKind::CommandExec => "command_exec",
        ToolActionKind::Network => "network",
        ToolActionKind::TrustedExternal => "trusted_external",
    }
}

fn tool_runner_label(runner: ToolRunner) -> &'static str {
    match runner {
        ToolRunner::Runtime => "runtime",
        ToolRunner::Bridge => "bridge",
    }
}

fn tool_concurrency_label(concurrency: ToolConcurrency) -> &'static str {
    match concurrency {
        ToolConcurrency::ParallelSafe => "parallel_safe",
        ToolConcurrency::Exclusive => "exclusive",
    }
}
