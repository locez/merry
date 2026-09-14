use super::{ConfigError, MerryConfig};
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

impl SandboxModeToml {
    /// The config value, as written in `config.toml`.
    const fn name(self) -> &'static str {
        match self {
            Self::With => "with-sandbox",
            Self::No => "no-sandbox",
            Self::Inner => "inner-sandbox",
        }
    }
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
/// Trusted execution has no sandbox, so `approval_policy` is only ever
/// [`ApprovalPolicy::Trusted`] together with
/// [`ProcessExecutionMode::Unrestricted`]; see [`MerryConfig::cli_defaults`].
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
    ///
    /// `approval_policy = "trusted"` means running without any sandbox, so
    /// it is rejected unless `sandbox = "no-sandbox"` is set alongside it.
    pub(crate) fn cli_defaults(&self) -> Result<CliDefaults, ConfigError> {
        let Some(cli) = self.raw.cli.as_ref() else {
            return Ok(CliDefaults::default());
        };
        if cli.approval_policy == Some(ApprovalPolicy::Trusted)
            && cli.sandbox != Some(SandboxModeToml::No)
        {
            let found = match cli.sandbox {
                Some(sandbox) => format!("sandbox = \"{}\"", sandbox.name()),
                None => "sandbox is not set".to_owned(),
            };
            return Err(ConfigError::Invalid(format!(
                "[cli] approval_policy = \"{}\" runs without any sandbox and requires \
                 sandbox = \"no-sandbox\", but {found}",
                ApprovalPolicy::Trusted.name()
            )));
        }
        Ok(CliDefaults::new(
            cli.sandbox.map(Into::into),
            cli.approval_policy,
        ))
    }
}
