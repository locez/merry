//! Pure, typed subagent role catalog and 4-level dynamic runtime resolver.
//!
//! `merry-coding` owns built-in coding roles (`explore`, `coder`, `review`),
//! capability intersection, orphaned tool pruning, and dynamic model/generation
//! inheritance so `merry-runtime` stays free of coding-domain roles.

use crate::policy_prompt::builtin_subagent_role_instructions;
use merry_core::ToolName;
use merry_llm::{GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy, ReasoningEffort};
use merry_runtime::{ChildWorkspaceScope, ParentPrimaryModel, SubagentTaskSpec};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::Arc,
};
use thiserror::Error;

/// Maximum UTF-8 bytes allowed for one role prompt file.
pub const MAX_ROLE_PROMPT_FILE_BYTES: usize = 32 * 1024;

/// Orthogonal OS sandbox workspace access mode for a subagent role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentWorkspaceAccess {
    /// OS-enforced read-only workspace (e.g. `explore`).
    ReadOnlyWorkspace,
    /// Full read-write workspace (e.g. `coder`, `review`).
    ReadWriteWorkspace,
}

/// Optional concrete provider/model override attached to `[subagents.default]`
/// or `[subagents.roles.<name>]`.
#[derive(Clone)]
pub struct SubagentModelOverride {
    /// Resolved provider implementation.
    pub provider: Arc<dyn ModelProvider>,
    /// Target model identifier.
    pub model: ModelName,
    /// Optional provider retry policy override.
    pub retry_policy: Option<ModelRetryPolicy>,
}

/// Global `[subagents.default]` overrides applied below role overrides and
/// above live parent runtime inheritance.
#[derive(Clone, Default)]
pub struct SubagentDefaultOverride {
    /// Optional default provider/model for all subagents.
    pub model_override: Option<SubagentModelOverride>,
    /// Optional default reasoning effort for all subagents.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Optional default model-turn budget for subagents.
    pub max_model_turns: Option<u32>,
    /// Optional default wall-clock timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Optional cumulative per-child token circuit breaker.
    pub max_total_tokens: Option<u64>,
}

