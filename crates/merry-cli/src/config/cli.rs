use super::MerryConfig;
use crate::coding::ProcessExecutionMode;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct CliToml {
    sandbox: Option<SandboxModeToml>,
    fully_trusted: Option<bool>,
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

/// Resolved `[cli]` defaults: the configured sandbox mode, if any, and
/// whether fully trusted review is on by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CliDefaults {
    process_execution_mode: Option<ProcessExecutionMode>,
    fully_trusted: bool,
}

impl CliDefaults {
    pub(crate) const fn new(
        process_execution_mode: Option<ProcessExecutionMode>,
        fully_trusted: bool,
    ) -> Self {
        Self {
            process_execution_mode,
            fully_trusted,
        }
    }

    /// The configured sandbox mode, when `sandbox` is set.
    pub(crate) const fn process_execution_mode(self) -> Option<ProcessExecutionMode> {
        self.process_execution_mode
    }

    /// Whether `fully_trusted = true` is set.
    pub(crate) const fn fully_trusted(self) -> bool {
        self.fully_trusted
    }
}

impl MerryConfig {
    /// Returns the `[cli]` defaults, empty when the table is absent.
    pub(crate) fn cli_defaults(&self) -> CliDefaults {
        let Some(cli) = self.raw.cli.as_ref() else {
            return CliDefaults::default();
        };
        CliDefaults::new(
            cli.sandbox.map(Into::into),
            cli.fully_trusted.unwrap_or(false),
        )
    }
}
