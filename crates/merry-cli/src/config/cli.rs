use super::MerryConfig;
use crate::coding::{AutoFallback, PermissionMode, SandboxMode};
use crate::config::ConfigError;
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct CliToml {
    sandbox: Option<SandboxMode>,
    permission_mode: Option<PermissionMode>,
    auto_fallback: Option<AutoFallback>,
}

/// Resolved `[cli]` defaults. Each field is absent when its key is unset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CliDefaults {
    sandbox: Option<SandboxMode>,
    permission_mode: Option<PermissionMode>,
    auto_fallback: Option<AutoFallback>,
}

impl CliDefaults {
    pub(crate) const fn new(
        sandbox: Option<SandboxMode>,
        permission_mode: Option<PermissionMode>,
        auto_fallback: Option<AutoFallback>,
    ) -> Self {
        Self {
            sandbox,
            permission_mode,
            auto_fallback,
        }
    }

    pub(crate) const fn sandbox(self) -> Option<SandboxMode> {
        self.sandbox
    }

    pub(crate) const fn permission_mode(self) -> Option<PermissionMode> {
        self.permission_mode
    }

    pub(crate) const fn auto_fallback(self) -> Option<AutoFallback> {
        self.auto_fallback
    }
}

impl MerryConfig {
    /// Returns the `[cli]` defaults, empty when the table is absent.
    pub(crate) fn cli_defaults(&self) -> Result<CliDefaults, ConfigError> {
        let Some(cli) = self.raw.cli.as_ref() else {
            return Ok(CliDefaults::default());
        };
        if cli.auto_fallback.is_some()
            && matches!(
                cli.permission_mode,
                Some(mode) if mode != PermissionMode::Auto
            )
        {
            return Err(ConfigError::Invalid(
                "auto_fallback is only valid when permission_mode is \"auto\"".to_owned(),
            ));
        }
        Ok(CliDefaults::new(
            cli.sandbox,
            cli.permission_mode,
            cli.auto_fallback,
        ))
    }
}
