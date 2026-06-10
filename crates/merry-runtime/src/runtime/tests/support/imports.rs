    use super::{
        AutomaticCompactionConfig, DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED, Runtime, RuntimeBuilder, RuntimeInner,
        TOOL_ACTION_POLICY_DENIED_MESSAGE, WORKSPACE_PATCH_TOOL_NAME,
        admit_action_to_generic_executor, memory_activation_seed_from_step_input,
        request_context_budget, send_cancelled_event,
    };
    use crate::action_audit::ActionAuditStatus;
    use crate::action_policy::{
        ActionPolicyDecision, ActionPolicyDisposition, ActionRiskTier, DefaultActionPolicy,
    };
    use crate::artifact::ArtifactContent;
    use crate::judgment::{
        JudgmentConfidence, JudgmentContext, JudgmentError, JudgmentEvidence, JudgmentFuture,
        JudgmentOutcome, JudgmentProvenance, JudgmentPurpose, JudgmentRecommendation,
        JudgmentRecord, JudgmentRiskLevel, JudgmentSource, JudgmentSourceKind,
        ModelBackedJudgmentSource,
    };
    use crate::ledger::{LedgerFactKind, LedgerProjection, LedgerScope};
    use crate::memory::{
        ActivatedMemory, MemoryActivationContext, MemoryActivationFuture, MemoryActivationReason,
        MemoryActivationScore, MemoryActivationSource, MemoryActivationSourceKind, MemoryError,
        MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection, MemoryScope,
    };
    use crate::model_config::RuntimeModelConfigs;
    use crate::process::{
        AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory,
        ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence, ProcessExitStatus,
        ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
        ProcessRunnerFuture, ProcessRunnerOutput, stable_process_input_fingerprint,
    };
    use crate::session::SessionState;
    use crate::tool::{
        ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, RegisteredTool,
        ToolActionKind, ToolActionPreflight, ToolActionProposalFuture, ToolExecutionContext,
        ToolExecutionError, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture, ToolRegistry,
        WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    };
    use crate::{
        ArtifactError, CheckpointDecision, CitationCompactionPolicy, ContextBudgetPolicy,
        PermissionReviewMode, RuntimeError, RuntimeModelRole, RuntimeTrustLevel, StepContext,
        request_permissions_tool,
    };
    use futures_util::StreamExt;
    use merry_core::{
        ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, PendingToolCall,
        RuntimeJournalEvent, RuntimeJournalPayload, SessionId, ToolCallArguments, ToolCallId,
        ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
    };
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelError, ModelEvent,
        ModelEventStream, ModelMessage, ModelMessageRole, ModelName, ModelOutput, ModelProvider,
        ModelProviderFuture, ModelRequest, ModelResponse, ModelRetryPolicy, ModelStreamContext,
        ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    };
    use schemars::Schema;
    use serde_json::json;
    use std::{
        future::Future,
        num::NonZeroUsize,
        sync::{
            Arc, Mutex as StdMutex, OnceLock,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
    };
    use tokio::sync::{Mutex, mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
