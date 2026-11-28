use crate::{
    final_output::FINAL_OUTPUT_TOOL_NAME,
    tool::{
        executor::{ToolExecutionContext, ToolExecutionError, ToolExecutor, ToolExecutorFuture},
        proposal::ToolActionKind,
    },
    tool_input_validation::{CompiledToolInputValidator, ToolInputValidationError},
};
use merry_core::{PendingToolCall, ToolName, ToolSpec};
use std::{collections::BTreeMap, sync::Arc};

/// Where a registered tool is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRunner {
    /// Merry/Rust runtime owns execution through a [`ToolExecutor`].
    Runtime,
    /// An external SDK runner executes after Merry emits a bridge event.
    Bridge,
}

/// Runtime execution policy for calls that appear in the same model batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolConcurrency {
    /// Calls may execute concurrently with adjacent parallel-safe calls.
    ParallelSafe,
    /// The call executes alone and acts as a barrier between concurrent waves.
    Exclusive,
}

/// Typed Plan Mode / checklist intrinsic operations owned by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanIntrinsicTool {
    EnterPlanMode,
    WritePlan,
    ReadPlan,
    ExitPlanMode,
    UpdateTodos,
}

/// Typed runtime-intrinsic dispatch target bound at tool registration time.
///
/// Storing the dispatch target on [`RegisteredTool`] eliminates name-based
/// string dispatch inside tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeIntrinsicTool {
    Plan(PlanIntrinsicTool),
    RequestPermissions,
}

/// Proposal preflight and low-risk workspace patch lane policy for a registered tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolActionProposalMode {
    /// Proposal preflight is disabled.
    Disabled,
    /// Runtime requests read-only proposal evidence before mutating execution.
    Enabled,
    /// Proposal preflight is enabled and this tool is eligible for the runtime's
    /// opt-in low-risk workspace patch lane.
    LowRiskWorkspacePatch,
}

/// Runtime-owned registered tool definition.
///
/// A registered tool binds a provider-visible spec to an executor. It does not
/// start or automate a tool loop. [`crate::Runtime::submit_tool_result`] is the
/// external/manual result path; [`crate::Runtime::execute_tool_call`] is the
/// runtime-registered executor path.
#[derive(Clone)]
pub struct RegisteredTool {
    pub(super) spec: ToolSpec,
    pub(super) external_binding: Option<merry_core::ExternalToolBinding>,
    pub(super) executor: Arc<dyn ToolExecutor>,
    pub(super) action_kind: ToolActionKind,
    pub(super) proposal_mode: ToolActionProposalMode,
    pub(super) intrinsic: Option<RuntimeIntrinsicTool>,
    pub(super) runner: ToolRunner,
    pub(super) concurrency: ToolConcurrency,
}

impl RegisteredTool {
    /// Creates a registered tool with an explicit runtime-owned action category.
    ///
    /// The spec is provider-visible after adapter rendering, but the executor
    /// remains a runtime-owned boundary. The action category stays inside
    /// runtime and is not rendered into the provider-visible tool spec.
    #[must_use]
    pub fn new(
        spec: ToolSpec,
        executor: Arc<dyn ToolExecutor>,
        action_kind: ToolActionKind,
    ) -> Self {
        Self {
            spec,
            external_binding: None,
            executor,
            action_kind,
            proposal_mode: ToolActionProposalMode::Disabled,
            intrinsic: None,
            runner: ToolRunner::Runtime,
            concurrency: ToolConcurrency::Exclusive,
        }
    }

    /// Creates a runtime-owned intrinsic control tool bound to its typed
    /// dispatch target.
    ///
    /// Intrinsic tools always carry [`ToolActionKind::RuntimeControl`] and are
    /// intercepted by runtime dispatch before the generic executor path.
    #[must_use]
    pub(crate) fn intrinsic(spec: ToolSpec, intrinsic: RuntimeIntrinsicTool) -> Self {
        Self {
            spec,
            external_binding: None,
            executor: Arc::new(IntrinsicToolExecutor),
            action_kind: ToolActionKind::RuntimeControl,
            proposal_mode: ToolActionProposalMode::Disabled,
            intrinsic: Some(intrinsic),
            runner: ToolRunner::Runtime,
            concurrency: ToolConcurrency::Exclusive,
        }
    }

