//! Configuration for the runtime-owned agent loop.

use crate::FinalOutputContract;
use std::num::NonZeroUsize;
use thiserror::Error;

/// Generic SDK/runtime default for one top-level agent run.
pub const DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS: usize = 128;

/// Retry policy for application-level structured final-output decoding.
///
/// A retry is another model continuation in the same runtime session. The
/// failed final-output call is recorded as a failed tool result before the
/// continuation is started, so the model receives an actionable failure and
/// the session remains resume-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredOutputRetryPolicy {
    max_retries: usize,
}

impl StructuredOutputRetryPolicy {
    /// Creates a policy with the supplied number of retries after the first
    /// structured-output attempt.
    #[must_use]
    pub const fn new(max_retries: usize) -> Self {
        Self { max_retries }
    }

    /// Returns a policy that does not retry a failed structured output.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::new(0)
    }

    /// Returns the maximum number of retries after the first attempt.
    #[must_use]
    pub const fn max_retries(self) -> usize {
        self.max_retries
    }
}

impl Default for StructuredOutputRetryPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Configuration for [`Runtime::run_agent_loop`].
///
/// `max_model_turns` bounds the number of model turns started by one loop run.
/// Context compaction may happen within the run, but it does not reset this
/// control-flow and cost budget.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLoopConfig {
    max_model_turns: NonZeroUsize,
    final_output_contract: Option<FinalOutputContract>,
    structured_output_retry_policy: StructuredOutputRetryPolicy,
}

impl AgentLoopConfig {
    /// Creates loop configuration with a non-zero model-turn budget.
    pub fn new(max_model_turns: usize) -> Result<Self, AgentLoopConfigError> {
        let Some(max_model_turns) = NonZeroUsize::new(max_model_turns) else {
            return Err(AgentLoopConfigError::MaxModelTurnsMustBeNonZero);
        };

        Ok(Self {
            max_model_turns,
            final_output_contract: None,
            structured_output_retry_policy: StructuredOutputRetryPolicy::default(),
        })
    }

    /// Maximum number of model turns this loop may start.
    #[must_use]
    pub fn max_model_turns(&self) -> usize {
        self.max_model_turns.get()
    }

    /// Adds a runtime-owned structured final-output contract.
    #[must_use]
    pub fn with_final_output_contract(mut self, contract: FinalOutputContract) -> Self {
        self.final_output_contract = Some(contract);
        self
    }

    /// Borrows the configured structured final-output contract.
    #[must_use]
    pub fn final_output_contract(&self) -> Option<&FinalOutputContract> {
        self.final_output_contract.as_ref()
    }

    pub(crate) fn merge_context_final_output_contract(
        mut self,
        context_contract: Option<FinalOutputContract>,
    ) -> Result<Self, AgentLoopConfigError> {
        if context_contract.is_some() && self.final_output_contract.is_some() {
            return Err(AgentLoopConfigError::FinalOutputContractConfiguredTwice);
        }
        if let Some(contract) = context_contract {
            self.final_output_contract = Some(contract);
        }
        Ok(self)
    }

    /// Sets retries for an application-level structured-output decoder.
    #[must_use]
    pub fn with_structured_output_retry_policy(
        mut self,
        policy: StructuredOutputRetryPolicy,
    ) -> Self {
        self.structured_output_retry_policy = policy;
        self
    }

    /// Returns the structured-output retry policy.
    #[must_use]
    pub fn structured_output_retry_policy(&self) -> StructuredOutputRetryPolicy {
        self.structured_output_retry_policy
    }
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_model_turns: NonZeroUsize::new(DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS)
                .expect("default agent loop model-turn budget is non-zero"),
            final_output_contract: None,
            structured_output_retry_policy: StructuredOutputRetryPolicy::default(),
        }
    }
}

/// Invalid agent loop configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AgentLoopConfigError {
    /// A loop without a model-turn budget would either do no useful work or
    /// hide a caller configuration mistake.
    #[error("agent loop max_model_turns must be greater than zero")]
    MaxModelTurnsMustBeNonZero,
    /// A single loop received its final-output contract from both public input
    /// paths. Silent precedence would make structured-output behavior depend
    /// on which entry point constructed the loop.
    #[error("agent loop final-output contract was configured more than once")]
    FinalOutputContractConfiguredTwice,
}
