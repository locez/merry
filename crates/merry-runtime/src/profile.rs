//! Runtime capability policy.

/// Merry-managed runtime capability policy.
///
/// This constrains capabilities owned by Merry-managed runners, such as future
/// file/process access lanes and provider-side network use. It is not a trust
/// label for arbitrary host code or in-process tool executors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProfile {
    network_allowed: bool,
}

impl RuntimeProfile {
    /// Creates the default capability policy.
    ///
    /// The default denies network and does not authorize bridge tools.
    #[must_use]
    pub fn new() -> Self {
        Self {
            network_allowed: false,
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

    /// Returns whether Merry-managed network capability is allowed.
    #[must_use]
    pub fn network_allowed(&self) -> bool {
        self.network_allowed
    }

    /// Returns whether this profile authorizes bridge tools.
    ///
    /// Bridge execution is host code outside Merry's sandbox. It is authorized
    /// only through an explicit runtime-builder opt-in, not by profile naming.
    #[must_use]
    pub fn bridge_tools_allowed(&self) -> bool {
        false
    }
}

impl Default for RuntimeProfile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeProfile;

    #[test]
    fn runtime_profile_controls_network_without_tool_network_field() {
        let profile = RuntimeProfile::default().allow_network();

        assert!(profile.network_allowed());
    }

    #[test]
    fn runtime_profile_denies_network_by_default() {
        let profile = RuntimeProfile::default();

        assert!(!profile.network_allowed());
    }

    #[test]
    fn runtime_profile_does_not_authorize_bridge_tools() {
        let profile = RuntimeProfile::default().allow_network();

        assert!(!profile.bridge_tools_allowed());
    }
}