    /// Attaches a stable adapter binding for session catalog persistence.
    ///
    /// Binding metadata is never rendered into the model's tool definition.
    #[must_use]
    pub fn with_external_binding(mut self, binding: merry_core::ExternalToolBinding) -> Self {
        self.external_binding = Some(binding);
        self
    }

    /// Returns the adapter identity, if this tool belongs to an external catalog.
    #[must_use]
    pub fn external_binding(&self) -> Option<&merry_core::ExternalToolBinding> {
        self.external_binding.as_ref()
    }

    /// Enables read-only proposal evidence for a mutating tool.
    ///
    /// Runtime calls [`ToolExecutor::propose`] only for registered tools that
    /// explicitly opt in here. The hook is still skipped for read-only tools.
    #[must_use]
    pub fn with_action_proposal(mut self) -> Self {
        if self.proposal_mode == ToolActionProposalMode::Disabled {
            self.proposal_mode = ToolActionProposalMode::Enabled;
        }
        self
    }

    /// Opts a [`ToolActionKind::WorkspaceWrite`] tool into the low-risk
    /// workspace patch lane when the runtime enables
    /// [`crate::RuntimeBuilder::allow_low_risk_apply_patches`].
    ///
    /// Automatically enables read-only action proposals so callers cannot
    /// misconfigure the patch lane without proposal preflight.
    #[must_use]
    pub fn with_low_risk_workspace_patch_lane(mut self) -> Self {
        self.proposal_mode = ToolActionProposalMode::LowRiskWorkspacePatch;
        self
    }

    /// Opts this tool into bounded concurrent execution within one model batch.
    ///
    /// Use this only when concurrent calls cannot race through writes,
    /// processes, network access, permissions, or runtime-control state.
    #[must_use]
    pub fn with_parallel_safe_execution(mut self) -> Self {
        self.concurrency = ToolConcurrency::ParallelSafe;
        self
    }

    /// Creates a registered read-only tool.
    ///
    /// Use this only for tools that do not write workspace state or execute
    /// commands. Network access is controlled by runtime profile, not by this
    /// constructor.
    #[must_use]
    pub fn read_only(spec: ToolSpec, executor: Arc<dyn ToolExecutor>) -> Self {
        Self::new(spec, executor, ToolActionKind::ReadOnly)
    }

    /// Creates a tool whose execution is delegated to an external SDK bridge.
    #[must_use]
    pub fn bridge(spec: ToolSpec) -> Self {
        Self {
            spec,
            external_binding: None,
            executor: Arc::new(BridgeExecutor),
            action_kind: ToolActionKind::ReadOnly,
            proposal_mode: ToolActionProposalMode::Disabled,
            intrinsic: None,
            runner: ToolRunner::Bridge,
            concurrency: ToolConcurrency::Exclusive,
        }
    }

    /// Borrows the provider-visible tool specification.
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Returns the runtime-owned action category for this tool.
    #[must_use]
    pub fn action_kind(&self) -> ToolActionKind {
        self.action_kind
    }

    /// Returns where this tool is executed.
    #[must_use]
    pub fn runner(&self) -> ToolRunner {
        self.runner
    }

    /// Returns the runtime execution policy for batched calls.
    #[must_use]
    pub fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    /// Returns whether this tool opted into action proposal evidence.
    #[must_use]
    pub const fn proposals_enabled(&self) -> bool {
        !matches!(self.proposal_mode, ToolActionProposalMode::Disabled)
    }

    /// Returns whether this tool opted into the low-risk workspace patch lane.
    #[must_use]
    pub const fn low_risk_workspace_patch_lane(&self) -> bool {
        matches!(
            self.proposal_mode,
            ToolActionProposalMode::LowRiskWorkspacePatch
        )
    }

