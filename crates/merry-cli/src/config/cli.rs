use super::MerryConfig;
use crate::coding::{ApprovalPolicy, ProcessExecutionMode};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct CliToml {
    sandbox: Option<SandboxModeToml>,
    approval_policy: Option<ApprovalPolicy>,
}

/// Default sandbox mode, named after the root flag it stands in for.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
enum SandboxModeToml {
    #[serde(rename = "with-sandbox")]
    With,
    #[serde(rename = "no-sandbox")]
    No,
    #[serde(rename = "inner-sandbox")]
    Inner,
}

impl From<SandboxModeToml> for ProcessExecutionMode {
    fn from(value: SandboxModeToml) -> Self {
        match value {
            SandboxModeToml::With => Self::OuterAndInner,
            SandboxModeToml::No => Self::Unrestricted,
            SandboxModeToml::Inner => Self::InnerOnly,
        }
    }
}

/// Resolved `[cli]` defaults: the configured sandbox mode and approval
/// policy, each absent when its key is unset.
///
/// The two keys are independent. `sandbox` sets the execution boundary and
/// `approval_policy` sets who reviews permission requests inside it; any
/// approval policy may be configured next to any sandbox mode, exactly as the
/// matching flags may be combined on the command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CliDefaults {
    process_execution_mode: Option<ProcessExecutionMode>,
    approval_policy: Option<ApprovalPolicy>,
}

impl CliDefaults {
    pub(crate) const fn new(
        process_execution_mode: Option<ProcessExecutionMode>,
        approval_policy: Option<ApprovalPolicy>,
    ) -> Self {
        Self {
            process_execution_mode,
            approval_policy,
        }
    }

    /// The configured sandbox mode, when `sandbox` is set.
    pub(crate) const fn process_execution_mode(self) -> Option<ProcessExecutionMode> {
        self.process_execution_mode
    }

    /// The configured reviewer, when `approval_policy` is set.
    pub(crate) const fn approval_policy(self) -> Option<ApprovalPolicy> {
        self.approval_policy
    }
}

impl MerryConfig {
    /// Returns the `[cli]` defaults, empty when the table is absent.
    pub(crate) fn cli_defaults(&self) -> CliDefaults {
        self.raw
            .cli
            .as_ref()
            .map_or_else(CliDefaults::default, |cli| {
                CliDefaults::new(cli.sandbox.map(Into::into), cli.approval_policy)
            })
    }
}
