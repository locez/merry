//! Runtime admission for provider-visible tools.
//!
//! A tool can remain part of the stable model contract while runtime policy
//! decides whether a particular call is executable. This keeps phase, role,
//! workspace scope, and permission changes out of provider-visible tool
//! schemas and ordering.

use merry_core::ToolName;
use std::collections::BTreeSet;

/// Runtime-owned allowlist for tool execution.
///
/// Admission does not change the advertised tool surface. A denied call is
/// resolved as a structured tool failure by the runtime before input
/// validation or executor dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAdmission {
    allowed_tools: BTreeSet<ToolName>,
}

impl ToolAdmission {
    /// Creates an admission policy that allows exactly the supplied names.
    #[must_use]
    pub fn allow_only<I>(tools: I) -> Self
    where
        I: IntoIterator<Item = ToolName>,
    {
        Self {
            allowed_tools: tools.into_iter().collect(),
        }
    }

    /// Returns whether a tool call is admitted for execution.
    #[must_use]
    pub fn allows(&self, tool_name: &ToolName) -> bool {
        self.allowed_tools.contains(tool_name)
    }

    /// Returns the admitted tool names in deterministic order.
    #[must_use]
    pub fn allowed_tools(&self) -> &BTreeSet<ToolName> {
        &self.allowed_tools
    }
}