/// Resolved definition for one subagent role in [`SubagentRoleCatalog`].
#[derive(Clone)]
pub struct SubagentRoleSpec {
    /// Stable role identifier (e.g. `explore`, `coder`, `review`, `browser`).
    pub name: String,
    /// Short one-line description advertised in `spawn_subagents`.
    pub description: String,
    /// Whether this role is currently enabled.
    pub enabled: bool,
    /// Orthogonal sandbox access mode for this role.
    pub workspace_access: SubagentWorkspaceAccess,
    /// Optional allowlist of tools for this role; `None` inherits parent tools.
    pub default_allowed_tools: Option<Vec<ToolName>>,
    /// Default model-turn budget when neither task nor `[subagents.default]` overrides it.
    pub default_max_model_turns: u32,
    /// Explicit role-level `max_model_turns` overlay (takes precedence over `[subagents.default]`).
    pub max_model_turns_override: Option<u32>,
    /// Default wall-clock timeout in seconds when omitted.
    pub default_timeout_secs: Option<u64>,
    /// Optional cumulative per-child token circuit breaker.
    pub max_total_tokens: Option<u64>,
    /// Role-specific prompt instructions rendered into `<merry_subagent_role>`.
    pub instructions: Option<String>,
    /// Optional provider/model override for this role.
    pub model_override: Option<SubagentModelOverride>,
    /// Optional reasoning-effort override for this role.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// User- or project-supplied overlay for defining or customizing a role.
#[derive(Clone, Default)]
pub struct SubagentRoleOverlay {
    /// Optional base role to inherit defaults from (`explore`, `coder`, `review`).
    pub base: Option<String>,
    /// Optional enabled toggle.
    pub enabled: Option<bool>,
    /// Optional one-line description override.
    pub description: Option<String>,
    /// Optional workspace access mode override.
    pub workspace_access: Option<SubagentWorkspaceAccess>,
    /// Optional provider/model override.
    pub model_override: Option<SubagentModelOverride>,
    /// Optional reasoning-effort override.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Optional default turn budget override.
    pub max_model_turns: Option<u32>,
    /// Optional default wall-clock timeout override.
    pub timeout_secs: Option<u64>,
    /// Optional token circuit breaker override.
    pub max_total_tokens: Option<u64>,
    /// Optional tool allowlist override.
    pub allowed_tools: Option<Vec<ToolName>>,
    /// Optional custom instructions (already combined with any loaded `prompt_file`).
    pub instructions: Option<String>,
}

/// Errors raised while validating or resolving subagent roles.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SubagentRoleError {
    /// Role identifier is empty or contains unsupported characters.
    #[error("invalid subagent role name {role:?}: use ASCII letters, digits, '_' or '-'")]
    InvalidRoleName { role: String },
    /// Referenced base role does not exist.
    #[error("unknown base role {base:?} for subagent role {role:?}")]
    UnknownBaseRole { role: String, base: String },
    /// Spawn requested an unknown role.
    #[error("unknown subagent role {role:?}; available roles: {available}")]
    UnknownRole { role: String, available: String },
    /// Spawn requested a role that is disabled via config or toggle.
    #[error("subagent role {role:?} is disabled")]
    DisabledRole { role: String },
    /// Role turn budget or timeout was zero.
    #[error("invalid {field} for subagent role {role:?}: must be greater than zero")]
    ZeroBudget { role: String, field: &'static str },
    /// Prompt file could not be loaded safely from the workspace.
    #[error("failed to load prompt_file {path:?} for subagent role {role:?}: {reason}")]
    InvalidPromptFile {
        role: String,
        path: String,
        reason: String,
    },
}

/// Validated catalog of built-in and configured subagent roles plus global defaults.
#[derive(Clone)]
pub struct SubagentRoleCatalog {
    default_override: SubagentDefaultOverride,
    roles: BTreeMap<String, SubagentRoleSpec>,
}

impl Default for SubagentRoleCatalog {
    fn default() -> Self {
        Self::builtin()
    }
}

impl SubagentRoleCatalog {
    /// Builds the default catalog containing Merry's 3 built-in roles:
    /// `explore`, `coder`, and `review`.
    #[must_use]
    pub fn builtin() -> Self {
        let mut roles = BTreeMap::new();
        roles.insert(
            "explore".to_owned(),
            SubagentRoleSpec {
                name: "explore".to_owned(),
                description: "fast read-only codebase researcher".to_owned(),
                enabled: true,
                workspace_access: SubagentWorkspaceAccess::ReadOnlyWorkspace,
                default_allowed_tools: Some(tool_names(&["read_text", "run_process", "web_fetch"])),
                default_max_model_turns: 30,
                max_model_turns_override: None,
                default_timeout_secs: Some(180),
                max_total_tokens: None,
                instructions: builtin_subagent_role_instructions("explore").map(str::to_owned),
                model_override: None,
                reasoning_effort: None,
            },
        );
        roles.insert(
            "coder".to_owned(),
            SubagentRoleSpec {
                name: "coder".to_owned(),
                description: "scoped implementation and local verification".to_owned(),
                enabled: true,
                workspace_access: SubagentWorkspaceAccess::ReadWriteWorkspace,
                default_allowed_tools: None,
                default_max_model_turns: 80,
                max_model_turns_override: None,
                default_timeout_secs: Some(600),
                max_total_tokens: None,
                instructions: builtin_subagent_role_instructions("coder").map(str::to_owned),
                model_override: None,
                reasoning_effort: None,
            },
        );
        roles.insert(
            "review".to_owned(),
            SubagentRoleSpec {
                name: "review".to_owned(),
                description: "independent skeptical code review and verification".to_owned(),
                enabled: true,
                workspace_access: SubagentWorkspaceAccess::ReadWriteWorkspace,
                default_allowed_tools: Some(tool_names(&[
                    "read_text",
                    "run_process",
                    "request_permissions",
                    "web_fetch",
                    "browser_tab",
                    "browser_network",
                    "read_background_process",
                    "stop_background_process",
                ])),
                default_max_model_turns: 40,
                max_model_turns_override: None,
                default_timeout_secs: Some(300),
                max_total_tokens: None,
                instructions: builtin_subagent_role_instructions("review").map(str::to_owned),
                model_override: None,
                reasoning_effort: None,
            },
        );
        Self {
            default_override: SubagentDefaultOverride::default(),
            roles,
        }
    }

    /// Replaces the `[subagents.default]` override layer.
    #[must_use]
    pub fn with_default_override(mut self, default_override: SubagentDefaultOverride) -> Self {
        self.default_override = default_override;
        self
    }

    /// Returns the configured `[subagents.default]` override layer.
    #[must_use]
    pub fn default_override(&self) -> &SubagentDefaultOverride {
        &self.default_override
    }

