//! Runtime capability policy.

use std::path::{Path, PathBuf};

/// Filesystem access granted or denied for one configured path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathAccess {
    /// The path may be read but not written by Merry-managed runtime backends.
    ReadOnly,
    /// The path may be read and written by Merry-managed runtime backends.
    ReadWrite,
    /// The path must not be made available to Merry-managed runtime backends.
    Deny,
}

impl PathAccess {
    /// Returns the stable config/API spelling for this access mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "ro",
            Self::ReadWrite => "rw",
            Self::Deny => "deny",
        }
    }
}

/// Trust source for a path access rule.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathAccessRuleSource {
    /// Rule came from the user's global Merry config.
    ///
    /// This is intentionally higher trust than project-local configuration,
    /// because normal coding-agent runs may edit files inside the project.
    TrustedGlobalConfig,
}

/// One platform-neutral path access rule consumed by sandbox backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAccessRule {
    path: PathBuf,
    access: PathAccess,
    source: PathAccessRuleSource,
}

impl PathAccessRule {
    /// Creates a path access rule.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, access: PathAccess, source: PathAccessRuleSource) -> Self {
        Self {
            path: path.into(),
            access,
            source,
        }
    }

    /// Returns the configured path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the requested access.
    #[must_use]
    pub const fn access(&self) -> PathAccess {
        self.access
    }

    /// Returns the trust source for this rule.
    #[must_use]
    pub const fn source(&self) -> PathAccessRuleSource {
        self.source
    }
}

/// Merry-managed low-level capability policy.
///
/// This constrains capabilities owned by Merry-managed runners, such as file
/// and process access lanes. It is not a complete product runtime profile or a
/// trust label for arbitrary host code or in-process tool executors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
}

impl RuntimeCapabilities {
    /// Creates the default capability policy.
    ///
    /// The default denies network and starts with no path grants.
    #[must_use]
    pub fn new() -> Self {
        Self {
            network_allowed: false,
            path_rules: Vec::new(),
        }
    }

    /// Allows Merry-managed network capability for this runtime.
    #[must_use]
    pub fn allow_network(mut self) -> Self {
        self.network_allowed = true;
        self
    }

    /// Denies Merry-managed network capability for this runtime.
    #[must_use]
    pub fn deny_network(mut self) -> Self {
        self.network_allowed = false;
        self
    }

    /// Adds one path access rule to this profile.
    #[must_use]
    pub fn with_path_rule(mut self, rule: PathAccessRule) -> Self {
        self.path_rules.push(rule);
        self
    }

    /// Replaces all path access rules for this profile.
    #[must_use]
    pub fn with_path_rules(mut self, rules: Vec<PathAccessRule>) -> Self {
        self.path_rules = rules;
        self
    }

    /// Returns whether Merry-managed network capability is allowed.
    #[must_use]
    pub fn network_allowed(&self) -> bool {
        self.network_allowed
    }

    /// Returns platform-neutral path access rules for Merry-managed backends.
    #[must_use]
    pub fn path_rules(&self) -> &[PathAccessRule] {
        &self.path_rules
    }
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{PathAccess, PathAccessRule, PathAccessRuleSource, RuntimeCapabilities};
    use std::path::Path;

    #[test]
    fn runtime_capabilities_control_network_without_tool_network_field() {
        let capabilities = RuntimeCapabilities::default().allow_network();

        assert!(capabilities.network_allowed());
    }

    #[test]
    fn runtime_capabilities_deny_network_by_default() {
        let capabilities = RuntimeCapabilities::default();

        assert!(!capabilities.network_allowed());
    }

    #[test]
    fn runtime_capabilities_carry_platform_neutral_path_rules() {
        let rule = PathAccessRule::new(
            "/var/log/foo",
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        );
        let capabilities = RuntimeCapabilities::default().with_path_rule(rule);

        assert_eq!(capabilities.path_rules().len(), 1);
        assert_eq!(
            capabilities.path_rules()[0].path(),
            Path::new("/var/log/foo")
        );
        assert_eq!(capabilities.path_rules()[0].access(), PathAccess::ReadOnly);
        assert_eq!(
            capabilities.path_rules()[0].source(),
            PathAccessRuleSource::TrustedGlobalConfig
        );
    }

    #[test]
    fn path_access_has_stable_config_spelling() {
        assert_eq!(PathAccess::ReadOnly.as_str(), "ro");
        assert_eq!(PathAccess::ReadWrite.as_str(), "rw");
        assert_eq!(PathAccess::Deny.as_str(), "deny");
    }
}
