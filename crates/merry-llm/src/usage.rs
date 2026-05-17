//! Provider-neutral usage accounting.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Token usage reported by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

impl Usage {
    /// Creates token usage from input and output token counts.
    #[must_use]
    pub const fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    /// Input token count.
    #[must_use]
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// Output token count.
    #[must_use]
    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Total token count, or `None` if addition would overflow.
    #[must_use]
    pub const fn total_tokens(&self) -> Option<u64> {
        self.input_tokens.checked_add(self.output_tokens)
    }
}