    /// Applies one role overlay on top of an existing role or its declared `base`
    /// role (defaulting to `coder` for new custom roles).
    pub fn apply_role_overlay(
        &mut self,
        name: &str,
        overlay: SubagentRoleOverlay,
    ) -> Result<(), SubagentRoleError> {
        validate_role_name(name)?;
        if overlay.max_model_turns == Some(0) {
            return Err(SubagentRoleError::ZeroBudget {
                role: name.to_owned(),
                field: "max_model_turns",
            });
        }
        if overlay.timeout_secs == Some(0) {
            return Err(SubagentRoleError::ZeroBudget {
                role: name.to_owned(),
                field: "timeout_secs",
            });
        }
        if overlay.max_total_tokens == Some(0) {
            return Err(SubagentRoleError::ZeroBudget {
                role: name.to_owned(),
                field: "max_total_tokens",
            });
        }

        let mut spec = if let Some(base_name) = overlay.base.as_deref() {
            let mut inherited = self.roles.get(base_name).cloned().ok_or_else(|| {
                SubagentRoleError::UnknownBaseRole {
                    role: name.to_owned(),
                    base: base_name.to_owned(),
                }
            })?;
            inherited.name = name.to_owned();
            inherited
        } else if let Some(existing) = self.roles.get(name) {
            existing.clone()
        } else {
            SubagentRoleSpec {
                name: name.to_owned(),
                description: format!("custom {name} specialist"),
                enabled: true,
                workspace_access: SubagentWorkspaceAccess::ReadWriteWorkspace,
                default_allowed_tools: None,
                default_max_model_turns: 80,
                max_model_turns_override: None,
                default_timeout_secs: Some(600),
                max_total_tokens: None,
                instructions: None,
                model_override: None,
                reasoning_effort: None,
            }
        };

        if let Some(enabled) = overlay.enabled {
            spec.enabled = enabled;
        }
        if let Some(description) = overlay.description {
            spec.description = description;
        }
        if let Some(access) = overlay.workspace_access {
            spec.workspace_access = access;
        }
        if let Some(model_override) = overlay.model_override {
            spec.model_override = Some(model_override);
        }
        if let Some(reasoning_effort) = overlay.reasoning_effort {
            spec.reasoning_effort = Some(reasoning_effort);
        }
        if let Some(turns) = overlay.max_model_turns {
            spec.default_max_model_turns = turns;
            spec.max_model_turns_override = Some(turns);
        }
        if let Some(timeout) = overlay.timeout_secs {
            spec.default_timeout_secs = Some(timeout);
        }
        if let Some(tokens) = overlay.max_total_tokens {
            spec.max_total_tokens = Some(tokens);
        }
        if let Some(tools) = overlay.allowed_tools {
            spec.default_allowed_tools = Some(tools);
        }
        if let Some(instructions) = overlay.instructions {
            spec.instructions = Some(instructions);
        }

        self.roles.insert(name.to_owned(), spec);
        Ok(())
    }

    /// Applies `[subagents.toggle]` flags for known roles.
    pub fn apply_toggles(
        &mut self,
        toggles: &BTreeMap<String, bool>,
    ) -> Result<(), SubagentRoleError> {
        for (name, enabled) in toggles {
            let Some(spec) = self.roles.get_mut(name) else {
                return Err(SubagentRoleError::UnknownRole {
                    role: name.clone(),
                    available: self.available_role_list(),
                });
            };
            spec.enabled = *enabled;
        }
        Ok(())
    }

    /// Looks up a role by name.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&SubagentRoleSpec> {
        self.roles.get(name)
    }

    /// Returns `(name, description)` summaries for all enabled roles in deterministic order.
    #[must_use]
    pub fn enabled_role_summaries(&self) -> Vec<(String, String)> {
        self.roles
            .values()
            .filter(|spec| spec.enabled)
            .map(|spec| (spec.name.clone(), spec.description.clone()))
            .collect()
    }