    /// Returns the typed proposal policy for this tool.
    #[must_use]
    pub(crate) const fn proposal_mode(&self) -> ToolActionProposalMode {
        self.proposal_mode
    }

    /// Returns the typed runtime-intrinsic dispatch target, if any.
    #[must_use]
    pub(crate) const fn runtime_intrinsic(&self) -> Option<RuntimeIntrinsicTool> {
        self.intrinsic
    }

    pub(crate) fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::clone(&self.executor)
    }
}

struct IntrinsicToolExecutor;

impl ToolExecutor for IntrinsicToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            Err(ToolExecutionError::infrastructure(format!(
                "intrinsic runtime tool {} reached the generic executor",
                call.name().as_str()
            )))
        })
    }
}

pub(super) struct BridgeExecutor;

impl ToolExecutor for BridgeExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            Err(ToolExecutionError::infrastructure(format!(
                "bridge tool {} must be executed by a bridge runner",
                call.name().as_str()
            )))
        })
    }
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("spec", &self.spec)
            .field("action_kind", &self.action_kind)
            .field("proposal_mode", &self.proposal_mode)
            .field("intrinsic", &self.intrinsic)
            .field("runner", &self.runner)
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolRegistry {
    pub(super) tools: BTreeMap<ToolName, RegisteredToolEntry>,
    pub(super) order: Vec<ToolName>,
}

#[derive(Debug, Clone)]
pub(super) struct RegisteredToolEntry {
    pub(super) tool: RegisteredTool,
    pub(super) input_validator: CompiledToolInputValidator,
}

impl ToolRegistry {
    pub(crate) fn from_registered(tools: Vec<RegisteredTool>) -> Result<Self, ToolRegistryError> {
        let mut registry = BTreeMap::new();
        let mut order = Vec::with_capacity(tools.len());

        for tool in tools {
            let name = tool.spec().name().clone();
            if name.as_str() == FINAL_OUTPUT_TOOL_NAME {
                return Err(ToolRegistryError::ReservedName { name });
            }
            let input_validator = CompiledToolInputValidator::compile(tool.spec().input_schema())
                .map_err(|source| ToolRegistryError::InvalidToolInputSchema {
                name: name.clone(),
                message: source.to_string(),
            })?;
            let entry = RegisteredToolEntry {
                tool,
                input_validator,
            };
            if registry.insert(name.clone(), entry).is_some() {
                return Err(ToolRegistryError::DuplicateName { name });
            }
            order.push(name);
        }

        Ok(Self {
            tools: registry,
            order,
        })
    }

    pub(crate) fn tool_specs(&self) -> Vec<ToolSpec> {
        self.order
            .iter()
            .filter_map(|name| self.tools.get(name))
            .map(|entry| entry.tool.spec().clone())
            .collect()
    }

    pub(crate) fn external_tool_catalog(
        &self,
    ) -> Result<merry_core::SessionToolCatalog, merry_core::CoreError> {
        merry_core::SessionToolCatalog::new(
            self.order
                .iter()
                .filter_map(|name| {
                    let tool = &self.tools.get(name)?.tool;
                    Some(merry_core::SessionToolCatalogEntry::new(
                        tool.spec().clone(),
                        tool.external_binding()?.clone(),
                    ))
                })
                .collect(),
        )
    }

    pub(crate) fn registered_tool(&self, name: &ToolName) -> Option<&RegisteredTool> {
        self.tools.get(name).map(|entry| &entry.tool)
    }

    pub(crate) fn validate_tool_input(
        &self,
        call: &PendingToolCall,
    ) -> Option<Result<(), ToolInputValidationError>> {
        self.tools
            .get(call.name())
            .map(|entry| entry.input_validator.validate_call(call))
    }

    pub(crate) fn first_bridge_tool_name(&self) -> Option<&ToolName> {
        self.tools
            .iter()
            .find_map(|(name, entry)| (entry.tool.runner() == ToolRunner::Bridge).then_some(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolRegistryError {
    DuplicateName { name: ToolName },
    ReservedName { name: ToolName },
    InvalidToolInputSchema { name: ToolName, message: String },
}