    fn available_role_list(&self) -> String {
        self.roles
            .values()
            .filter(|spec| spec.enabled)
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Inputs to the pure [`resolve_subagent_runtime`] resolver.
pub struct SubagentResolutionInput<'a> {
    /// Delegated child task contract.
    pub task: &'a SubagentTaskSpec,
    /// Tools already bounded by the parent's admitted catalog.
    pub parent_allowed_tools: &'a [ToolName],
    /// Child workspace scope validated against the parent.
    pub workspace_scope: &'a ChildWorkspaceScope,
    /// Whether the parent workspace itself is writable.
    pub parent_workspace_writable: bool,
    /// Delegation depth assigned to this child (`parent.depth + 1`).
    pub depth: u8,
    /// Maximum allowed delegation depth.
    pub max_depth: u8,
    /// Maximum configured child model-turn cap.
    pub max_model_turns_cap: u32,
    /// Live snapshot of the parent's currently active primary provider/model.
    pub parent_primary_model: Option<&'a ParentPrimaryModel>,
    /// Fallback provider/model from initial composition when no live snapshot exists.
    pub fallback_provider: Arc<dyn ModelProvider>,
    /// Fallback model identifier from initial composition.
    pub fallback_model: &'a ModelName,
    /// Live parent generation config (including `reasoning_effort` & `service_tier`).
    pub parent_generation_config: &'a GenerationConfig,
}

/// Deterministic, fully resolved runtime specification for one child agent.
pub struct ResolvedSubagentRuntime {
    /// Effective resolved role name.
    pub role_name: String,
    /// Resolved provider handle.
    pub provider: Arc<dyn ModelProvider>,
    /// Resolved model identifier.
    pub model: ModelName,
    /// Optional provider retry policy override.
    pub retry_policy: Option<ModelRetryPolicy>,
    /// Resolved generation config (4-level inheritance).
    pub generation_config: GenerationConfig,
    /// Effective OS sandbox writability (capability intersection).
    pub workspace_writable: bool,
    /// Effective allowed tools after capability intersection and orphan pruning.
    pub allowed_tools: Vec<ToolName>,
    /// Effective model-turn budget.
    pub max_model_turns: u32,
    /// Effective wall-clock timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Effective cumulative per-child token circuit breaker.
    pub max_total_tokens: Option<u64>,
    /// Effective role prompt instructions for `<merry_subagent_role>`.
    pub role_instructions: Option<String>,
}

/// Pure function resolving role, capability intersection, orphaned tool
/// pruning, and 4-level dynamic model/generation inheritance for one child.
pub fn resolve_subagent_runtime(
    catalog: &SubagentRoleCatalog,
    input: &SubagentResolutionInput<'_>,
) -> Result<ResolvedSubagentRuntime, SubagentRoleError> {
    let requested_role = input.task.role().unwrap_or("coder");
    let Some(role) = catalog.role(requested_role) else {
        return Err(SubagentRoleError::UnknownRole {
            role: requested_role.to_owned(),
            available: catalog.available_role_list(),
        });
    };
    if !role.enabled {
        return Err(SubagentRoleError::DisabledRole {
            role: role.name.clone(),
        });
    }

    // 1. Workspace writability = strict capability intersection.
    let task_forbids_writes =
        input.task.write_scope_is_explicit() && input.workspace_scope.write_scope().is_empty();
    let workspace_writable = input.parent_workspace_writable
        && role.workspace_access == SubagentWorkspaceAccess::ReadWriteWorkspace
        && !task_forbids_writes;

    // 2. Allowed tools = capability intersection + orphaned tool pruning.
    let mut allowed_tools = if input.task.allowed_tools_are_explicit() {
        input.parent_allowed_tools.to_vec()
    } else if let Some(role_tools) = role.default_allowed_tools.as_deref() {
        let role_set: BTreeSet<&str> = role_tools.iter().map(ToolName::as_str).collect();
        input
            .parent_allowed_tools
            .iter()
            .filter(|tool| role_set.contains(tool.as_str()))
            .cloned()
            .collect()
    } else {
        input.parent_allowed_tools.to_vec()
    };

    if !workspace_writable {
        allowed_tools.retain(|tool| tool.as_str() != "apply_patch");
    }
    allowed_tools = prune_orphaned_tools(allowed_tools, input.depth, input.max_depth);

    // 3. 4-level dynamic model & provider routing:
    //    Level 2: role.model_override
    //    Level 3: catalog.default_override.model_override
    //    Level 4: live parent_primary_model (or initial fallback)
    let (provider, model, retry_policy) = if let Some(override_cfg) = role.model_override.as_ref() {
        (
            Arc::clone(&override_cfg.provider),
            override_cfg.model.clone(),
            override_cfg.retry_policy,
        )
    } else if let Some(default_cfg) = catalog.default_override.model_override.as_ref() {
        (
            Arc::clone(&default_cfg.provider),
            default_cfg.model.clone(),
            default_cfg.retry_policy,
        )
    } else if let Some(live) = input.parent_primary_model {
        (
            Arc::clone(&live.provider),
            live.model.clone(),
            Some(live.retry_policy),
        )
    } else {
        (
            Arc::clone(&input.fallback_provider),
            input.fallback_model.clone(),
            None,
        )
    };

    // 4. 4-level dynamic GenerationConfig inheritance:
    //    Base: live parent_generation_config (Level 4)
    //    Overlay reasoning_effort: default (Level 3) -> role (Level 2) -> task (Level 1)
    let mut generation_config = input.parent_generation_config.clone();
    if let Some(effort) = catalog.default_override.reasoning_effort.as_ref() {
        generation_config = generation_config.with_reasoning_effort(Some(effort.clone()));
    }
    if let Some(effort) = role.reasoning_effort.as_ref() {
        generation_config = generation_config.with_reasoning_effort(Some(effort.clone()));
    }
    if let Some(effort) = input.task.reasoning_effort() {
        generation_config = generation_config.with_reasoning_effort(Some(effort.clone()));
    }

    // 5. Effective turn budget, timeout, and token circuit breaker:
    //    task explicit -> role explicit overlay -> [subagents.default] -> built-in role default.
    let max_model_turns = if input.task.max_model_turns_is_explicit() {
        input.task.max_model_turns()
    } else {
        role.max_model_turns_override
            .or(catalog.default_override.max_model_turns)
            .unwrap_or(role.default_max_model_turns)
            .min(input.max_model_turns_cap.max(1))
    };
    let timeout_secs = input
        .task
        .timeout_secs()
        .or(role.default_timeout_secs)
        .or(catalog.default_override.timeout_secs);
    let max_total_tokens = input
        .task
        .max_total_tokens()
        .or(role.max_total_tokens)
        .or(catalog.default_override.max_total_tokens);

    Ok(ResolvedSubagentRuntime {
        role_name: role.name.clone(),
        provider,
        model,
        retry_policy,
        generation_config,
        workspace_writable,
        allowed_tools,
        max_model_turns,
        timeout_secs,
        max_total_tokens,
        role_instructions: role.instructions.clone(),
    })
}

/// Removes control tools whose backing capability is absent from the child.
fn prune_orphaned_tools(mut tools: Vec<ToolName>, depth: u8, max_depth: u8) -> Vec<ToolName> {
    let can_spawn = depth < max_depth && tools.iter().any(|t| t.as_str() == "spawn_subagents");
    if !can_spawn {
        tools.retain(|tool| {
            !matches!(
                tool.as_str(),
                "spawn_subagents" | "send_subagent_message" | "wait_subagents" | "cancel_subagents"
            )
        });
    }
    let can_run_process = tools.iter().any(|t| t.as_str() == "run_process");
    if !can_run_process {
        tools.retain(|tool| {
            !matches!(
                tool.as_str(),
                "read_background_process" | "stop_background_process"
            )
        });
    }
    tools
}

/// Loads a workspace-relative `prompt_file` safely, enforcing containment under
/// `workspace_root` and [`MAX_ROLE_PROMPT_FILE_BYTES`].
pub fn load_workspace_prompt_file(
    workspace_root: &Path,
    role: &str,
    relative_path: &str,
) -> Result<String, SubagentRoleError> {
    let trimmed = relative_path.trim();
    let rel = Path::new(trimmed);
    if trimmed.is_empty()
        || rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: "path must be workspace-relative without parent traversal".to_owned(),
        });
    }
    let full_path = workspace_root.join(rel);
    let canonical_root =
        fs::canonicalize(workspace_root).map_err(|error| SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: error.to_string(),
        })?;
    let canonical_file =
        fs::canonicalize(&full_path).map_err(|error| SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: error.to_string(),
        })?;
    if !canonical_file.starts_with(&canonical_root) {
        return Err(SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: "prompt_file symlink escapes workspace root".to_owned(),
        });
    }
    let bytes =
        fs::read(&canonical_file).map_err(|error| SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: error.to_string(),
        })?;
    if bytes.len() > MAX_ROLE_PROMPT_FILE_BYTES {
        return Err(SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: format!("exceeds {MAX_ROLE_PROMPT_FILE_BYTES} bytes"),
        });
    }
    let text = String::from_utf8(bytes).map_err(|error| SubagentRoleError::InvalidPromptFile {
        role: role.to_owned(),
        path: relative_path.to_owned(),
        reason: error.to_string(),
    })?;
    if text.trim().is_empty() {
        return Err(SubagentRoleError::InvalidPromptFile {
            role: role.to_owned(),
            path: relative_path.to_owned(),
            reason: "prompt_file must not be blank".to_owned(),
        });
    }
    Ok(text)
}

fn validate_role_name(name: &str) -> Result<(), SubagentRoleError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SubagentRoleError::InvalidRoleName {
            role: name.to_owned(),
        });
    }
    Ok(())
}

fn tool_names(names: &[&str]) -> Vec<ToolName> {
    names
        .iter()
        .map(|name| ToolName::new(name).expect("static built-in tool name is valid"))
        .collect()
}

#[cfg(test)]
mod tests;
